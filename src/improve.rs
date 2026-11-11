//! Improvement plane: detect -> triage -> propose on a branch -> gate. Also the gate for changes made by a
//! connected agent. Its only output is a Git branch waiting for a decision; it never changes main.
use crate::store::{self, params, ri, rs, Row};
use crate::util::{git, hash, head, now, read, root, tail, today, write, Sx, R};
use crate::{evals, release, router, runtime, sandbox};
use regex::Regex;
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

static CYCLE: Mutex<()> = Mutex::new(());

pub fn worktree(pid: i64) -> PathBuf {
    root().join(".forge").join("wt").join(format!("p{pid}"))
}

fn weight(kind: &str) -> i64 {
    match kind {
        "error" => 3,
        "eval" => 2,
        _ => 1,
    }
}

// ---------- detect and de-duplicate ----------

/// One issue per fingerprint. Fixed issues reopen; rejected ones reopen only once occurrences double.
pub fn upsert(fp: &str, kind: &str, app: &str, title: &str, detail: &str, n: i64) -> R<()> {
    let t = now();
    store::x("insert into issues(fingerprint,kind,app,title,detail,count,first_ts,last_ts,status)
              values(?,?,?,?,?,?,?,?,?)
              on conflict(fingerprint) do update set count=count+excluded.count, last_ts=excluded.last_ts,
                detail=excluded.detail,
                status=case when status='fixed' or (status='rejected' and count+excluded.count>=2*seen_count)
                            then 'open' else status end",
        params![fp, kind, app, head(title, 200), head(detail, 4000), n, t, t, if kind == "transient" { "closed" } else { "open" }]).map(|_| ())
}

pub fn detect() -> R<()> {
    let (since, t) = (store::kv_get("detect_ts").as_f64().unwrap_or(0.0), now());
    for r in store::q("select fingerprint, app, count(*) n, max(error) e from traces
                       where ts>? and fingerprint is not null and version!='eval' group by fingerprint, app", params![since])? {
        let fp = rs(&r, "fingerprint");
        let kind = if fp.starts_with("RouterError") { "transient" } else { "error" }; // a model outage needs no code fix
        upsert(&fp, kind, &rs(&r, "app"), &fp, &rs(&r, "e"), ri(&r, "n"))?;
    }
    for f in store::q("select f.id, f.text, f.rating, t.app, t.input, t.output from feedback f
                       join traces t on t.id=f.trace_id where f.seen=0", params![])? {
        let text = rs(&f, "text").trim().to_string();
        if ri(&f, "rating") < 0 {
            let words: String = text.to_lowercase().split(|c: char| !c.is_alphanumeric()).filter(|w| !w.is_empty()).collect::<Vec<_>>().join(" ");
            let fp = if words.is_empty() { format!("feedback:thumbsdown:{}", rs(&f, "app")) } else { format!("feedback:{}", &hash(&words)[..10]) };
            let detail = json!({"user_comment": text, "input": rs(&f, "input"), "output": rs(&f, "output")}).to_string();
            upsert(&fp, "feedback", &rs(&f, "app"), if text.is_empty() { "Answers were rated 'not good'" } else { &text }, &detail, 1)?;
        }
        store::x("update feedback set seen=1 where id=?", params![ri(&f, "id")])?;
    }
    store::kv_set("detect_ts", &json!(t))
}

pub fn eval_check(app: &str) -> R<Value> {
    let card = evals::run_host(app, &root())?;
    evals::save(app, "stable", &card);
    let failing: Vec<String> = card["failed"].as_array().into_iter().flatten().map(|f| format!("eval:{app}:{}", f["id"].as_str().unwrap_or(""))).collect();
    for r in store::q("select id, fingerprint from issues where kind='eval' and app=? and status='open'", params![app])? {
        if !failing.contains(&rs(&r, "fingerprint")) {
            // the case passes again: nothing left to fix
            store::x("update issues set status='fixed' where id=?", params![ri(&r, "id")])?;
        }
    }
    for f in card["failed"].as_array().into_iter().flatten() {
        let id = f["id"].as_str().unwrap_or("");
        upsert(&format!("eval:{app}:{id}"), "eval", app, &format!("Golden case '{id}' fails"), &json!({"case": f["case"], "got": f["got"]}).to_string(), 1)?;
    }
    Ok(card)
}

