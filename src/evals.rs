//! Evaluation engine: runs the golden cases, produces the scorecard, compares to baseline. It only scores.
use crate::runtime::{self, Invoker};
use crate::store::{self, params};
use crate::util::{head, now, read, R};
use crate::router;
use serde_json::{json, Value};
use std::path::Path;

pub const NOISE: f64 = 0.05; // a success-rate change smaller than this is treated as run-to-run noise

/// `evals/golden.jsonl` plus any `evals/issue_*.jsonl` reproductions, in a stable order.
pub fn cases(at: &Path, app: &str) -> R<Vec<Value>> {
    let dir = at.join("apps").join(app).join("evals");
    let mut files = vec![dir.join("golden.jsonl")];
    let mut issues: Vec<_> = std::fs::read_dir(&dir).into_iter().flatten().flatten().map(|e| e.path())
        .filter(|p| p.file_name().and_then(|n| n.to_str()).is_some_and(|n| n.starts_with("issue_") && n.ends_with(".jsonl"))).collect();
    issues.sort();
    files.extend(issues);
    let mut out = Vec::new();
    for file in files {
        for line in read(&file)?.lines().filter(|l| !l.trim().is_empty()) {
            out.push(serde_json::from_str(line).map_err(|e| format!("{}: a golden case is not valid JSON: {e}", file.display()))?);
        }
    }
    if out.is_empty() {
        return Err(format!("apps/{app}/evals/golden.jsonl has no cases"));
    }
    Ok(out)
}

fn list(v: &Value) -> Vec<String> {
    v.as_array().map(|a| a.iter().filter_map(|s| s.as_str().map(|s| s.to_lowercase())).collect()).unwrap_or_default()
}

pub fn run(app: &str, at: &Path, inv: &mut dyn Invoker) -> R<Value> {
    let mut rows = Vec::new();
    for case in cases(at, app)? {
        let r = runtime::handle(app, &case["input"], at, "eval", false, inv);
        let out = &r["output"];
        let text = out.as_object().map(|o| o.values().map(|v| v.as_str().map(String::from).unwrap_or_else(|| v.to_string())).collect::<Vec<_>>().join(" ")).unwrap_or_default();
        let low = text.to_lowercase();
        let mut passed = r["ok"] == true
            && (case.get("expect").is_none() || out == &case["expect"])
            && case["answer"].as_str().map_or(true, |a| text.trim().to_lowercase() == a.trim().to_lowercase())
            && list(&case["must_include"]).iter().all(|s| low.contains(s))
            && !list(&case["must_not_include"]).iter().any(|s| low.contains(s))
            && text.chars().count() as u64 <= case["max_chars"].as_u64().unwrap_or(u64::MAX);
        if passed {
            if let Some(rubric) = case["rubric"].as_str() {
                let prompt = runtime::render(at, "forge_judge", &json!({"input": case["input"].to_string(), "answer": text, "rubric": rubric}))?;
                let verdict = router::call("judge", &json!([{"role": "user", "content": prompt}]), at, "", false)?;
                passed = verdict.trim().to_uppercase().starts_with("PASS");
            }
        }
        let got = if text.is_empty() { r["error"].as_str().unwrap_or("").to_string() } else { head(&text, 500) };
        rows.push(json!({"id": case["id"], "pass": passed, "ms": r["ms"], "usd": r["usd"], "got": got, "case": case}));
    }
    let n = rows.len() as f64;
    let mut ms: Vec<i64> = rows.iter().map(|r| r["ms"].as_i64().unwrap_or(0)).collect();
    ms.sort();
    Ok(json!({
        "n": rows.len(),
        "success": rows.iter().filter(|r| r["pass"] == true).count() as f64 / n,
        "p95_ms": ms[(ms.len() - 1).min((0.95 * n) as usize)],
        "usd_per_req": rows.iter().map(|r| r["usd"].as_f64().unwrap_or(0.0)).sum::<f64>() / n,
        "failed": rows.iter().filter(|r| r["pass"] != true).map(|r| json!({"id": r["id"], "got": r["got"], "case": r["case"]})).collect::<Vec<_>>(),
    }))
}

/// Score the app as built on this computer (released code, or a copy that differs only in prompts or routing).
pub fn run_host(app: &str, at: &Path) -> R<Value> {
    let bin = runtime::build_app(at, app)?;
    run(app, at, &mut runtime::Host(bin))
}

/// The reasons the candidate fails the gate; an empty list means it passes.
pub fn compare(base: &Value, cand: &Value, must_improve: bool, limit_ms: i64) -> Vec<String> {
    let f = |v: &Value, k: &str| v[k].as_f64().unwrap_or(0.0);
    let mut why = Vec::new();
    if f(cand, "success") < f(base, "success") - 1e-9 {
        why.push(format!("success rate fell from {:.0}% to {:.0}%", f(base, "success") * 100.0, f(cand, "success") * 100.0));
    }
    if f(cand, "p95_ms") > limit_ms as f64 {
        why.push(format!("slowest request took {} ms, over the {limit_ms} ms limit", f(cand, "p95_ms")));
    }
    if f(cand, "usd_per_req") > f(base, "usd_per_req") * 1.2 + 1e-9 {
        why.push("cost per request rose by more than 20%".to_string());
    }
    let better = f(cand, "success") > f(base, "success") + NOISE
        || f(cand, "usd_per_req") < f(base, "usd_per_req") * 0.9
        || f(cand, "p95_ms") < f(base, "p95_ms") * 0.7;
    if must_improve && why.is_empty() && !better {
        why.push("no measurable improvement over the current version".to_string());
    }
    why
}

pub fn save(app: &str, version: &str, card: &Value) {
    let _ = store::x("insert into scorecards(ts,app,version,card) values(?,?,?,?)", params![now(), app, version, card.to_string()]);
}

pub fn latest(app: &str) -> Option<Value> {
    let rows = store::q("select card from scorecards where app=? and version='stable' order by id desc limit 1", params![app]).ok()?;
    serde_json::from_str(&store::rs(rows.first()?, "card")).ok()
}
