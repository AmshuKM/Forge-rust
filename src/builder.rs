//! Idea in, working app out: Forge drives the chosen builder (Claude Code or the local model) through its gates.
//! A new app built from the owner's own idea goes live as soon as it passes every gate.
use crate::store::{self, params, rs};
use crate::util::{clock_s, head, now, read, root, tail, which, write, Sx, R};
use crate::{improve, mcp, release, router, runtime, sandbox};
use regex::Regex;
use serde_json::{json, Value};
use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};

static BUILDING: AtomicBool = AtomicBool::new(false); // one build at a time: they compete for the GPU and the sandbox
const CLAUDE_TOOLS: &str = "mcp__forge__guide,mcp__forge__read_file,mcp__forge__write_file,mcp__forge__check_change";
const STOP_WORDS: [&str; 30] = ["a", "an", "the", "app", "that", "which", "make", "build", "create", "me", "for", "to", "and",
                                "with", "i", "want", "please", "can", "you", "of", "my", "is", "it", "this", "some", "any",
                                "tool", "program", "simple", "given"];
// Words that start the details ("count words IN a text"): a name with two words already ends before one.
const JOINERS: [&str; 18] = ["in", "on", "at", "by", "from", "into", "between", "using", "as", "when", "if", "so", "then", "or",
                             "but", "per", "via", "where"];

struct Done;

impl Drop for Done {
    fn drop(&mut self) {
        BUILDING.store(false, Ordering::SeqCst);
    }
}

/// A free app name: the one asked for, or one made from the idea's first words.
pub fn name_for(idea: &str, wanted: &str) -> R<String> {
    let taken = |name: &str| runtime::apps(&root()).iter().any(|a| a == name)
        || store::q("select 1 from proposals where app=? and status='testing'", params![name]).map(|r| !r.is_empty()).unwrap_or(false);
    if !wanted.trim().is_empty() {
        let name = wanted.trim().to_lowercase();
        if !runtime::valid_name(&name) {
            return Err("Names use lowercase letters, digits and underscores, start with a letter, 2 to 31 characters.".to_string());
        }
        if runtime::apps(&root()).contains(&name) {
            return Err(format!("An app called {name} already exists. Pick another name."));
        }
        return Ok(name);
    }
    let lower = idea.to_lowercase();
    let mut words: Vec<&str> = Vec::new();
    for w in lower.split(|c: char| !c.is_ascii_alphanumeric()).filter(|w| w.starts_with(|c: char| c.is_ascii_lowercase())) {
        if JOINERS.contains(&w) {
            if words.len() >= 2 {
                break;
            }
        } else if !STOP_WORDS.contains(&w) {
            words.push(w);
            if words.len() == 3 {
                break;
            }
        }
    }
    let mut base = head(words.first().copied().unwrap_or("app"), 24);
    for w in words.iter().skip(1) {
        // whole words only, so names never end mid-word
        if base.len() + 1 + w.len() > 24 {
            break;
        }
        base = format!("{base}_{w}");
    }
    if base.len() < 2 {
        base = format!("app_{base}");
    }
    let (mut name, mut n) = (base.clone(), 2);
    while taken(&name) {
        name = format!("{base}_{n}");
        n += 1;
    }
    Ok(name)
}

/// "input -> expected answer" per line. These become golden cases the builder may not change.
pub fn parse_examples(text: &str) -> R<Vec<Value>> {
    let arrow = Regex::new(r"\s*(?:→|=>|->)\s*").expect("valid pattern");
    let mut cases = Vec::new();
    for line in text.lines().map(str::trim).filter(|l| !l.is_empty()) {
        let (input, answer) = match arrow.find_iter(line).last() {
            Some(m) => (line[..m.start()].trim(), line[m.end()..].trim()),
            None => ("", ""),
        };
        if input.is_empty() || answer.is_empty() {
            return Err(format!("Example '{}' needs an arrow between input and answer, like: 2+3*4 -> 14", head(line, 60)));
        }
        cases.push(json!({"id": format!("you-{}", cases.len() + 1), "input": {"input": input}, "answer": answer}));
    }
    if cases.len() > 50 {
        return Err("Please give at most 50 examples.".to_string());
    }
    Ok(cases)
}

/// Put the owner's examples back exactly as given, whatever the builder did to them.
pub fn restore_examples(wt: &Path, name: &str, cases: &[Value]) -> R<()> {
    let rel = format!("apps/{name}/evals/golden.jsonl");
    let mut lines: Vec<String> = cases.iter().map(|c| c.to_string()).collect();
    for line in read(&wt.join(&rel)).unwrap_or_default().lines().filter(|l| !l.trim().is_empty()) {
        // a malformed line is kept: it fails the evals and is reported to the builder
        let mine = serde_json::from_str::<Value>(line).ok().is_some_and(|c| c["id"].as_str().is_some_and(|id| id.starts_with("you-")));
        if !mine {
            lines.push(line.to_string());
        }
    }
    if lines.is_empty() {
        return Ok(());
    }
    write(wt, &rel, &format!("{}\n", lines.join("\n")))
}