/// Open issues ranked by frequency x severity; one per app; apps with a change in flight wait.
pub fn triage(limit: i64) -> R<Vec<Row>> {
    let busy: Vec<String> = store::q("select app from proposals where status in ('testing','awaiting','canary')", params![])?.iter().map(|r| rs(r, "app")).collect();
    let mut rows: Vec<Row> = store::q("select * from issues where status='open' and kind!='transient'", params![])?
        .into_iter().filter(|r| !busy.contains(&rs(r, "app"))).collect();
    rows.sort_by_key(|r| -ri(r, "count") * weight(&rs(r, "kind")));
    let mut picked: Vec<Row> = Vec::new();
    for r in rows {
        if !picked.iter().any(|p| rs(p, "app") == rs(&r, "app")) {
            picked.push(r);
        }
    }
    picked.truncate(limit.max(0) as usize);
    Ok(picked)
}

// ---------- shared pieces of a proposal ----------

/// Allow-list per channel for Forge's own fixer. Golden cases, limits, config and Forge itself are never writable.
pub fn allowed(channel: &str, app: &str, paths: &[String], issue_id: i64) -> Option<String> {
    for p in paths {
        let ok = match channel {
            "prompt" => Regex::new(&format!(r"^prompts/{app}_[\w-]+\.md$")).expect("valid pattern").is_match(p),
            "routing" => p == "config/routing.toml",
            _ => p == &format!("apps/{app}/src/main.rs") || p == &format!("apps/{app}/evals/issue_{issue_id}.jsonl"),
        };
        if !ok {
            return Some(format!("changing {p} is not allowed in a {channel} proposal"));
        }
    }
    None
}

/// Reads the model's reply: a CAUSE line, then FILE blocks.
pub fn parse(reply: &str) -> R<(String, Vec<(String, String)>)> {
    let reply = reply.replace('\r', "");
    let cause = Regex::new(r"(?m)^CAUSE:\s*(.+)$").expect("valid pattern").captures(&reply).map(|c| c[1].trim().to_string());
    let files: Vec<(String, String)> = Regex::new(r"(?ms)^FILE:\s*(\S+)\s*\n<<<\n(.*?)\n>>>").expect("valid pattern").captures_iter(&reply)
        .map(|c| (c[1].replace('\\', "/"), format!("{}\n", &c[2]))).collect();
    match cause {
        Some(cause) if !files.is_empty() => Ok((cause, files)),
        _ => Err("the reply did not follow the required CAUSE / FILE format".to_string()),
    }
}

pub fn slots(text: &str) -> Vec<String> {
    let mut found: Vec<String> = Regex::new(r"\{\{\w+\}\}").expect("valid pattern").find_iter(text).map(|m| m.as_str().to_string()).collect();
    found.sort();
    found.dedup();
    found
}

/// Apps run on this computer once released, so their source may not reach for files, the network,
/// other programs, or anything that sidesteps Rust's safety.
/// shortcut: a text check, not a proof; the sandbox, scanners and the owner's review back it up.
pub fn forbidden(source: &str) -> Option<String> {
    let rules = [
        (r"\bunsafe\b", "unsafe code"),
        (r"\bstd\s*::\s*(fs|net|process|env|os)\b", "files, network, other programs or the environment (std::fs, std::net, std::process, std::env, std::os)"),
        (r"\bstd\s*::\s*\{[^}]*\b(fs|net|process|env|os)\b", "files, network, other programs or the environment"),
        (r"\binclude(_str|_bytes)?\s*!", "include! macros"),
        (r"\bextern\b", "extern declarations"),
        (r"#!\s*\[", "crate-level attributes"),
    ];
    for (pattern, what) in rules {
        if Regex::new(pattern).expect("valid pattern").is_match(source) {
            return Some(format!("apps may not use {what}"));
        }
    }
    None
}

/// Forge writes every app's Cargo.toml itself, so an app can never add a dependency or a build script.
pub fn cargo_toml(app: &str) -> String {
    format!("[package]\nname = \"{app}\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n[dependencies]\nforge-app = {{ path = \"../../sdk\" }}\n\n[workspace]\n")
}

