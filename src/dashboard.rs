//! Local dashboard: build an app from an idea, use apps, review changes, watch health. Listens on 127.0.0.1 only.
use crate::store::{self, params, rf, ri, rs, Row};
use crate::util::{clock, esc, head, now, random_hex, read, root, when, R};
use crate::{builder, improve, release, router, runtime, sandbox};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::io::Read;
use std::sync::OnceLock;

const PORT: u16 = 8765;
const CSS: &str = include_str!("../static/dashboard.css");
const FONT: &[u8] = include_bytes!("../static/BarlowCondensed-Bold.ttf"); // bundled, so the look holds offline on every system

/// In every form, so another website cannot press these buttons for you.
fn token() -> &'static str {
    static TOKEN: OnceLock<String> = OnceLock::new();
    TOKEN.get_or_init(|| random_hex(16))
}

type Form = HashMap<String, String>;

fn page(title: &str, body: &str, current: &str, refresh: Option<String>, heading: Option<String>) -> String {
    let waiting = store::scalar("select count(*) from proposals where status='awaiting'", params![]).unwrap_or(0.0) as i64;
    let badge = if waiting > 0 { format!(" <span class=\"badge\" aria-label=\"{waiting} waiting\">{waiting}</span>") } else { String::new() };
    let mut nav = String::new();
    for (href, label, extra) in [("/", "Build an app", ""), ("/apps", "My apps", ""), ("/review", "Review changes", badge.as_str()), ("/health", "Health", "")] {
        nav.push_str(&format!("<a href=\"{href}\"{}>{label}{extra}</a>", if href == current { " aria-current=page" } else { "" }));
    }
    let meta = refresh.map(|url| format!("<meta http-equiv=\"refresh\" content=\"5;url={}\">", esc(&url))).unwrap_or_default();
    // tables scroll sideways on phones
    let body = body.replace("<table", "<div class=\"tw\"><table").replace("</table>", "</table></div>");
    format!("<!doctype html><html lang=\"en\"><head><meta charset=\"utf-8\">{meta}<meta name=\"viewport\" content=\"width=device-width,initial-scale=1\">\
             <title>{} - Forge</title><style>{CSS}</style></head><body><nav class=\"rail\" aria-label=\"Main\"><a class=\"brand\" href=\"/\">FORGE</a>{nav}</nav>\
             <main><h1>{}</h1>{body}</main></body></html>", esc(title), heading.unwrap_or_else(|| esc(title)))
}

fn form(action: &str, inner: &str, class: &str) -> String {
    format!("<form method=\"post\" action=\"{action}\" class=\"{class}\"><input type=\"hidden\" name=\"t\" value=\"{}\">{inner}</form>", token())
}

fn hidden(name: &str, value: &str) -> String {
    format!("<input type=\"hidden\" name=\"{name}\" value=\"{}\">", esc(value))
}

/// A status word set in split-flap tiles. The tiles are decoration; screen readers get the word once.
fn flaps(word: &str, state: &str, small: bool, flip: bool) -> String {
    let tiles: String = word.chars().enumerate().map(|(n, c)| format!("<i style=\"--i:{n}\">{}</i>", esc(&c.to_string()))).collect();
    format!("<span class=\"flaps s-{state}{}{}\" aria-hidden=\"true\">{tiles}</span><span class=\"sr\">{}</span>",
            if small { " sm" } else { "" }, if flip { " flip" } else { "" }, esc(&word.to_lowercase()))
}

fn status_word(status: &str) -> (String, &'static str) {
    match status {
        "testing" => ("BUILDING".into(), "run"),
        "awaiting" => ("WAITING".into(), "run"),
        "canary" => ("TRIAL".into(), "run"),
        "shipped" => ("LIVE".into(), "pass"),
        "escalated" => ("STOPPED".into(), "fail"),
        "rejected" => ("REJECTED".into(), "fail"),
        "rolled_back" => ("UNDONE".into(), "wait"),
        other => (other.to_uppercase(), "wait"),
    }
}

fn took(seconds: f64) -> String {
    format!("{}:{:02}", seconds as i64 / 60, seconds as i64 % 60)
}

fn details(title: &str, text: &str) -> String {
    if text.is_empty() { String::new() } else { format!("<details><summary>{}</summary><pre>{}</pre></details>", esc(title), esc(text)) }
}

fn gate_of(p: &Row) -> Value {
    serde_json::from_str(&rs(p, "gate")).unwrap_or(json!({}))
}

