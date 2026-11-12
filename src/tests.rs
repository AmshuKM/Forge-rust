//! Tests for the engine. Each runs against a throwaway project folder and an in-memory database.
use crate::runtime::Invoker;
use crate::store::params;
use crate::util::{now, R};
use crate::{builder, evals, improve, mcp, models, release, router, runtime, store, util};
use serde_json::{json, Value};
use std::path::PathBuf;
use std::sync::{Arc, Mutex, MutexGuard};

static LOCK: Mutex<()> = Mutex::new(()); // tests share the process environment, so they run one at a time

struct Env {
    dir: PathBuf,
    _guard: MutexGuard<'static, ()>,
}

const LOCAL: &str = "[endpoints.ollama]\nbase = \"http://unused.invalid/v1\"\n[models.main]\nendpoint = \"ollama\"\nmodel = \"a\"\n[models.big]\nendpoint = \"ollama\"\nmodel = \"b\"\n";

fn env() -> Env {
    let guard = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let dir = std::env::temp_dir().join(format!("forge-test-{}", util::random_hex(6)));
    for sub in ["config", "prompts", "apps"] {
        std::fs::create_dir_all(dir.join(sub)).unwrap();
    }
    std::fs::copy(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("config").join("routing.toml"), dir.join("config").join("routing.toml")).unwrap();
    std::fs::write(dir.join("prompts").join("p.md"), "Doc: {{document}} Q: {{question}}").unwrap();
    std::fs::write(dir.join("local.toml"), LOCAL).unwrap(); // a fixed pair of models, so tests never depend on this computer's setup
    std::env::set_var("FORGE_ROOT", &dir);
    std::env::set_var("FORGE_DB", ":memory:");
    std::env::set_var("FORGE_LOCAL_CONFIG", dir.join("local.toml"));
    std::env::remove_var("FORGE_MOCK");
    store::reset();
    *router::POSTER.lock().unwrap_or_else(|e| e.into_inner()) = None;
    Env { dir, _guard: guard }
}

