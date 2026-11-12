//! Runtime plane: run one request through an app, step by step, and record the trace.
//! Nothing here changes code, prompts or routing.
use crate::store::{self, params};
use crate::util::{hash, head, now, random_hex, read, root, tail, Sx, R};
use crate::router;
use regex::Regex;
use serde_json::{json, Value};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

pub const RETRIES: usize = 2; // extra model calls after an unacceptable reply, then a safe error
pub const SAFE_ERROR: &str = "Sorry, something went wrong with that request. It has been logged.";
const MAX_INPUT_CHARS: usize = 200_000;
const MAX_STEPS: usize = 12; // an app that never finishes is stopped here

pub fn valid_name(app: &str) -> bool {
    (2..=31).contains(&app.len()) && app.starts_with(|c: char| c.is_ascii_lowercase())
        && app.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
}

/// Apps present in a project folder: `apps/<name>/` with a program and a manifest.
pub fn apps(at: &Path) -> Vec<String> {
    let mut found: Vec<String> = std::fs::read_dir(at.join("apps")).into_iter().flatten().flatten()
        .filter(|e| e.path().join("src").join("main.rs").is_file() && e.path().join("app.json").is_file())
        .filter_map(|e| e.file_name().into_string().ok()).filter(|n| valid_name(n)).collect();
    found.sort();
    found
}

/// `apps/<app>/app.json`: the description and form fields. Read as data; the app is never run to describe itself.
pub fn manifest(at: &Path, app: &str) -> Value {
    read(&at.join("apps").join(app).join("app.json")).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or(json!({}))
}

fn text_of(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Null => String::new(),
        other => other.to_string(),
    }
}

/// Fill `{{name}}` slots in one pass, so text inside a value is never treated as a slot.
pub fn render(at: &Path, prompt: &str, vars: &Value) -> R<String> {
    if prompt.is_empty() || !prompt.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-') {
        return Err(format!("'{prompt}' is not a valid prompt name"));
    }
    let limit = router::config(at)?["budget"]["context_chars"].as_u64().unwrap_or(24000) as usize;
    let template = read(&at.join("prompts").join(format!("{prompt}.md")))?;
    let slot = Regex::new(r"\{\{(\w+)\}\}").expect("valid pattern");
    Ok(slot.replace_all(&template, |c: &regex::Captures| head(&text_of(&vars[&c[1]]), limit)).into_owned())
}

/// Runs one step of an app: input in, step out. The host runs the built program; the sandbox runs it in Docker.
pub trait Invoker {
    fn invoke(&mut self, input: &Value) -> R<Value>;
}

pub struct Host(pub PathBuf);