pub fn open_proposal(app: &str, channel: &str, issue_id: Option<i64>) -> R<(i64, PathBuf)> {
    let pid = store::x("insert into proposals(ts,issue_id,app,channel,status) values(?,?,?,?,'testing')", params![now(), issue_id, app, channel])?;
    let wt = worktree(pid);
    let branch = format!("proposal/{pid}");
    if let Err(e) = git(&["worktree", "add", "-b", &branch, &wt.to_string_lossy(), "main"], &root()) {
        let _ = store::x("delete from proposals where id=?", params![pid]);
        return Err(e);
    }
    store::x("update proposals set branch=? where id=?", params![branch, pid])?;
    Ok((pid, wt))
}

pub fn finish(pid: i64, wt: &Path, summary: &str, gate: &Value) -> R<()> {
    git(&["add", "-A"], wt)?;
    git(&["commit", "-m", &format!("proposal {pid}: {summary}")], wt)?;
    let diff = git(&["diff", "main", &format!("proposal/{pid}")], &root())?;
    store::x("update proposals set status='awaiting', summary=?, diff=?, gate=? where id=?",
        params![summary, head(&diff, 60000), gate.to_string(), pid]).map(|_| ())
}

fn fail(mut gate: Value, why: impl Into<String>) -> Value {
    gate["ok"] = json!(false);
    gate["why"] = json!(why.into());
    gate
}

// ---------- Forge's own fixer ----------

/// One try at a fix. Returns the gate result: {"ok": bool, "why": text, ...evidence}.
fn attempt(issue: &Row, channel: &str, wt: &Path, base: &Value, failure: &str) -> R<Value> {
    let (app, iid) = (rs(issue, "app"), ri(issue, "id"));
    let limit_ms = router::config(&root())?["gates"]["p95_ms"].as_i64().unwrap_or(180_000);
    let repro = format!("apps/{app}/evals/issue_{iid}.jsonl");
    let (targets, rules) = if channel == "code" {
        (vec![format!("apps/{app}/src/main.rs")],
         format!("Change only apps/{app}/src/main.rs. It must keep using the forge_app crate and nothing else: no unsafe, \
                  no std::fs, std::net, std::process or std::env.\nAlso write a new file {repro} holding ONE line of JSON: \
                  a golden case that fails on the current code and passes with your fix, for example \
                  {{\"id\": \"x\", \"input\": {{\"field\": \"value\"}}, \"answer\": \"expected answer\"}}."))
    } else {
        let mut files: Vec<String> = std::fs::read_dir(root().join("prompts")).into_iter().flatten().flatten()
            .filter_map(|e| e.file_name().into_string().ok()).filter(|n| n.starts_with(&format!("{app}_")) && n.ends_with(".md"))
            .map(|n| format!("prompts/{n}")).collect();
        files.sort();
        (files, "Change only the prompt files shown. Keep every {{placeholder}} exactly as it is and add none.".to_string())
    };
    let mut files = String::new();
    for t in &targets {
        files.push_str(&format!("FILE: {t}\n<<<\n{}\n>>>\n\n", read(&wt.join(t))?));
    }
    let memory: Vec<String> = store::recall(&format!("{} {}", rs(issue, "title"), rs(issue, "detail")), 3).iter()
        .map(|m| format!("- [{}] {}", rs(m, "kind"), rs(m, "text"))).collect();
    let prompt = runtime::render(&root(), "forge_fix", &json!({
        "kind": rs(issue, "kind"), "count": ri(issue, "count"), "title": rs(issue, "title"), "detail": rs(issue, "detail"),
        "memory": if memory.is_empty() { "nothing yet".to_string() } else { memory.join("\n") }, "files": files, "rules": rules,
        "failure": if failure.is_empty() { String::new() } else { format!("Your previous attempt was rejected: {failure}") }}))?;
    let (cause, new) = parse(&router::call("develop", &json!([{"role": "user", "content": prompt}]), &root(), "", false)?)?;
    let gate = json!({"cause": cause});
    let paths: Vec<String> = new.iter().map(|(p, _)| p.clone()).collect();
    if let Some(err) = allowed(channel, &app, &paths, iid) {
        return Ok(fail(gate, err));
    }
    let content = |path: &str| new.iter().find(|(p, _)| p == path).map(|(_, t)| t.clone());
    let mut gate = gate;
    let card = if channel == "code" {
        let (Some(source), Some(case_text), 2) = (content(&targets[0]), content(&repro), new.len()) else {
            return Ok(fail(gate, format!("the reply must contain exactly {} and {repro}", targets[0])));
        };
        if let Some(why) = forbidden(&source) {
            return Ok(fail(gate, why));
        }
        let Ok(mut case) = serde_json::from_str::<Value>(case_text.trim()) else {
            return Ok(fail(gate, format!("{repro} must be one line of JSON")));
        };
        let case_id = format!("issue-{iid}");
        case["id"] = json!(case_id);
        write(wt, &repro, &format!("{case}\n"))?;
        let before = sandbox::evals(wt, &app)?;
        if !before["failed"].as_array().into_iter().flatten().any(|f| f["id"] == json!(case_id)) {
            return Ok(fail(gate, "the new golden case passes on the current code, so it does not reproduce the problem"));
        }
        write(wt, &targets[0], &source)?;
        let (ok, out) = sandbox::tests(wt, &app);
        gate["tests"] = json!(out);
        if !ok {
            return Ok(fail(gate, format!("unit tests failed:\n{}", tail(&out, 1500))));
        }
        let (ok, out) = sandbox::scans(wt);
        gate["scans"] = json!(out);
        if !ok {
            return Ok(fail(gate, format!("a security scan failed:\n{}", tail(&out, 1500))));
        }
        sandbox::evals(wt, &app)?
    } else {
        for (path, text) in &new {
            let old = read(&wt.join(path)).map_err(|_| format!("{path} must exist already"))?;
            if slots(&old) != slots(text) {
                return Ok(fail(gate, format!("{path} must keep the same placeholders")));
            }
            write(wt, path, text)?;
        }
        evals::run_host(&app, wt)? // only prompt text differs from main, so no unapproved code runs here
    };
    gate["before"] = base.clone();
    gate["after"] = card.clone();
    let why = evals::compare(base, &card, rs(issue, "kind") == "eval", limit_ms);
    gate["ok"] = json!(why.is_empty());
    gate["why"] = json!(why.join("; "));
    Ok(gate)
}