fn alert(message: &str) -> String {
    format!("<p class=\"bad\" role=\"alert\">{}</p>", esc(message))
}

// ---------- pages ----------

fn build_home(values: &Form, error: &str) -> R<String> {
    let v = |k: &str| esc(values.get(k).map(String::as_str).unwrap_or(""));
    let who = if builder::kind() == "claude" { "Claude Code" } else { "your local model" };
    let mut body = format!("<p>Describe an app. Forge has {who} build it in Rust, tests it in a locked sandbox, and makes it live \
                            once it passes every check. To change who builds, run <code>forge setup</code>.</p>");
    if !error.is_empty() {
        body.push_str(&alert(error));
    }
    body.push_str(&form("/build", &format!(
        "<label for=\"idea\">What should the app do?</label>\
         <textarea id=\"idea\" name=\"idea\" rows=\"6\" required minlength=\"10\" maxlength=\"4000\" placeholder=\"A scientific calculator with sin, cos, log and powers\">{}</textarea>\
         <label for=\"examples\">Examples it must get right <span class=\"hint\">Optional. One per line: what you type, an arrow, and the exact answer. The builder cannot change these.</span></label>\
         <textarea id=\"examples\" name=\"examples\" rows=\"4\" placeholder=\"2+3*4 -> 14\">{}</textarea>\
         <label for=\"name\">Name <span class=\"hint\">Optional. Made from your description if left empty.</span></label>\
         <input type=\"text\" id=\"name\" name=\"name\" maxlength=\"31\" value=\"{}\">\
         <button>Build it</button> <span class=\"muted\">Usually a few minutes. You can leave the page while it builds.</span>", v("idea"), v("examples"), v("name")), ""));
    let builds = store::q("select * from proposals where channel='agent' order by id desc limit 8", params![])?;
    if !builds.is_empty() {
        body.push_str("<h2>Recent builds</h2><table><tr><th>Build</th><th>App</th><th>Status</th><th>Started</th></tr>");
        for p in &builds {
            let (word, state) = status_word(&rs(p, "status"));
            body.push_str(&format!("<tr><td><a href=\"/build?id={id}\">Build {id}</a></td><td>{}</td><td>{}</td><td>{}</td></tr>",
                esc(&rs(p, "app")), flaps(&word, state, true, false), when(rf(p, "ts")), id = ri(p, "id")));
        }
        body.push_str("</table>");
    }
    Ok(page("Build an app", &body, "/", None, None))
}

/// The departure board: one row per check, with its state worked out from the recorded progress.
/// Each row: (name, note, word, state, one-letter code, time, took).
fn board_rows(p: &Row) -> Vec<(String, String, String, &'static str, char, String, String)> {
    let stages = store::kv_get(&format!("stages:{}", ri(p, "id")));
    let status = rs(p, "status");
    let (live, finished) = (status == "testing", ["awaiting", "canary", "shipped"].contains(&status.as_str()) && !rs(p, "gate").is_empty());
    let rows = [("write", "The builder writes the app", "Claude Code or your local model", "DONE"),
                ("tests", "Compile and unit tests", "Built and run with no network", "PASSED"),
                ("scans", "Security scans", "gitleaks, semgrep and trivy", "PASSED"),
                ("examples", "Golden cases", "Your examples, then the builder's own", "PASSED"),
                ("live", "Goes live", if rs(p, "channel") == "agent" { "Appears under My apps" } else { "After your approval" }, "LIVE")];
    rows.iter().map(|(key, name, note, done_word)| {
        let mut s = stages[*key].clone();
        if s.is_null() && ((finished && *key != "live") || (*key == "live" && status == "shipped")) {
            s = json!({"ok": true}); // recorded before per-check progress existed, or released by approval
        }
        let (word, state, code) = match (s.is_object(), s["ok"].as_bool()) {
            (false, _) => ("WAIT", "wait", 'w'),
            (true, Some(true)) => (*done_word, "pass", 'p'),
            (true, Some(false)) => ("FAILED", "fail", 'f'),
            (true, None) if live => ("RUNNING", "run", 'r'),
            (true, None) => ("STOPPED", "fail", 'x'),
        };
        let start = s["start"].as_f64();
        let end = s["end"].as_f64().or(if code == 'r' { Some(now()) } else { None });
        let spent = match (start, end) {
            (Some(a), Some(b)) => took(b - a),
            _ => "–".to_string(),
        };
        (name.to_string(), note.to_string(), word.to_string(), state, code, clock(start.unwrap_or(0.0)), spent)
    }).collect()
}