impl Invoker for Host {
    fn invoke(&mut self, input: &Value) -> R<Value> {
        let mut child = Command::new(&self.0).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn()
            .map_err(|e| format!("Crash: the app could not be started: {e}"))?;
        if let Some(mut stdin) = child.stdin.take() {
            let _ = stdin.write_all(input.to_string().as_bytes());
        }
        // shortcut: output is read after exit, so an app printing more than a pipe holds waits out the timeout
        let deadline = now() + 10.0;
        while child.try_wait().s()?.is_none() {
            if now() > deadline {
                let _ = child.kill();
                let _ = child.wait();
                return Err("Timeout: the app took longer than 10 seconds".to_string());
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        let out = child.wait_with_output().s()?;
        if !out.status.success() {
            return Err(format!("Crash: {}", tail(String::from_utf8_lossy(&out.stderr).trim(), 800)));
        }
        serde_json::from_str(String::from_utf8_lossy(&out.stdout).trim()).map_err(|_| "BadStep: the app printed something that is not a step".to_string())
    }
}

/// An invoker for an app that could not be built: every request fails with the reason.
pub struct Broken(pub String);

impl Invoker for Broken {
    fn invoke(&mut self, _input: &Value) -> R<Value> {
        Err(self.0.clone())
    }
}

/// Compile an app on this computer (only apps that passed the gates are built here) and return its program.
pub fn build_app(at: &Path, app: &str) -> R<PathBuf> {
    if !valid_name(app) {
        return Err("Unknown app.".to_string());
    }
    let slot = if at == root() { "stable".to_string() } else { hash(&at.to_string_lossy()) };
    let target = root().join(".forge").join("target").join(slot);
    let bin = target.join("release").join(format!("{app}{}", std::env::consts::EXE_SUFFIX));
    let dir = at.join("apps").join(app);
    let modified = |p: &Path| p.metadata().and_then(|m| m.modified()).ok();
    let mut sources = vec![dir.join("Cargo.toml"), at.join("sdk").join("src").join("lib.rs")];
    sources.extend(std::fs::read_dir(dir.join("src")).into_iter().flatten().flatten().map(|e| e.path()));
    let newest = sources.iter().filter_map(|p| modified(p)).max();
    if let (Some(built), Some(newest)) = (modified(&bin), newest) {
        if built >= newest {
            return Ok(bin);
        }
    }
    let out = Command::new("cargo").args(["build", "--release", "--offline", "--quiet", "--manifest-path"])
        .arg(dir.join("Cargo.toml")).arg("--target-dir").arg(&target).output()
        .map_err(|e| format!("Crash: cargo could not be started: {e}"))?;
    if !out.status.success() {
        return Err(format!("Crash: the app does not compile:\n{}", tail(String::from_utf8_lossy(&out.stderr).trim(), 1500)));
    }
    Ok(bin)
}

fn fingerprint(app: &str, err: &str) -> String {
    if err.starts_with("RouterError") {
        return format!("RouterError:{app}");
    }
    if let Some(c) = Regex::new(r"panicked at ([^:\s]+:\d+)").expect("valid pattern").captures(err) {
        return format!("Panic:{app}:{}", c[1].replace('\\', "/")); // the same crash must read the same on Windows and in the sandbox
    }
    let kind = err.split(':').next().unwrap_or("");
    format!("{}:{app}", if ["Timeout", "BadStep"].contains(&kind) { kind } else { "Crash" })
}

/// Which copy of the project serves this request: the released one, or an approved change on trial.
pub fn pick_root() -> (PathBuf, String) {
    let canary = store::kv_get("canary");
    if canary.is_object() {
        let mut byte = [0u8; 1];
        let _ = getrandom::getrandom(&mut byte);
        if (byte[0] as f64) * 100.0 / 256.0 < canary["pct"].as_f64().unwrap_or(0.0) {
            return (PathBuf::from(canary["path"].as_str().unwrap_or("")), format!("canary:p{}", canary["proposal"]));
        }
    }
    (root(), "stable".to_string())
}

/// Run one request through an app and record it. Always returns a result, never an error.
pub fn handle(app: &str, payload: &Value, at: &Path, version: &str, cache: bool, inv: &mut dyn Invoker) -> Value {
    let (trace_id, t0) = (random_hex(8), now());
    let (mut ok, mut output, mut error, mut fp, mut shown) = (false, Value::Null, None::<String>, None::<String>, SAFE_ERROR.to_string());
    if !payload.is_object() || payload.to_string().len() > MAX_INPUT_CHARS {
        shown = "That input is too large.".to_string();
        error = Some(shown.clone());
    } else {
        let (mut hint, mut reply, mut asked) = (String::new(), Value::Null, 0usize);
        for step_no in 0.. {
            let fail = |why: String| (Some(why.clone()), Some(fingerprint(app, &why)));
            if step_no >= MAX_STEPS {
                (error, fp) = (Some("BadStep: the app did not finish".to_string()), Some(format!("BadStep:{app}")));
                break;
            }
            let step = match inv.invoke(&json!({"payload": payload, "reply": reply})) {
                Ok(step) => step,
                Err(e) => {
                    (error, fp) = fail(e);
                    break;
                }
            };
            if let Some(message) = step["error"].as_str() {
                // the person's input was not acceptable: shown to them, not a system fault
                shown = message.to_string();
                error = Some(shown.clone());
                break;
            }
            if step["output"].is_object() {
                ok = true;
                output = step["output"].clone();
                break;
            }
            if let Some(reason) = step["invalid"].as_str() {
                hint = reason.to_string();
                reply = Value::Null;
                if asked > RETRIES {
                    let stable: String = Regex::new(r"\d+").expect("valid pattern").replace_all(&hint, "N").into_owned();
                    error = Some(format!("OutputInvalid: {hint}"));
                    fp = Some(format!("OutputInvalid:{app}:{}", head(&stable, 60)));
                    break;
                }
                continue;
            }
            let ask = &step["llm"];
            let (Some(task), Some(prompt)) = (ask["task"].as_str(), ask["prompt"].as_str()) else {
                (error, fp) = (Some("BadStep: the app returned a step Forge does not know".to_string()), Some(format!("BadStep:{app}")));
                break;
            };
            if asked > RETRIES {
                (error, fp) = (Some("BadStep: the app kept asking for a model".to_string()), Some(format!("BadStep:{app}")));
                break;
            }
            asked += 1;
            let text = render(at, prompt, &ask["vars"]).map(|mut text| {
                if !hint.is_empty() {
                    text.push_str(&format!("\n\nYour previous answer was rejected because {hint}. Correct that."));
                }
                text
            });
            let called = text.and_then(|text| router::call(task, &json!([{"role": "user", "content": text}]), at, &trace_id, cache && hint.is_empty()));
            match called {
                Ok(text) => reply = json!(text),
                Err(e) => {
                    (error, fp) = fail(if e.starts_with("RouterError") { e } else { format!("Crash: {e}") });
                    break;
                }
            }
        }
    }
    let ms = ((now() - t0) * 1000.0) as i64;
    let _ = store::x("insert into traces(id,ts,app,version,ok,error,fingerprint,ms,input,output) values(?,?,?,?,?,?,?,?,?,?)",
        params![trace_id, now(), app, version, ok as i64, error.as_deref().map(|e| tail(e, 4000)), fp,
                ms, head(&payload.to_string(), 4000), head(&output.to_string(), 4000)]);
    let usd = store::scalar("select coalesce(sum(usd),0) from calls where trace_id=?", params![trace_id]).unwrap_or(0.0);
    json!({"trace_id": trace_id, "ok": ok, "output": if ok { output } else { Value::Null },
           "error": if ok { Value::Null } else { json!(shown) }, "ms": ms, "usd": usd, "version": version})
}

/// A live request from the dashboard: picks the copy to serve, builds it if needed, runs it.
pub fn serve(app: &str, payload: &Value) -> Value {
    let (at, version) = pick_root();
    let result = match build_app(&at, app) {
        Ok(bin) => handle(app, payload, &at, &version, true, &mut Host(bin)),
        Err(e) => handle(app, payload, &at, &version, true, &mut Broken(e)),
    };
    if version.starts_with("canary") {
        crate::release::check();
    }
    result
}