pub fn propose(issue: &Row, base: &Value) -> R<i64> {
    let channel = if rs(issue, "kind") == "error" { "code" } else { "prompt" };
    let (pid, wt) = open_proposal(&rs(issue, "app"), channel, Some(ri(issue, "id")))?;
    store::x("update issues set status='proposed' where id=?", params![ri(issue, "id")])?;
    let (mut failure, mut gate) = (String::new(), json!({}));
    let limit = router::config(&root())?["budget"]["max_attempts"].as_i64().unwrap_or(3);
    for n in 1..=limit {
        // bounded: then a human decides
        if store::kv_get("kill") == json!(true) {
            failure = "stopped by the kill switch".to_string();
            break;
        }
        store::x("update proposals set attempts=? where id=?", params![n, pid])?;
        gate = attempt(issue, channel, &wt, base, &failure).unwrap_or_else(|e| json!({"ok": false, "why": e}));
        if gate["ok"] == true {
            finish(pid, &wt, gate["cause"].as_str().unwrap_or("fix"), &gate)?;
            return Ok(pid);
        }
        failure = gate["why"].as_str().unwrap_or("").to_string();
        let _ = git(&["checkout", "--", "."], &wt);
        let _ = git(&["clean", "-fd"], &wt);
    }
    let title = rs(issue, "title");
    store::x("update proposals set summary=?, gate=? where id=?", params![format!("Could not fix: {title}"), gate.to_string(), pid])?;
    let p = store::q("select * from proposals where id=?", params![pid])?.remove(0);
    release::close(&p, "escalated", "hold", &format!("Gave up after {} attempts: {failure}", ri(&p, "attempts")));
    release::cleanup(pid);
    Ok(pid)
}