fn build_page(pid: i64, seen: &str) -> R<String> {
    let Some(p) = store::q("select * from proposals where id=?", params![pid])?.into_iter().next() else {
        return Ok(page("Not found", "<p>There is no build with that number. <a href=\"/\">Start a new one</a>.</p>", "/", None, None));
    };
    let (app, status) = (rs(&p, "app"), rs(&p, "status"));
    let board = board_rows(&p);
    let code: String = board.iter().map(|r| r.4).collect();
    let top = match status.as_str() {
        "shipped" => format!("<div class=\"done\" role=\"status\"><p class=\"ok\">Your app is live.</p><a class=\"button\" href=\"/use?app={a}\">Open {a}</a></div>", a = esc(&app)),
        "testing" => "<p class=\"meta\" role=\"status\">Building. You can leave this page; it carries on and this board updates itself.</p>".to_string(),
        "awaiting" => "<p role=\"status\">It passed every check and is waiting for your approval on <a href=\"/review\">Review changes</a>.</p>".to_string(),
        _ => format!("<p class=\"bad\" role=\"status\">It did not pass the checks. {}</p><p>Try again with a clearer description or a few examples. <a href=\"/\">Start a new build</a>.</p>", esc(&rs(&p, "reason"))),
    };
    let attempt = store::kv_get(&format!("attempt:{pid}")).as_i64().unwrap_or(1);
    let tries = if attempt > 1 { format!(" · attempt {attempt} of {}", router::config(&root())?["budget"]["max_attempts"]) } else { String::new() };
    let mut body = format!("<p class=\"meta\">Started {}{tries}</p>{top}", when(rf(&p, "ts")));
    if let Some(idea) = store::kv_get(&format!("idea:{pid}")).as_str() {
        body.push_str(&format!("<blockquote class=\"ask\">{}</blockquote>", esc(idea)));
    }
    let mut table = "<table class=\"board\"><caption class=\"sr\">Checks for this build</caption><tr><th class=\"tc\" scope=\"col\">Time</th>\
                     <th scope=\"col\">Check</th><th scope=\"col\">Status</th><th class=\"dc\" scope=\"col\" style=\"text-align:right\">Took</th></tr>".to_string();
    for (n, (name, note, word, state, c, time, spent)) in board.iter().enumerate() {
        // flip only what changed since the last look
        let changed = seen.len() == board.len() && seen.chars().nth(n) != Some(*c);
        table.push_str(&format!("<tr><td class=\"t tc\">{time}</td><td class=\"what\">{}<small>{}</small></td><td>{}</td><td class=\"dur\">{spent}</td></tr>",
            esc(name), esc(note), flaps(word, state, false, changed)));
    }
    table.push_str("</table>");
    let golden = [root(), improve::worktree(pid)].iter().map(|d| d.join("apps").join(&app).join("evals").join("golden.jsonl")).find(|f| f.exists());
    let mine: Vec<Value> = golden.and_then(|f| read(&f).ok()).unwrap_or_default().lines().filter_map(|l| serde_json::from_str::<Value>(l).ok())
        .filter(|c| c["id"].as_str().is_some_and(|id| id.starts_with("you-"))).collect();
    let mut side = "<div class=\"panel\"><h2>Your examples</h2>".to_string();
    if mine.is_empty() {
        side.push_str("<p class=muted>None given, so the builder's own checks decide.</p>");
    } else {
        side.push_str("<table class=\"examples\"><tr><th scope=\"col\">You type</th><th scope=\"col\">Answer</th></tr>");
        for c in &mine {
            side.push_str(&format!("<tr><td>{}</td><td>{}</td></tr>", esc(c["input"]["input"].as_str().unwrap_or("")), esc(c["answer"].as_str().unwrap_or(""))));
        }
        side.push_str("</table>");
    }
    let gate = gate_of(&p);
    let log: Vec<String> = store::kv_get(&format!("build:{pid}")).as_array().into_iter().flatten().filter_map(|l| l.as_str().map(String::from)).collect();
    let record = format!("{}{}{}{}", details("Build log", &log.join("\n")), details("Test output", gate["tests"].as_str().unwrap_or("")),
                         details("Security scans", gate["scans"].as_str().unwrap_or("")), details("The code", &rs(&p, "diff")));
    side.push_str(&format!("</div><div class=\"panel\"><h2>Technical record</h2>{}</div>",
        if record.is_empty() { "<p class=muted>Appears as the checks run.</p>".to_string() } else { record }));
    body.push_str(&format!("<div class=\"split\"><section aria-label=\"Checks\">{table}</section><aside>{side}</aside></div>"));
    let refresh = (status == "testing").then(|| format!("/build?id={pid}&seen={code}"));
    Ok(page(&format!("Build {pid}: {app}"), &body, "/", refresh, Some(format!("<span class=\"plat\">{pid}</span>{}", esc(&app)))))
}