impl Drop for Env {
    fn drop(&mut self) {
        *router::POSTER.lock().unwrap_or_else(|e| e.into_inner()) = None;
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn fake_network(f: impl Fn(&str, &Value) -> R<Value> + Send + 'static) {
    *router::POSTER.lock().unwrap_or_else(|e| e.into_inner()) = Some(Box::new(f));
}

fn user(text: &str) -> Value {
    json!([{"role": "user", "content": text}])
}

/// An app made of a closure, standing in for a compiled program.
struct Fake<F: FnMut(&Value) -> R<Value>>(F);

impl<F: FnMut(&Value) -> R<Value>> Invoker for Fake<F> {
    fn invoke(&mut self, input: &Value) -> R<Value> {
        (self.0)(input)
    }
}

#[test]
fn router_falls_back_to_the_next_model() {
    let e = env();
    let seen = Arc::new(Mutex::new(Vec::new()));
    let log = seen.clone();
    fake_network(move |_, body| {
        let mut seen = log.lock().unwrap();
        seen.push(body["model"].clone());
        if seen.len() == 1 { Err("down".to_string()) } else { Ok(json!({"choices": [{"message": {"content": "hi"}}], "usage": {}})) }
    });
    assert_eq!(router::call("summarize", &user("x"), &e.dir, "", true).unwrap(), "hi");
    assert_eq!(*seen.lock().unwrap(), vec![json!("a"), json!("b")]);
    let oks: Vec<i64> = store::q("select ok from calls order by id", params![]).unwrap().iter().map(|r| store::ri(r, "ok")).collect();
    assert_eq!(oks, vec![0, 1]);
}

#[test]
fn router_stops_when_every_model_fails_and_names_missing_setup() {
    let e = env();
    fake_network(|_, _| Err("down".to_string()));
    assert!(router::call("summarize", &user("x"), &e.dir, "", true).unwrap_err().starts_with("RouterError"));
    std::env::set_var("FORGE_LOCAL_CONFIG", e.dir.join("none.toml"));
    assert!(router::call("summarize", &user("x"), &e.dir, "", true).unwrap_err().contains("forge setup"));
}

#[test]
fn router_enforces_the_daily_budget_on_paid_models() {
    let e = env();
    std::fs::write(e.dir.join("config").join("routing.toml"), "[budget]\ndaily_usd = 1.0\n[endpoints.c]\nbase = \"http://unused.invalid/v1\"\nkey_env = \"FORGE_TEST_KEY\"\n\
        [models.paid]\nendpoint = \"c\"\nmodel = \"m\"\nusd_in = 1.0\nusd_out = 1.0\n[tasks]\nt = [\"paid\"]\n").unwrap();
    std::env::set_var("FORGE_TEST_KEY", "k");
    fake_network(|_, _| panic!("a paid model was called over budget"));
    store::x("insert into calls(ts,usd,ok) values(?,?,1)", params![now(), 2.0]).unwrap();
    assert!(router::call("t", &user("x"), &e.dir, "", true).unwrap_err().contains("budget"));
}

#[test]
fn local_file_adds_models_without_touching_shared_settings_and_mock_needs_no_model() {
    let e = env();
    let cfg = router::config(&e.dir).unwrap();
    assert_eq!(cfg["models"]["main"]["model"], "a");
    assert_eq!(cfg["tasks"]["summarize"], json!(["main", "big"]));
    assert!(cfg["budget"]["daily_usd"].is_number());
    std::env::set_var("FORGE_MOCK", "1");
    fake_network(|_, _| panic!("a model was called in mock mode"));
    assert!(router::call("summarize", &user("hi"), &e.dir, "", true).unwrap().starts_with("MOCK REPLY"));
    std::env::remove_var("FORGE_MOCK");
}

#[test]
fn render_does_not_expand_slots_inside_values() {
    let e = env();
    assert_eq!(runtime::render(&e.dir, "p", &json!({"document": "{{question}}", "question": "SECRET"})).unwrap(), "Doc: {{question}} Q: SECRET");
    assert!(runtime::render(&e.dir, "../config/routing", &json!({})).is_err());
}

#[test]
fn runtime_retries_are_bounded_and_failure_is_safe() {
    let e = env();
    let calls = Arc::new(Mutex::new(0));
    let count = calls.clone();
    fake_network(move |_, _| {
        *count.lock().unwrap() += 1;
        Ok(json!({"choices": [{"message": {"content": "never good enough"}}]}))
    });
    let mut app = Fake(|input: &Value| Ok(if input["reply"].is_null() { json!({"llm": {"task": "answer", "prompt": "p", "vars": {}}}) } else { json!({"invalid": "it had 3 mistakes"}) }));
    let r = runtime::handle("t", &json!({"x": "1"}), &e.dir, "stable", true, &mut app);
    assert_eq!(r["ok"], false);
    assert_eq!(r["error"], runtime::SAFE_ERROR);
    assert_eq!(*calls.lock().unwrap(), runtime::RETRIES + 1);
    assert_eq!(store::rs(&store::q("select fingerprint from traces", params![]).unwrap()[0], "fingerprint"), "OutputInvalid:t:it had N mistakes");
}

#[test]
fn input_errors_are_shown_crashes_are_fingerprinted_and_endless_apps_stop() {
    let e = env();
    let r = runtime::handle("t", &json!({}), &e.dir, "stable", true, &mut Fake(|_: &Value| Ok(json!({"error": "Need x."}))));
    assert_eq!(r["error"], "Need x.");
    runtime::handle("t", &json!({}), &e.dir, "stable", true, &mut Fake(|_: &Value| Err("Crash: thread 'main' panicked at src/main.rs:12:5:\nboom".to_string())));
    runtime::handle("t", &json!({}), &e.dir, "stable", true, &mut Fake(|_: &Value| Ok(json!({"invalid": "no"}))));
    let ok = runtime::handle("t", &json!({}), &e.dir, "stable", true, &mut Fake(|_: &Value| Ok(json!({"output": {"answer": "a"}}))));
    assert_eq!(ok["output"]["answer"], "a");
    let fps: Vec<Value> = store::q("select fingerprint from traces order by ts", params![]).unwrap().iter().map(|r| r["fingerprint"].clone()).collect();
    assert_eq!(fps[0], Value::Null); // the person's mistake, not a fault
    assert_eq!(fps[1], "Panic:t:src/main.rs:12");
    runtime::handle("t", &json!({}), &e.dir, "stable", true, &mut Fake(|_: &Value| Err("Crash: thread 'main' panicked at src\\main.rs:12:5:\nboom".to_string())));
    let windows = store::q("select fingerprint from traces order by ts desc limit 1", params![]).unwrap();
    assert_eq!(windows[0]["fingerprint"], "Panic:t:src/main.rs:12"); // a Windows path gives the same fingerprint
    assert_eq!(fps[2], "BadStep:t"); // an app that never finishes is stopped, not looped forever
    assert_eq!(fps[3], Value::Null);
}

#[test]
fn detect_makes_one_issue_per_fingerprint_and_closes_transient() {
    let _e = env();
    let trace = |id: &str, version: &str, fp: &str| {
        store::x("insert into traces(id,ts,app,version,ok,error,fingerprint,ms) values(?,?,'t',?,0,'e',?,1)", params![id, now(), version, fp]).unwrap();
    };
    for i in 0..5 {
        trace(&format!("a{i}"), "stable", "Panic:t:src/main.rs:9");
    }
    trace("b", "stable", "RouterError:t");
    trace("c", "eval", "Panic:t:src/main.rs:1");
    improve::detect().unwrap();
    improve::detect().unwrap(); // a second pass must not double count
    let rows = store::q("select * from issues order by fingerprint", params![]).unwrap();
    assert_eq!(rows.len(), 2);
    assert_eq!((store::rs(&rows[0], "fingerprint").as_str(), store::ri(&rows[0], "count")), ("Panic:t:src/main.rs:9", 5));
    assert_eq!(store::rs(&rows[1], "status"), "closed");
    let picked = improve::triage(5).unwrap();
    assert_eq!(picked.len(), 1);
    store::x("insert into proposals(app,status) values('t','awaiting')", params![]).unwrap();
    assert!(improve::triage(5).unwrap().is_empty()); // an app with a change in flight waits
}

#[test]
fn rejected_issue_stays_blocked_until_occurrences_double() {
    let _e = env();
    improve::upsert("fp", "error", "t", "title", "d", 5).unwrap();
    store::x("update issues set status='rejected', seen_count=count", params![]).unwrap();
    improve::upsert("fp", "error", "t", "title", "d", 1).unwrap();
    assert_eq!(store::rs(&store::q("select status from issues", params![]).unwrap()[0], "status"), "rejected");
    improve::upsert("fp", "error", "t", "title", "d", 4).unwrap();
    assert_eq!(store::rs(&store::q("select status from issues", params![]).unwrap()[0], "status"), "open");
}

#[test]
fn builders_are_confined_to_their_own_app() {
    for good in ["apps/notes/src/main.rs", "apps/notes/src/parse.rs", "apps/notes/app.json", "apps/notes/evals/golden.jsonl", "prompts/notes_ask.md"] {
        assert_eq!(improve::agent_path("notes", good).unwrap(), good);
    }
    for bad in ["src/store.rs", "apps/docqa/src/main.rs", "config/routing.toml", "apps/notes/../docqa/src/main.rs", "prompts/forge_fix.md",
                ".mcp.json", "apps/notes_two/src/main.rs", "apps/notes/Cargo.toml", "apps/notes/build.rs", "sdk/src/lib.rs"] {
        assert!(improve::agent_path("notes", bad).is_err(), "{bad}");
    }
    assert!(improve::allowed("code", "docqa", &["apps/docqa/src/main.rs".into(), "apps/docqa/evals/issue_7.jsonl".into()], 7).is_none());
    for (channel, path) in [("code", "apps/docqa/evals/golden.jsonl"), ("code", "src/improve.rs"), ("prompt", "prompts/forge_judge.md"), ("routing", "apps/docqa/src/main.rs")] {
        assert!(improve::allowed(channel, "docqa", &[path.to_string()], 7).is_some(), "{path}");
    }
    assert!(improve::cargo_toml("notes").contains("forge-app = { path = \"../../sdk\" }"));
}

#[test]
fn app_source_may_not_reach_outside_itself() {
    assert!(improve::forbidden("use forge_app::{answer, run, Input, Step};\nfn main() { run(app); }").is_none());
    for bad in ["unsafe { x }", "use std::fs;", "std::net::TcpStream::connect(a)", "use std::{collections::HashMap, process::Command};",
                "std :: env :: var(\"HOME\")", "include_str!(\"/etc/passwd\")", "extern \"C\" { fn system(); }", "#![feature(x)]"] {
        assert!(improve::forbidden(bad).is_some(), "{bad}");
    }
}

#[test]
fn parse_reads_cause_and_files_and_rejects_free_text() {
    let (cause, files) = improve::parse("CAUSE: a bug\nFILE: apps/x/src/main.rs\n<<<\nfn main() {}\n>>>\n").unwrap();
    assert_eq!((cause.as_str(), files), ("a bug", vec![("apps/x/src/main.rs".to_string(), "fn main() {}\n".to_string())]));
    assert!(improve::parse("Sure! Here is my fix.").is_err());
    assert_eq!(improve::slots("a {{x}} b {{y}} {{x}}"), vec!["{{x}}", "{{y}}"]);
}

#[test]
fn write_cannot_escape_its_folder() {
    let e = env();
    for bad in ["../outside.txt", "/etc/x", "C:/x", "a/../../b"] {
        assert!(util::write(&e.dir, bad, "x").is_err(), "{bad}");
    }
    util::write(&e.dir, "a/b.txt", "ok").unwrap();
}

#[test]
fn compare_gate() {
    let base = json!({"success": 0.8, "p95_ms": 1000, "usd_per_req": 0.0});
    let with = |k: &str, v: Value| {
        let mut c = base.clone();
        c[k] = v;
        c
    };
    assert!(evals::compare(&base, &base, false, 5000).is_empty());
    assert!(!evals::compare(&base, &base, true, 5000).is_empty()); // equal is not an improvement
    assert!(evals::compare(&base, &with("success", json!(0.9)), true, 5000).is_empty());
    assert!(!evals::compare(&base, &with("success", json!(0.7)), false, 5000).is_empty());
    assert!(!evals::compare(&base, &with("p95_ms", json!(9000)), false, 5000).is_empty());
}

#[test]
fn golden_cases_are_scored_with_every_kind_of_check() {
    let e = env();
    std::fs::create_dir_all(e.dir.join("apps").join("t").join("evals")).unwrap();
    std::fs::write(e.dir.join("apps").join("t").join("evals").join("golden.jsonl"),
        "{\"id\": \"a\", \"input\": {\"input\": \"1\"}, \"answer\": \" ONE \"}\n{\"id\": \"b\", \"input\": {\"input\": \"2\"}, \"must_include\": [\"two\"], \"must_not_include\": [\"one\"]}\n\
         {\"id\": \"c\", \"input\": {\"input\": \"3\"}, \"expect\": {\"answer\": \"three\"}}\n{\"id\": \"d\", \"input\": {\"input\": \"4\"}, \"answer\": \"four\"}\n").unwrap();
    let mut app = Fake(|input: &Value| Ok(json!({"output": {"answer": match input["payload"]["input"].as_str().unwrap() { "1" => "one", "2" => "Two", "3" => "three", _ => "wrong" }}})));
    let card = evals::run("t", &e.dir, &mut app).unwrap();
    assert_eq!((card["n"].clone(), card["success"].clone()), (json!(4), json!(0.75)));
    assert_eq!(card["failed"][0]["id"], "d");
}

#[test]
fn examples_become_fixed_golden_cases() {
    let e = env();
    let cases = builder::parse_examples("2+3*4 -> 14\n\n  sqrt(16) => 4 \nf(x)->y -> z\n10 → ten").unwrap();
    let pairs: Vec<(&str, &str)> = cases.iter().map(|c| (c["input"]["input"].as_str().unwrap(), c["answer"].as_str().unwrap())).collect();
    assert_eq!(pairs, vec![("2+3*4", "14"), ("sqrt(16)", "4"), ("f(x)->y", "z"), ("10", "ten")]);
    assert!(builder::parse_examples("").unwrap().is_empty());
    assert!(builder::parse_examples("no arrow here").unwrap_err().contains("arrow"));
    util::write(&e.dir, "apps/x/evals/golden.jsonl", "{\"id\": \"you-1\", \"input\": {\"input\": \"2+3*4\"}, \"answer\": \"99\"}\n{\"id\": \"mine\", \"input\": {}}\n").unwrap();
    builder::restore_examples(&e.dir, "x", &cases[..1]).unwrap(); // the builder edited the owner's answer: put it back
    let text = util::read(&e.dir.join("apps/x/evals/golden.jsonl")).unwrap();
    let lines: Vec<Value> = text.lines().map(|l| serde_json::from_str(l).unwrap()).collect();
    assert_eq!(lines, vec![json!({"id": "you-1", "input": {"input": "2+3*4"}, "answer": "14"}), json!({"id": "mine", "input": {}})]);
}

#[test]
fn app_names_come_from_the_idea_and_never_clash() {
    let e = env();
    for file in ["src/main.rs", "app.json"] {
        util::write(&e.dir, &format!("apps/slugify/{file}"), "{}").unwrap();
    }
    assert_eq!(builder::name_for("Make me a scientific calculator app", "").unwrap(), "scientific_calculator");
    assert_eq!(builder::name_for("Convert temperatures between Celsius and Fahrenheit", "").unwrap(), "convert_temperatures");
    assert_eq!(builder::name_for("Slugify", "").unwrap(), "slugify_2"); // taken
    assert!(builder::name_for("anything", "slugify").unwrap_err().contains("already exists"));
    assert!(builder::name_for("anything", "Bad Name!").is_err());
}

#[test]
fn failing_trial_is_discarded_and_remembered() {
    let e = env();
    store::x("insert into issues(id,fingerprint,kind,app,title,count,status) values(1,'fp','error','t','t',3,'proposed')", params![]).unwrap();
    store::x("insert into proposals(id,issue_id,app,channel,branch,status,summary) values(1,1,'t','code','proposal/1','canary','s')", params![]).unwrap();
    store::kv_set("canary", &json!({"proposal": 1, "path": e.dir.to_string_lossy(), "pct": 50, "need": 6, "since": 0})).unwrap();
    for i in 0..3 {
        store::x("insert into traces(id,ts,app,version,ok,fingerprint,ms) values(?,?,'t','canary:p1',0,'E:x',1)", params![format!("c{i}"), now()]).unwrap();
    }
    release::check();
    assert_eq!(store::rs(&store::q("select status from proposals", params![]).unwrap()[0], "status"), "rolled_back");
    assert_eq!(store::rs(&store::q("select status from issues", params![]).unwrap()[0], "status"), "hold");
    assert!(store::kv_get("canary").is_null());
    assert_eq!(store::recall("rolled_back", 3).len(), 1);
}

/// The real sandbox: compiles, tests, scores and scans the bundled sample app inside Docker.
/// Run with `cargo test -- --ignored`. It needs Docker and downloads the sandbox and scanner images.
#[test]
#[ignore = "needs Docker"]
fn sandbox_compiles_tests_scores_and_scans_a_real_app() {
    let _e = env();
    let repo = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    assert!(crate::sandbox::available(), "Docker is not running");
    let (ok, out) = crate::sandbox::tests(&repo, "slugify");
    assert!(ok, "unit tests failed in the sandbox:\n{out}");
    let card = crate::sandbox::evals(&repo, "slugify").expect("golden cases ran in the sandbox");
    assert_eq!(card["success"], json!(1.0), "{card}");
    let (ok, out) = crate::sandbox::scans(&repo);
    assert!(ok, "a security scan failed:\n{out}");
}

#[test]
fn mcp_lists_tools_and_turns_failures_into_tool_errors() {
    let _e = env();
    assert!(mcp::handle(&json!({"method": "notifications/initialized"})).is_none());
    assert_eq!(mcp::handle(&json!({"id": 1, "method": "initialize", "params": {}})).unwrap()["result"]["serverInfo"]["name"], "forge");
    let listed = mcp::handle(&json!({"id": 2, "method": "tools/list"})).unwrap();
    let names: Vec<&str> = listed["result"]["tools"].as_array().unwrap().iter().map(|t| t["name"].as_str().unwrap()).collect();
    assert!(names.contains(&"submit_change"));
    assert!(!names.iter().any(|n| ["approve", "release", "merge"].contains(n))); // an agent can never approve its own work
    let bad = mcp::handle(&json!({"id": 3, "method": "tools/call", "params": {"name": "write_file", "arguments": {"change_id": 999, "path": "apps/x/src/main.rs", "content": ""}}})).unwrap();
    assert_eq!(bad["result"]["isError"], true);
    let guide = mcp::handle(&json!({"id": 4, "method": "tools/call", "params": {"name": "guide"}})).unwrap();
    assert!(guide["result"]["content"][0]["text"].as_str().unwrap().contains("forge_app"));
    assert!(mcp::handle(&json!({"id": 5, "method": "nope"})).unwrap()["error"].is_object());
}

#[test]
fn pick_prefers_long_context_then_size_within_gpu_memory() {
    let m = |name: &str, gb: f64, ctx: u64, think: bool| json!({"model": name, "size": gb * 1e9, "ctx": ctx, "thinking": think, "endpoint": "ollama", "base": "x", "key_env": null});
    let found = vec![m("tiny", 2.7, 0, true), m("big12", 7.6, 0, false), m("long9", 6.6, 65536, true), m("plain9", 6.6, 0, true), m("huge", 40.0, 0, false)];
    let roles = models::pick(&found, Some(11.0));
    let chosen: Vec<(&str, &str)> = roles.iter().map(|(r, v)| (r.as_str(), v["model"].as_str().unwrap())).collect();
    assert_eq!(chosen, vec![("main", "long9"), ("think", "long9"), ("fast", "tiny"), ("big", "big12")]);
    let cfg: Value = toml::from_str(&models::render(&roles, "claude")).unwrap();
    assert_eq!(cfg["builder"]["kind"], "claude");
    assert_eq!(cfg["models"]["think"]["extra"], json!({"reasoning_effort": "medium"}));
    assert!(cfg["models"]["big"].get("extra").is_none()); // never send reasoning options to a model without them
    assert!(models::pick(&[], None).is_empty());
}