/// Model-selection channel: try each model first for this app's tasks, keep the best that beats baseline.
pub fn tune_routing(app: &str, base: &Value) -> R<Option<i64>> {
    let cfg = router::config(&root())?;
    let tasks: Vec<String> = cfg["apps"][app]["tasks"].as_array().into_iter().flatten().filter_map(|t| t.as_str().map(String::from)).collect();
    if tasks.is_empty() {
        return Ok(None); // the app uses no model, so there is no model order to tune
    }
    let text = read(&root().join("config").join("routing.toml"))?;
    let golden = read(&root().join("apps").join(app).join("evals").join("golden.jsonl"))?;
    let key = json!(hash(&format!("{text}{golden}")));
    if store::kv_get(&format!("tuned:{app}")) == key {
        return Ok(None); // nothing changed since the last search: do not repeat it
    }
    store::kv_set(&format!("tuned:{app}"), &key)?;
    let (pid, wt) = open_proposal(app, "routing", None)?;
    let mut best: Option<((f64, f64, f64), String, String, Value)> = None;
    for (name, m) in cfg["models"].as_object().cloned().unwrap_or_default() {
        let ep = &cfg["endpoints"][m["endpoint"].as_str().unwrap_or("")];
        let no_key = ep["key_env"].as_str().is_some_and(|v| std::env::var(v).is_err());
        if no_key || tasks.iter().all(|t| cfg["tasks"][t.as_str()][0] == json!(name)) {
            continue;
        }
        let mut cand = text.clone();
        for t in &tasks {
            let mut chain = vec![name.clone()];
            chain.extend(cfg["tasks"][t.as_str()].as_array().into_iter().flatten().filter_map(|n| n.as_str().map(String::from)).filter(|n| n != &name));
            let line = Regex::new(&format!(r"(?m)^({}\s*=\s*)\[[^\]]*\]", regex::escape(t))).expect("valid pattern");
            cand = line.replace(&cand, |c: &regex::Captures| format!("{}{}", &c[1], json!(chain))).into_owned();
        }
        write(&wt, "config/routing.toml", &cand)?;
        let card = evals::run_host(app, &wt)?;
        let f = |k: &str| card[k].as_f64().unwrap_or(0.0);
        let rank = (f("success"), -f("usd_per_req"), -f("p95_ms"));
        let passes = evals::compare(base, &card, true, cfg["gates"]["p95_ms"].as_i64().unwrap_or(180_000)).is_empty();
        if passes && best.as_ref().map_or(true, |b| rank > b.0) {
            best = Some((rank, name, cand, card));
        }
    }
    let Some((_, name, cand, card)) = best else {
        store::x("delete from proposals where id=?", params![pid])?;
        release::cleanup(pid);
        store::remember("routing", app, "Tried every configured model first; none beat the current order.");
        return Ok(None);
    };
    write(&wt, "config/routing.toml", &cand)?;
    let summary = format!("Use {name} first for {}", tasks.join(", "));
    finish(pid, &wt, &summary, &json!({"cause": format!("{summary} (better score, cost or speed on the golden cases)"), "before": base, "after": card}))?;
    Ok(Some(pid))
}

// ---------- changes made by a builder or a connected agent ----------

pub fn start_change(app: &str, issue_id: Option<i64>) -> R<(i64, PathBuf)> {
    if !runtime::valid_name(app) {
        return Err("App names use lowercase letters, digits and underscores, starting with a letter (2 to 31 characters).".to_string());
    }
    if store::kv_get("kill") == json!(true) {
        return Err("The owner has stopped all changes (kill switch is on).".to_string());
    }
    if let Some(busy) = store::q("select * from proposals where app=? and status in ('testing','awaiting','canary')", params![app])?.first() {
        if rs(busy, "status") == "testing" && rs(busy, "channel") == "agent" {
            return Ok((ri(busy, "id"), worktree(ri(busy, "id")))); // resume the open change
        }
        return Err(format!("Change {} for {app} is still {}. Wait for the owner's decision.", ri(busy, "id"), rs(busy, "status")));
    }
    // no daily cap here: a person asked for this change. It is still bounded by one change per app and limited submits.
    let (pid, wt) = open_proposal(app, "agent", issue_id)?;
    write(&wt, &format!("apps/{app}/Cargo.toml"), &cargo_toml(app))?;
    if let Some(id) = issue_id {
        store::x("update issues set status='proposed' where id=? and app=?", params![id, app])?;
    }
    Ok((pid, wt))
}

pub fn agent_change(pid: i64) -> R<(Row, PathBuf)> {
    let mut rows = store::q("select * from proposals where id=? and channel='agent' and status='testing'", params![pid])?;
    if rows.is_empty() {
        return Err(format!("Change {pid} is not open for editing."));
    }
    Ok((rows.remove(0), worktree(pid)))
}