fn apps_page() -> R<String> {
    let mut rows = String::new();
    for a in runtime::apps(&root()) {
        let used = store::scalar("select count(*) from traces where app=? and version!='eval'", params![a])? as i64;
        let released = store::q("select id, ts from proposals where app=? and status='shipped' order by id", params![a])?;
        let number = released.last().map(|r| format!("<span class=\"plat\">{}</span>", ri(r, "id"))).unwrap_or_default();
        let built = released.first().map(|r| when(rf(r, "ts"))).unwrap_or_else(|| "with Forge".to_string());
        let note = runtime::manifest(&root(), &a)["description"].as_str().unwrap_or("No description.").to_string();
        rows.push_str(&format!("<tr><td><a href=\"/use?app={a}\"><strong>{number}{a}</strong></a></td><td>{}</td><td>{built}</td><td>{used}</td></tr>", esc(&note)));
    }
    let body = if rows.is_empty() { "<p>No apps yet. <a href=\"/\">Build one</a>.</p>".to_string() } else {
        format!("<table><tr><th scope=\"col\">App</th><th scope=\"col\">What it does</th><th scope=\"col\">Built</th><th scope=\"col\">Used</th></tr>{rows}</table>")
    };
    Ok(page("My apps", &body, "/apps", None, None))
}

fn app_page(app: &str, values: &Form, result: Option<&Value>, note: &str) -> R<String> {
    let mut fields = hidden("app", app);
    for field in runtime::manifest(&root(), app)["fields"].as_array().into_iter().flatten() {
        let (name, label, kind) = (field[0].as_str().unwrap_or(""), field[1].as_str().unwrap_or(""), field[2].as_str().unwrap_or("text"));
        let (name, value) = (esc(name), esc(values.get(name).map(String::as_str).unwrap_or("")));
        fields.push_str(&format!("<label for=\"{name}\">{}</label>", esc(label)));
        fields.push_str(&if kind == "textarea" { format!("<textarea id=\"{name}\" name=\"{name}\" rows=\"10\">{value}</textarea>") }
                         else { format!("<input type=\"text\" id=\"{name}\" name=\"{name}\" value=\"{value}\">") });
    }
    let mut body = format!("{note}{}", form("/ask", &format!("{fields}<button>Run</button> <span class=muted>Apps that use a local model can take up to a minute.</span>"), ""));
    if let Some(r) = result {
        if r["ok"] == true {
            let text = r["output"]["answer"].as_str().map(String::from).unwrap_or_else(|| serde_json::to_string_pretty(&r["output"]).unwrap_or_default());
            body.push_str(&format!("<section class=\"card\" aria-live=\"polite\"><h2>Result</h2><pre>{}</pre><p class=\"muted\">{:.1} s, cost ${:.4}</p>{}</section>",
                esc(&text), r["ms"].as_f64().unwrap_or(0.0) / 1000.0, r["usd"].as_f64().unwrap_or(0.0),
                form("/feedback", &format!("{}{}<label for=\"fb\">Was this good? If not, say what was wrong (optional)</label>\
                    <input type=\"text\" id=\"fb\" name=\"text\" maxlength=\"500\"><button name=\"rating\" value=\"1\">Good</button>\
                    <button name=\"rating\" value=\"-1\" class=\"quiet\">Not good</button>", hidden("trace", r["trace_id"].as_str().unwrap_or("")), hidden("app", app)), "")));
        } else {
            body.push_str(&alert(r["error"].as_str().unwrap_or("")));
        }
    }
    Ok(page(app, &body, "/apps", None, None))
}

fn score_table(before: &Value, after: &Value) -> String {
    if !before.is_object() || !after.is_object() {
        return String::new();
    }
    let f = |v: &Value, k: &str| v[k].as_f64().unwrap_or(0.0);
    let rows = [("Golden cases passing", format!("{:.0}%", f(before, "success") * 100.0), format!("{:.0}%", f(after, "success") * 100.0)),
                ("Slowest request", format!("{:.1} s", f(before, "p95_ms") / 1000.0), format!("{:.1} s", f(after, "p95_ms") / 1000.0)),
                ("Cost per request", format!("${:.4}", f(before, "usd_per_req")), format!("${:.4}", f(after, "usd_per_req")))];
    format!("<table><tr><th>Measure</th><th>Now</th><th>With this change</th></tr>{}</table>",
            rows.iter().map(|(a, b, c)| format!("<tr><td>{a}</td><td>{b}</td><td>{c}</td></tr>")).collect::<String>())
}