/// Who builds: the choice saved at setup, else Claude Code when installed, else the local model.
pub fn kind() -> String {
    let chosen = router::config(&root()).ok().and_then(|c| c["builder"]["kind"].as_str().map(String::from)).unwrap_or_default();
    if chosen == "claude" || chosen == "local" {
        return chosen;
    }
    if which("claude").is_some() { "claude".to_string() } else { "local".to_string() }
}

pub fn log(pid: i64, text: &str) {
    let key = format!("build:{pid}");
    let mut lines = store::kv_get(&key).as_array().cloned().unwrap_or_default();
    lines.push(json!(format!("{}  {text}", clock_s(now()))));
    let _ = store::kv_set(&key, &json!(lines));
}

fn examples_note(name: &str, cases: &[Value]) -> String {
    if cases.is_empty() {
        return String::new();
    }
    let shown: Vec<String> = cases.iter().map(|c| format!("  {}  ->  {}", c["input"]["input"].as_str().unwrap_or(""), c["answer"].as_str().unwrap_or(""))).collect();
    format!("The owner gave examples the app must get exactly right:\n{}\nThey are already in apps/{name}/evals/golden.jsonl with ids starting 'you-'; \
             keep them unchanged. The app's main form field must be named \"input\", and the answer must be exactly the answer text shown.", shown.join("\n"))
}

fn claude_round(pid: i64, name: &str, idea: &str, cases: &[Value], failure: &str) -> R<String> {
    let claude = which("claude").ok_or("Claude Code is not installed")?;
    let exe = std::env::current_exe().s()?;
    // Claude Code starts this same program as its tool server, so the builder works without cargo on its PATH
    let config = root().join(".forge").join("mcp.json");
    write(&root(), ".forge/mcp.json", &json!({"mcpServers": {"forge": {"command": exe.to_string_lossy(), "args": ["mcp"]}}}).to_string())?;
    let mut prompt = format!(
        "Build a new app for Forge. Change {pid} is already open for the app '{name}'. Call the forge `guide` tool first, then use only \
         read_file, write_file and check_change with change_id {pid}. Run check_change until it passes; Forge submits the change itself \
         when you finish.\n\nThe owner's request:\n<request>\n{idea}\n</request>\n\n{}\n\n", examples_note(name, cases));
    if !failure.is_empty() {
        prompt.push_str(&format!("Your previous attempt failed Forge's gates:\n{failure}\nFix that.\n\n"));
    }
    prompt.push_str("Begin your final reply with one sentence saying what you built.");
    let mut child = Command::new(claude)
        .args(["-p", "--mcp-config"]).arg(&config)
        .args(["--strict-mcp-config", "--allowedTools", CLAUDE_TOOLS, "--disallowedTools", "Bash,Edit,Write,NotebookEdit", "--output-format", "json"])
        .current_dir(root()).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn()
        .map_err(|e| format!("Claude Code could not be started: {e}"))?;
    if let Some(mut stdin) = child.stdin.take() {
        let _ = stdin.write_all(prompt.as_bytes());
    }
    let deadline = now() + 1800.0;
    while child.try_wait().s()?.is_none() {
        if now() > deadline {
            let _ = child.kill();
            let _ = child.wait();
            return Err("Claude Code did not finish within 30 minutes".to_string());
        }
        std::thread::sleep(std::time::Duration::from_millis(200));
    }
    let out = child.wait_with_output().s()?;
    let result: Value = serde_json::from_str(String::from_utf8_lossy(&out.stdout).trim()).unwrap_or(json!({}));
    let text = result["result"].as_str().unwrap_or("").trim().to_string();
    if result["is_error"] == true || text.is_empty() || text.to_lowercase().contains("authenticate") {
        let why = if text.is_empty() { tail(String::from_utf8_lossy(&out.stderr).trim(), 300) } else { head(&text, 300) };
        return Err(format!("Claude Code failed: {why}"));
    }
    log(pid, &format!("Claude Code finished ({} steps, about ${:.2} of your Claude plan or credit).", result["num_turns"], result["total_cost_usd"].as_f64().unwrap_or(0.0)));
    // the opening line says what was built
    Ok(head(text.lines().find(|l| !l.trim().is_empty()).unwrap_or("").trim_matches(|c: char| c == ' ' || c == '#' || c == '*'), 200))
}