/// A builder may touch its own app's source, manifest, golden cases and prompts. Nothing else, ever.
pub fn agent_path(app: &str, rel: &str) -> R<String> {
    let rel = rel.replace('\\', "/");
    let patterns = [format!(r"^apps/{app}/src/[\w/]+\.rs$"), format!(r"^apps/{app}/evals/golden\.jsonl$"),
                    format!(r"^apps/{app}/app\.json$"), format!(r"^prompts/{app}_[\w-]+\.md$")];
    if rel.contains("..") || !patterns.iter().any(|p| Regex::new(p).expect("valid pattern").is_match(&rel)) {
        return Err(format!("{rel} is not allowed. Only apps/{app}/src/*.rs, apps/{app}/app.json, apps/{app}/evals/golden.jsonl and prompts/{app}_*.md can be changed."));
    }
    Ok(rel)
}

fn agent_gate(p: &Row, wt: &Path) -> R<Value> {
    let (app, pid) = (rs(p, "app"), ri(p, "id"));
    let dir = wt.join("apps").join(&app);
    write(wt, &format!("apps/{app}/Cargo.toml"), &cargo_toml(&app))?; // always Forge's own, whatever is there
    git(&["add", "-A"], wt)?;
    let paths: Vec<String> = git(&["diff", "--cached", "--name-only", "main"], wt)?.lines().map(String::from).collect();
    let cargo = format!("apps/{app}/Cargo.toml");
    if paths.iter().all(|p| p == &cargo) {
        return Ok(fail(json!({}), "nothing was changed"));
    }
    for path in paths.iter().filter(|p| *p != &cargo) {
        if let Err(why) = agent_path(&app, path) {
            return Ok(fail(json!({}), why));
        }
    }
    for need in ["src/main.rs", "app.json", "evals/golden.jsonl"] {
        if !dir.join(need).is_file() {
            return Ok(fail(json!({}), format!("apps/{app}/{need} is missing")));
        }
    }
    if !runtime::manifest(wt, &app)["fields"].as_array().is_some_and(|f| !f.is_empty() && f.iter().all(|x| x.as_array().is_some_and(|x| x.len() == 3))) {
        return Ok(fail(json!({}), format!("apps/{app}/app.json needs \"fields\": a list of [name, label, \"text\" or \"textarea\"]")));
    }
    let mut has_test = false;
    for entry in std::fs::read_dir(dir.join("src")).s()?.flatten() {
        let source = read(&entry.path())?;
        if let Some(why) = forbidden(&source) {
            return Ok(fail(json!({}), format!("{}: {why}", entry.file_name().to_string_lossy())));
        }
        has_test |= source.contains("#[test]");
    }
    if !has_test {
        return Ok(fail(json!({}), "the app needs at least one unit test (#[test])"));
    }
    let new_app = !root().join("apps").join(&app).join("src").join("main.rs").exists();
    let mut gate = json!({"cause": "Built by a connected agent"});
    let touched: Vec<&String> = paths.iter().filter(|f| f.starts_with(&format!("apps/{app}/evals/")) && root().join(f).exists()).collect();
    if !touched.is_empty() {
        gate["warning"] = json!(format!("This change edits existing golden cases: {}", touched.iter().map(|s| s.as_str()).collect::<Vec<_>>().join(", ")));
    }
    store::stage(pid, "tests", None);
    let (ok, out) = sandbox::tests(wt, &app);
    store::stage(pid, "tests", Some(ok));
    gate["tests"] = json!(out);
    if !ok {
        return Ok(fail(gate, format!("unit tests failed:\n{}", tail(&out, 1500))));
    }
    store::stage(pid, "scans", None);
    let (ok, out) = sandbox::scans(wt);
    store::stage(pid, "scans", Some(ok));
    gate["scans"] = json!(out);
    if !ok {
        return Ok(fail(gate, format!("a security scan failed:\n{}", tail(&out, 1500))));
    }
    store::stage(pid, "examples", None);
    let card = match sandbox::evals(wt, &app) {
        Ok(card) => card,
        Err(e) => {
            store::stage(pid, "examples", Some(false));
            return Ok(fail(gate, e));
        }
    };
    gate["after"] = card.clone();
    let limit_ms = router::config(&root())?["gates"]["p95_ms"].as_i64().unwrap_or(180_000);
    let why = if new_app {
        // no baseline yet: the app's own golden cases define "done"
        let mut why = Vec::new();
        let failed: Vec<&str> = card["failed"].as_array().into_iter().flatten().filter_map(|f| f["id"].as_str()).collect();
        if !failed.is_empty() {
            why.push(format!("golden cases fail: {}", failed.join(", ")));
        }
        if card["p95_ms"].as_i64().unwrap_or(0) > limit_ms {
            why.push(format!("slowest request took {} ms, over the {limit_ms} ms limit", card["p95_ms"]));
        }
        why
    } else {
        let before = match evals::latest(&app) {
            Some(card) => card,
            None => evals::run_host(&app, &root())?,
        };
        gate["before"] = before.clone();
        evals::compare(&before, &card, false, limit_ms)
    };
    store::stage(pid, "examples", Some(why.is_empty()));
    gate["ok"] = json!(why.is_empty());
    gate["why"] = json!(why.join("; "));
    Ok(gate)
}