fn proposal_card(p: &Row) -> R<String> {
    let (gate, pid, channel) = (gate_of(p), ri(p, "id"), rs(p, "channel"));
    let mut risk = if channel == "code" || channel == "agent" { "Code change: read the diff carefully.".to_string() }
                   else { "Low risk: only wording or model order changes. No code.".to_string() };
    if let Some(w) = gate["warning"].as_str() {
        risk.push_str(&format!(" <strong class=\"bad\">{}</strong>", esc(w)));
    }
    let attempts = ri(p, "attempts").max(1);
    let mut body = format!("<section class=\"card\"><h2>Change {pid}: {}</h2><p><strong>{}</strong>, {} change, {attempts} attempt(s). {risk}</p>",
                           esc(&rs(p, "summary")), esc(&rs(p, "app")), esc(&channel));
    if let Some(issue) = store::q("select * from issues where id=?", params![ri(p, "issue_id")])?.first() {
        body.push_str(&format!("<p>Why: {} (seen {} times).</p>", esc(&rs(issue, "title")), ri(issue, "count")));
    }
    if !rs(p, "reason").is_empty() {
        body.push_str(&format!("<p class=\"bad\">{}</p>", esc(&rs(p, "reason"))));
    }
    body.push_str(&score_table(&gate["before"], &gate["after"]));
    body.push_str(&format!("<details open><summary>What changes</summary><pre>{}</pre></details>", esc(&rs(p, "diff"))));
    body.push_str(&details("Test output", gate["tests"].as_str().unwrap_or("")));
    body.push_str(&details("Security scans", gate["scans"].as_str().unwrap_or("")));
    if rs(p, "status") == "awaiting" {
        body.push_str(&form("/approve", &format!("{}<button>Approve</button>", hidden("id", &pid.to_string())), "inline"));
        body.push_str(&form("/reject", &format!("{}<label for=\"r{pid}\">Reason for rejecting (it will be remembered)</label>\
            <input type=\"text\" id=\"r{pid}\" name=\"reason\" maxlength=\"300\"><button class=\"danger\">Reject</button>", hidden("id", &pid.to_string())), ""));
    }
    Ok(body + "</section>")
}

fn review_page() -> R<String> {
    let mut body = String::new();
    let c = store::kv_get("canary");
    if let Some(pid) = c["proposal"].as_i64() {
        let n = store::scalar("select count(*) from traces where version=? and ts>=?", params![format!("canary:p{pid}"), c["since"].as_f64().unwrap_or(0.0)])? as i64;
        body.push_str(&format!("<p role=\"status\">Change {pid} is in its trial: {n} of {} trial requests so far. It handles about {}% of requests and is discarded automatically if they fail.</p>", c["need"], c["pct"]));
    }
    let waiting = store::q("select * from proposals where status='awaiting' order by id", params![])?;
    if waiting.is_empty() {
        body.push_str("<p>Nothing is waiting for your approval.</p>");
    }
    for p in &waiting {
        body.push_str(&proposal_card(p)?);
    }
    let stuck = store::q("select * from proposals where status='escalated' order by id desc limit 5", params![])?;
    if !stuck.is_empty() {
        body.push_str("<h2>Needs your help</h2>");
        for p in &stuck {
            body.push_str(&format!("<section class=\"card\"><h3>{}</h3><p>{}</p>{}</section>", esc(&rs(p, "summary")), esc(&rs(p, "reason")),
                form("/reopen", &format!("{}<button class=\"quiet\">Let it try again</button>", hidden("id", &ri(p, "issue_id").to_string())), "inline")));
        }
    }
    let done = store::q("select * from proposals where status in ('shipped','rejected','rolled_back') order by id desc limit 10", params![])?;
    if !done.is_empty() {
        body.push_str("<h2>History</h2><table><tr><th>Change</th><th>Outcome</th><th>Note</th><th></th></tr>");
        for p in &done {
            let undo = if rs(p, "status") == "shipped" { form("/undo", &format!("{}<button class=\"quiet\">Undo</button>", hidden("id", &ri(p, "id").to_string())), "inline") } else { String::new() };
            body.push_str(&format!("<tr><td>{}: {}</td><td>{}</td><td>{}</td><td>{undo}</td></tr>", ri(p, "id"), esc(&rs(p, "summary")),
                esc(&rs(p, "status").replace('_', " ")), esc(&rs(p, "reason"))));
        }
        body.push_str("</table>");
    }
    Ok(page("Review changes", &body, "/review", None, None))
}