fn local_round(pid: i64, wt: &Path, name: &str, idea: &str, cases: &[Value], failure: &str) -> R<String> {
    let (mut summary, mut failure) = (format!("New app {name}"), failure.to_string());
    for n in 0..2 {
        // two drafts per round, each checked in the sandbox
        let mut files = String::new();
        for rel in [format!("apps/{name}/src/main.rs"), format!("apps/{name}/app.json"), format!("apps/{name}/evals/golden.jsonl")] {
            if let Ok(text) = read(&wt.join(&rel)) {
                files.push_str(&format!("FILE: {rel}\n<<<\n{text}\n>>>\n\n"));
            }
        }
        let prompt = runtime::render(&root(), "forge_build", &json!({
            "name": name, "idea": idea, "guide": mcp::GUIDE, "examples": examples_note(name, cases),
            "files": if files.is_empty() { "none yet".to_string() } else { files },
            "failure": if failure.is_empty() { String::new() } else { format!("The last draft failed:\n{failure}\nFix that.") }}))?;
        log(pid, &format!("Local model is writing draft {}...", n + 1));
        let (cause, written) = improve::parse(&router::call("develop", &json!([{"role": "user", "content": prompt}]), &root(), "", false)?)?;
        summary = cause;
        for (path, text) in &written {
            write(wt, &improve::agent_path(name, path)?, text)?;
        }
        restore_examples(wt, name, cases)?;
        let (ok, out) = sandbox::tests(wt, name);
        if ok {
            match sandbox::evals(wt, name) {
                Ok(card) if card["failed"].as_array().is_some_and(|f| f.is_empty()) => return Ok(summary),
                Ok(card) => failure = format!("golden cases fail: {}", head(&card["failed"].to_string(), 2000)),
                Err(e) => failure = e,
            }
        } else {
            failure = format!("unit tests failed:\n{}", tail(&out, 2000));
        }
        log(pid, if n == 0 { "Draft failed the checks; trying again." } else { "Draft failed the checks." });
    }
    Ok(summary)
}

fn status(pid: i64) -> String {
    store::q("select status from proposals where id=?", params![pid]).ok().and_then(|r| r.first().map(|r| rs(r, "status"))).unwrap_or_default()
}

fn run(pid: i64, name: &str, idea: &str, cases: &[Value]) -> R<()> {
    let wt = improve::worktree(pid);
    let (mut failure, mut builder) = (String::new(), kind());
    let limit = router::config(&root())?["budget"]["max_attempts"].as_i64().unwrap_or(3);
    for attempt in 1..=limit {
        // bounded: then the owner decides
        log(pid, &format!("Attempt {attempt}: {} is building...", if builder == "claude" { "Claude Code" } else { "the local model" }));
        store::kv_set(&format!("stages:{pid}"), &json!({}))?; // a fresh board for each attempt
        store::kv_set(&format!("attempt:{pid}"), &json!(attempt))?;
        store::stage(pid, "write", None);
        let mut round = if builder == "claude" { claude_round(pid, name, idea, cases, &failure) } else { local_round(pid, &wt, name, idea, cases, &failure) };
        if let (Err(e), true) = (&round, builder == "claude") {
            log(pid, &format!("{e} Switching to the local model."));
            builder = "local".to_string();
            round = local_round(pid, &wt, name, idea, cases, &failure);
        }
        store::stage(pid, "write", Some(round.is_ok()));
        let summary = round?;
        restore_examples(&wt, name, cases)?;
        log(pid, "Running every gate: unit tests, security scans, golden cases...");
        let message = improve::submit_change(pid, &summary)?;
        match status(pid).as_str() {
            "awaiting" => {
                store::stage(pid, "live", None);
                release::approve(pid)?; // the owner's own idea and every gate passed: it goes live
                store::stage(pid, "live", Some(status(pid) == "shipped"));
                log(pid, "Passed every gate. Your app is live.");
                return Ok(());
            }
            "testing" => {
                log(pid, message.lines().next().unwrap_or(""));
                failure = message;
            }
            _ => {
                log(pid, message.lines().next().unwrap_or(""));
                return Ok(());
            }
        }
    }
    Ok(())
}

/// Validate, open the change, and build in the background. Returns the change number.
pub fn start(idea: &str, wanted_name: &str, examples: &str) -> R<i64> {
    let idea = idea.trim().to_string();
    if idea.chars().count() < 10 {
        return Err("Describe the app in a sentence or more.".to_string());
    }
    if idea.chars().count() > 4000 {
        return Err("Please keep the description under 4,000 characters.".to_string());
    }
    let cases = parse_examples(examples)?;
    let name = name_for(&idea, wanted_name)?;
    if !sandbox::available() {
        return Err("Docker is not running, so nothing can be tested. Start Docker Desktop and try again.".to_string());
    }
    if BUILDING.compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst).is_err() {
        return Err("Another app is being built. Wait for it to finish.".to_string());
    }
    let done = Done;
    let (pid, wt) = improve::start_change(&name, None)?;
    restore_examples(&wt, &name, &cases)?;
    store::kv_set(&format!("idea:{pid}"), &json!(idea))?;
    log(pid, &if cases.is_empty() { format!("Building '{name}'.") } else { format!("Building '{name}' with {} example(s) it must get right.", cases.len()) });
    std::thread::spawn(move || {
        let _done = done;
        if let Err(e) = run(pid, &name, &idea, &cases) {
            // a crash must leave a clear record, never a half-open change
            log(pid, &format!("Stopped: {e}"));
            if let Some(mut p) = store::q("select * from proposals where id=? and status='testing'", params![pid]).ok().and_then(|mut r| if r.is_empty() { None } else { Some(r.remove(0)) }) {
                p.insert("summary".to_string(), json!(format!("Build of {name}")));
                release::close(&p, "escalated", "hold", &format!("Build stopped: {}", head(&e, 500)));
            }
        }
    });
    Ok(pid)
}