pub fn submit_change(pid: i64, summary: &str) -> R<String> {
    let (p, wt) = agent_change(pid)?;
    let (n, limit) = (ri(&p, "attempts") + 1, router::config(&root())?["budget"]["max_attempts"].as_i64().unwrap_or(3));
    store::x("update proposals set attempts=? where id=?", params![n, pid])?;
    let gate = agent_gate(&p, &wt).unwrap_or_else(|e| json!({"ok": false, "why": e}));
    let summary = head(summary.trim(), 200);
    let why = gate["why"].as_str().unwrap_or("").to_string();
    if gate["ok"] == true {
        finish(pid, &wt, if summary.is_empty() { "Change to the app" } else { &summary }, &gate)?;
        return Ok(format!("Change {pid} passed every gate. It now waits for the owner's approval in the dashboard."));
    }
    if n >= limit {
        // bounded: the owner decides what happens next; the work stays in the folder for them
        store::x("update proposals set summary=?, gate=? where id=?", params![summary, gate.to_string(), pid])?;
        let mut p = p;
        p.insert("summary".to_string(), json!(summary));
        release::close(&p, "escalated", "hold", &format!("Gave up after {n} submits: {}. Work is kept in {}", head(&why, 500), wt.display()));
        return Ok(format!("Gate failed and the submit limit ({limit}) is reached. The change is escalated to the owner.\n{why}"));
    }
    Ok(format!("Gate failed (submit {n} of {limit}). Fix it, run check_change, then submit again.\n{why}"))
}

// ---------- the scheduled cycle ----------

pub fn cycle() -> String {
    let Ok(_running) = CYCLE.try_lock() else {
        return "A cycle is already running.".to_string();
    };
    run_cycle().unwrap_or_else(|e| format!("failed: {e}"))
}

fn run_cycle() -> R<String> {
    if store::kv_get("kill") == json!(true) {
        return Ok("Stopped: the kill switch is on.".to_string());
    }
    let cfg = router::config(&root())?;
    detect()?;
    let (mut notes, mut cards) = (Vec::new(), Vec::new());
    for app in runtime::apps(&root()) {
        match eval_check(&app) {
            Ok(card) => cards.push((app, card)),
            Err(e) => notes.push(format!("Could not score {app}: {}.", head(&e, 200))),
        }
    }
    let made = store::scalar("select count(*) from proposals where ts>=? and channel!='agent'", params![today()])? as i64;
    let mut room = cfg["budget"]["daily_proposals"].as_i64().unwrap_or(3) - made;
    for issue in triage(room)? {
        let title = rs(&issue, "title");
        if rs(&issue, "kind") == "error" && !sandbox::available() {
            notes.push(format!("Skipped '{title}': Docker is not running, so code fixes are paused."));
            continue;
        }
        let Some((_, base)) = cards.iter().find(|(a, _)| a == &rs(&issue, "app")) else { continue };
        let pid = propose(&issue, base)?;
        room -= 1;
        notes.push(format!("Change {pid} for '{title}'."));
    }
    for (app, card) in &cards {
        let busy = !store::q("select 1 from proposals where app=? and status in ('testing','awaiting','canary')", params![app])?.is_empty();
        if room > 0 && !busy && store::kv_get("kill") != json!(true) {
            if let Some(pid) = tune_routing(app, card)? {
                room -= 1;
                notes.push(format!("Change {pid}: a better model order for {app}."));
            }
        }
    }
    let scores: Vec<String> = cards.iter().map(|(a, c)| format!("{a} {:.0}%", c["success"].as_f64().unwrap_or(0.0) * 100.0)).collect();
    Ok(format!("Golden cases: {}. {}", scores.join(", "), if notes.is_empty() { "Nothing to propose.".to_string() } else { notes.join(" ") }))
}