fn health_page() -> R<String> {
    let cfg = router::config(&root())?;
    let kill = store::kv_get("kill") == json!(true);
    let last = store::kv_get("last_cycle");
    let mut body = format!("<p>Paid model spend today: <strong>${:.2}</strong> of ${:.2} allowed. Local models are free.</p>",
                           router::spent_today(), cfg["budget"]["daily_usd"].as_f64().unwrap_or(0.0));
    let roles: Vec<String> = cfg["models"].as_object().into_iter().flatten().map(|(r, m)| format!("{} = {}", esc(r), esc(m["model"].as_str().unwrap_or("")))).collect();
    body.push_str(&if roles.is_empty() {
        "<p class=\"bad\">No models are set up, so apps that use a model will not work. Apps that use no model are fine. Run <code>forge setup</code> to choose models.</p>".to_string()
    } else { format!("<p>Models: {}.</p>", roles.join(", ")) });
    body.push_str(&format!("<p>Sandbox (Docker): {}</p>", if sandbox::available() { "<span class=ok>running</span>" }
        else { "<span class=bad>not running</span>. Nothing can be built or fixed until it is; released apps still work." }));
    body.push_str(&format!("<p>Improvement engine: {}. Last run {}: {}</p>", if kill { "<span class=bad>stopped by you</span>" } else { "on" },
        when(last["ts"].as_f64().unwrap_or(0.0)), esc(last["result"].as_str().unwrap_or("not run yet"))));
    body.push_str(&form("/run", "<button class=\"quiet\">Run an improvement cycle now</button>", "inline"));
    body.push_str(&form("/kill", &format!("{}{}", hidden("on", if kill { "0" } else { "1" }),
        if kill { "<button class=\"quiet\">Turn the improvement engine back on</button>" } else { "<button class=\"danger\">Stop all self-improvement</button>" }), "inline"));
    body.push_str("<h2>Scorecard</h2><table><tr><th>When</th><th>App</th><th>Golden cases passing</th><th>Slowest</th><th>Cost per request</th></tr>");
    for r in store::q("select * from scorecards where version='stable' order by id desc limit 8", params![])? {
        let c: Value = serde_json::from_str(&rs(&r, "card")).unwrap_or(json!({}));
        body.push_str(&format!("<tr><td>{}</td><td>{}</td><td>{:.0}% of {}</td><td>{:.1} s</td><td>${:.4}</td></tr>", when(rf(&r, "ts")), esc(&rs(&r, "app")),
            c["success"].as_f64().unwrap_or(0.0) * 100.0, c["n"], c["p95_ms"].as_f64().unwrap_or(0.0) / 1000.0, c["usd_per_req"].as_f64().unwrap_or(0.0)));
    }
    body.push_str("</table><h2>Problems found</h2><table><tr><th>Problem</th><th>Type</th><th>Seen</th><th>Last</th><th>Status</th></tr>");
    for r in store::q("select * from issues order by last_ts desc limit 20", params![])? {
        body.push_str(&format!("<tr><td>{}</td><td>{}</td><td>{}</td><td>{}</td><td>{}</td></tr>", esc(&rs(&r, "title")), esc(&rs(&r, "kind")), ri(&r, "count"), when(rf(&r, "last_ts")), esc(&rs(&r, "status"))));
    }
    body.push_str("</table><h2>Recent requests</h2><table><tr><th>When</th><th>App</th><th>Version</th><th>Result</th><th>Time</th></tr>");
    for r in store::q("select * from traces where version!='eval' order by ts desc limit 15", params![])? {
        let result = if ri(&r, "ok") == 1 { "ok".to_string() } else { esc(&head(&if rs(&r, "fingerprint").is_empty() { rs(&r, "error") } else { rs(&r, "fingerprint") }, 80)) };
        body.push_str(&format!("<tr><td>{}</td><td>{}</td><td>{}</td><td>{result}</td><td>{:.1} s</td></tr>", when(rf(&r, "ts")), esc(&rs(&r, "app")), esc(&rs(&r, "version")), rf(&r, "ms") / 1000.0));
    }
    Ok(page("Health", &(body + "</table>"), "/health", None, None))
}

// ---------- background work ----------

fn run_cycle() {
    let _ = store::kv_set("last_cycle", &json!({"ts": now(), "result": "running now"}));
    let result = improve::cycle();
    let _ = store::kv_set("last_cycle", &json!({"ts": now(), "result": result}));
}

fn scheduler() {
    if !store::kv_get("last_cycle").is_object() {
        // the first automatic run is a full interval away
        let _ = store::kv_set("last_cycle", &json!({"ts": now(), "result": "not run yet"}));
    }
    loop {
        std::thread::sleep(std::time::Duration::from_secs(60));
        let hours = router::config(&root()).ok().and_then(|c| c["improve"]["interval_hours"].as_f64()).unwrap_or(24.0);
        let due = now() - store::kv_get("last_cycle")["ts"].as_f64().unwrap_or(now()) > hours * 3600.0;
        if due && store::kv_get("kill") != json!(true) {
            run_cycle();
        }
    }
}

// ---------- http ----------

fn decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let (mut out, mut i) = (Vec::with_capacity(bytes.len()), 0);
    while i < bytes.len() {
        match bytes[i] {
            b'+' => out.push(b' '),
            b'%' if i + 2 < bytes.len() + 0 && bytes.get(i + 1..i + 3).and_then(|h| u8::from_str_radix(std::str::from_utf8(h).ok()?, 16).ok()).is_some() => {
                out.push(u8::from_str_radix(std::str::from_utf8(&bytes[i + 1..i + 3]).unwrap_or("00"), 16).unwrap_or(b'?'));
                i += 2;
            }
            b => out.push(b),
        }
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn parse_form(body: &str) -> Form {
    body.split('&').filter(|p| !p.is_empty()).map(|pair| {
        let (k, v) = pair.split_once('=').unwrap_or((pair, ""));
        (decode(k), decode(v))
    }).collect()
}

fn same(a: &str, b: &str) -> bool {
    a.len() == b.len() && a.bytes().zip(b.bytes()).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

enum Reply {
    Html(u16, String),
    Redirect(String),
    Font,
}

fn get(path: &str, query: &Form) -> R<Reply> {
    Ok(match path {
        "/font/signage.ttf" => Reply::Font,
        "/" => Reply::Html(200, build_home(&Form::new(), "")?),
        "/apps" => Reply::Html(200, apps_page()?),
        "/use" => {
            let app = query.get("app").cloned().unwrap_or_default();
            if runtime::apps(&root()).contains(&app) { Reply::Html(200, app_page(&app, &Form::new(), None, "")?) }
            else { Reply::Html(404, page("Not found", "<p>No such app. <a href=\"/apps\">See your apps</a>.</p>", "/apps", None, None)) }
        }
        "/build" => {
            let seen = query.get("seen").filter(|s| s.chars().all(|c| c.is_ascii_alphabetic())).cloned().unwrap_or_default();
            match query.get("id").and_then(|id| id.parse::<i64>().ok()) {
                Some(pid) => Reply::Html(200, build_page(pid, &seen)?),
                None => Reply::Html(404, page("Not found", "<p>No such build.</p>", "/", None, None)),
            }
        }
        "/review" => Reply::Html(200, review_page()?),
        "/health" => Reply::Html(200, health_page()?),
        _ => Reply::Html(404, page("Not found", "<p>That page does not exist. <a href=\"/\">Go to Forge</a>.</p>", "/", None, None)),
    })
}

fn post(path: &str, mut f: Form) -> R<Reply> {
    let id = |f: &Form| f.get("id").and_then(|v| v.parse::<i64>().ok()).ok_or_else(|| "That change number is missing.".to_string());
    Ok(match path {
        "/build" => match builder::start(f.get("idea").map_or("", String::as_str), f.get("name").map_or("", String::as_str), f.get("examples").map_or("", String::as_str)) {
            Ok(pid) => Reply::Redirect(format!("/build?id={pid}")),
            Err(e) => Reply::Html(400, build_home(&f, &e)?),
        },
        "/ask" => {
            let app = f.remove("app").unwrap_or_default();
            if !runtime::apps(&root()).contains(&app) {
                return Err("Unknown app.".to_string());
            }
            let result = runtime::serve(&app, &json!(f));
            Reply::Html(200, app_page(&app, &f, Some(&result), "")?)
        }
        "/feedback" => {
            let app = f.get("app").cloned().unwrap_or_default();
            if !runtime::apps(&root()).contains(&app) {
                return Err("Unknown app.".to_string());
            }
            store::x("insert into feedback(ts,trace_id,rating,text) values(?,?,?,?)", params![now(), f.get("trace").map_or("", String::as_str),
                if f.get("rating").map(String::as_str) == Some("1") { 1 } else { -1 }, head(f.get("text").map_or("", String::as_str), 500)])?;
            Reply::Html(200, app_page(&app, &Form::new(), None, "<p class=\"ok\" role=\"status\">Thank you. Your feedback was saved.</p>")?)
        }
        "/approve" => {
            release::approve(id(&f)?)?;
            Reply::Redirect("/review".into())
        }
        "/reject" => {
            release::reject(id(&f)?, f.get("reason").map_or("", String::as_str))?;
            Reply::Redirect("/review".into())
        }
        "/undo" => {
            release::undo(id(&f)?)?;
            Reply::Redirect("/review".into())
        }
        "/reopen" => {
            store::x("update issues set status='open' where id=? and status='hold'", params![id(&f)?])?;
            Reply::Redirect("/review".into())
        }
        "/kill" => {
            store::kv_set("kill", &json!(f.get("on").map(String::as_str) == Some("1")))?;
            Reply::Redirect("/health".into())
        }
        "/run" => {
            std::thread::spawn(run_cycle);
            Reply::Redirect("/health".into())
        }
        _ => Reply::Html(404, page("Not found", "<p>That action does not exist.</p>", "/", None, None)),
    })
}

fn respond(mut req: tiny_http::Request) {
    let header = |name: &'static str, value: &str| tiny_http::Header::from_bytes(name.as_bytes(), value.as_bytes()).expect("valid header");
    let host = req.headers().iter().find(|h| h.field.equiv("Host")).map(|h| h.value.as_str().split(':').next().unwrap_or("").to_string()).unwrap_or_default();
    let url = req.url().to_string();
    let (path, query) = url.split_once('?').unwrap_or((&url, ""));
    let is_post = *req.method() == tiny_http::Method::Post;
    let problem = |title: &str, message: &str| Reply::Html(400, page(title, &format!("{}<p><a href=\"/\">Back to Forge</a></p>", alert(message)), "/", None, None));
    let reply = if host != "127.0.0.1" && host != "localhost" {
        Reply::Html(403, "Forbidden".to_string())
    } else if !is_post {
        get(path, &parse_form(query)).unwrap_or_else(|e| problem("Something went wrong", &e))
    } else if req.body_length().unwrap_or(0) > 400_000 {
        problem("Could not do that", "That request was too large.")
    } else {
        let mut body = String::new();
        let _ = req.as_reader().take(400_001).read_to_string(&mut body);
        let mut f = parse_form(&body);
        if !same(&f.remove("t").unwrap_or_default(), token()) {
            Reply::Html(403, page("Page out of date", "<p role=\"alert\">This page is from before the last restart. <a href=\"/\">Reload it</a> and try again.</p>", "/", None, None))
        } else {
            post(path, f).unwrap_or_else(|e| problem("Could not do that", &e))
        }
    };
    let _ = match reply {
        Reply::Html(code, html) => req.respond(tiny_http::Response::from_string(html).with_status_code(code).with_header(header("Content-Type", "text/html; charset=utf-8"))),
        Reply::Redirect(to) => req.respond(tiny_http::Response::from_string("").with_status_code(303).with_header(header("Location", &to))),
        Reply::Font => req.respond(tiny_http::Response::from_data(FONT.to_vec()).with_header(header("Content-Type", "font/ttf")).with_header(header("Cache-Control", "max-age=86400"))),
    };
}

pub fn serve() -> R<()> {
    let port = std::env::var("FORGE_PORT").ok().and_then(|p| p.parse::<u16>().ok()).unwrap_or(PORT);
    let busy = format!("Port {port} is already in use, by Forge or another program. Close it, or pick another port by setting FORGE_PORT (for example FORGE_PORT=8766).");
    // Windows lets two programs listen on one port, so look before binding
    let addr = std::net::SocketAddr::from(([127, 0, 0, 1], port));
    if std::net::TcpStream::connect_timeout(&addr, std::time::Duration::from_millis(300)).is_ok() {
        return Err(busy);
    }
    let server = tiny_http::Server::http(addr).map_err(|_| busy)?;
    println!("Forge is running. Open http://127.0.0.1:{port} in your browser. Press Ctrl+C to stop.");
    std::thread::spawn(scheduler);
    for req in server.incoming_requests() {
        std::thread::spawn(move || respond(req));
    }
    Ok(())
}
