//! Find local model servers and their models, pick one per role, and write config/local.toml.
use crate::util::{which, Sx, R};
use crate::router;
use serde_json::{json, Value};
use std::io::Write;
use std::time::Duration;

const SERVERS: [(&str, &str); 5] = [
    // the usual local addresses of OpenAI-compatible model servers
    ("ollama", "http://localhost:11434"),
    ("lmstudio", "http://localhost:1234"),
    ("llamacpp", "http://localhost:8080"),
    ("vllm", "http://localhost:8000"),
    ("unsloth", "http://localhost:8888"),
];
const SKIP: [&str; 5] = ["embed", "rerank", "whisper", "tts", "cloud"];
const LONG: u64 = 16384; // prompts carry whole documents, so a short context window silently cuts them off
const ROLES: [(&str, &str); 4] = [
    ("main", "everyday answers"),
    ("think", "writing fixes and judging answers (a reasoning model helps)"),
    ("fast", "quick small jobs"),
    ("big", "backup when the main model fails"),
];

/// Servers that need a key; the key itself stays in the environment.
fn key_env(server: &str) -> Option<&'static str> {
    (server == "unsloth").then_some("FORGE_UNSLOTH_KEY")
}

fn get(url: &str, key: Option<&str>, body: Option<Value>) -> Result<Value, ureq::Error> {
    let mut req = if body.is_some() { ureq::post(url) } else { ureq::get(url) }.timeout(Duration::from_secs(5));
    if let Some(key) = key {
        req = req.set("Authorization", &format!("Bearer {key}"));
    }
    let response = match body {
        Some(b) => req.send_json(b)?,
        None => req.call()?,
    };
    Ok(response.into_json::<Value>().unwrap_or(Value::Null))
}

fn skipped(name: &str) -> bool {
    let lower = name.to_lowercase();
    SKIP.iter().any(|s| lower.contains(s))
}

fn ollama(base: &str) -> Result<Vec<Value>, ureq::Error> {
    let mut found = Vec::new();
    for t in get(&format!("{base}/api/tags"), None, None)?["models"].as_array().cloned().unwrap_or_default() {
        let name = t["name"].as_str().unwrap_or("").to_string();
        let size = t["size"].as_u64().unwrap_or(0);
        if skipped(&name) || size < 100_000_000 {
            continue; // cloud stubs are tiny
        }
        let info = get(&format!("{base}/api/show"), None, Some(json!({"model": name}))).unwrap_or(Value::Null);
        let caps: Vec<String> = info["capabilities"].as_array().or(t["capabilities"].as_array())
            .map(|a| a.iter().filter_map(|c| c.as_str().map(String::from)).collect()).unwrap_or_else(|| vec!["completion".to_string()]);
        let ctx = info["parameters"].as_str().and_then(|p| p.lines().find(|l| l.trim_start().starts_with("num_ctx")))
            .and_then(|l| l.split_whitespace().nth(1)).and_then(|n| n.parse::<u64>().ok()).unwrap_or(0);
        if caps.iter().any(|c| c == "completion") {
            found.push(json!({"model": name, "size": size, "thinking": caps.iter().any(|c| c == "thinking"), "ctx": ctx}));
        }
    }
    Ok(found)
}

/// Models found, and notes for the person (for example a server that needs a key).
pub fn detect() -> (Vec<Value>, Vec<String>) {
    let (mut found, mut notes) = (Vec::new(), Vec::new());
    for (name, base) in SERVERS {
        let key = key_env(name).and_then(|v| std::env::var(v).ok());
        let models = if name == "ollama" {
            ollama(base)
        } else {
            get(&format!("{base}/v1/models"), key.as_deref(), None).map(|r| {
                r["data"].as_array().into_iter().flatten().filter_map(|m| m["id"].as_str()).filter(|id| !skipped(id))
                    .map(|id| json!({"model": id, "size": 0, "thinking": false, "ctx": 0})).collect()
            })
        };
        match models {
            Ok(models) => {
                for mut m in models {
                    m["endpoint"] = json!(name);
                    m["base"] = json!(format!("{base}/v1"));
                    m["key_env"] = json!(key_env(name));
                    found.push(m);
                }
            }
            Err(ureq::Error::Status(401 | 403, _)) => notes.push(format!(
                "Found {name} at {base}, but it needs an API key. Set {} and run setup again.", key_env(name).unwrap_or("its key"))),
            Err(_) => {} // nothing listening there, or not a model server
        }
    }
    (found, notes)
}

pub fn vram_gb() -> Option<f64> {
    which("nvidia-smi")?;
    let out = std::process::Command::new("nvidia-smi").args(["--query-gpu=memory.total", "--format=csv,noheader,nounits"]).output().ok()?;
    String::from_utf8_lossy(&out.stdout).split_whitespace().filter_map(|x| x.parse::<f64>().ok()).fold(None, |a: Option<f64>, b| Some(a.map_or(b, |a| a.max(b)))).map(|mb| mb / 1024.0)
}

/// Choose a model for each role: long context first, then the largest that fits in GPU memory.
pub fn pick(models: &[Value], vram: Option<f64>) -> Vec<(String, Value)> {
    if models.is_empty() {
        return Vec::new();
    }
    let limit = vram.unwrap_or(8.0) * 0.9e9;
    let size = |m: &Value| m["size"].as_f64().unwrap_or(0.0);
    let mut fits: Vec<&Value> = models.iter().filter(|m| size(m) <= limit).collect();
    if fits.is_empty() {
        fits = models.iter().collect();
    }
    let rank = |m: &Value| (m["ctx"].as_u64().unwrap_or(0) >= LONG, size(m) as u64);
    let best = |list: &[&Value]| (*list.iter().max_by_key(|m| rank(m)).expect("list is not empty")).clone();
    let main = best(&fits);
    let thinkers: Vec<&Value> = fits.iter().copied().filter(|m| m["thinking"] == true).collect();
    let think = if thinkers.is_empty() { main.clone() } else { best(&thinkers) };
    let fast = (*fits.iter().min_by_key(|m| size(m) as u64).expect("list is not empty")).clone();
    let big = (*fits.iter().max_by_key(|m| size(m) as u64).expect("list is not empty")).clone();
    vec![("main".into(), main), ("think".into(), think), ("fast".into(), fast), ("big".into(), big)]
}

pub fn render(roles: &[(String, Value)], builder: &str) -> String {
    let mut out = vec![
        "# This computer's models. Written by: forge setup".to_string(),
        "# Not shared through Git. Edit a model line by hand, or run setup again to choose.".to_string(), String::new(),
        "[builder]".to_string(), format!("kind = {}   # claude = Claude Code, local = the think model below", json!(builder)), String::new(),
    ];
    let mut seen: Vec<&str> = Vec::new();
    for (_, m) in roles {
        let name = m["endpoint"].as_str().unwrap_or("");
        if seen.contains(&name) {
            continue;
        }
        seen.push(name);
        out.push(format!("[endpoints.{name}]"));
        out.push(format!("base = {}", m["base"]));
        if m["key_env"].is_string() {
            out.push(format!("key_env = {}", m["key_env"]));
        }
        out.push(String::new());
    }
    for (role, m) in roles {
        out.push(format!("[models.{role}]"));
        out.push(format!("endpoint = {}", m["endpoint"]));
        out.push(format!("model = {}", m["model"]));
        if m["thinking"] == true {
            // reasoning is slow: only the role that writes fixes and judges gets it
            out.push(format!("extra = {{ reasoning_effort = \"{}\" }}", if role == "think" { "medium" } else { "none" }));
        }
        if role == "think" {
            out.push("timeout_s = 900".to_string());
        }
        out.push(String::new());
    }
    out.join("\n")
}

pub fn describe(m: &Value) -> String {
    let size = m["size"].as_f64().filter(|s| *s > 0.0).map(|s| format!(", {:.1} GB", s / 1e9)).unwrap_or_default();
    let ctx = m["ctx"].as_u64().filter(|c| *c > 0).map(|c| format!(", {}k context", c / 1024)).unwrap_or_default();
    format!("{} ({}{size}{ctx})", m["model"].as_str().unwrap_or(""), m["endpoint"].as_str().unwrap_or(""))
}

fn ask(prompt: &str) -> String {
    print!("{prompt}");
    let _ = std::io::stdout().flush();
    let mut line = String::new();
    let _ = std::io::stdin().read_line(&mut line);
    line.trim().to_string()
}

fn choice(answer: &str, count: usize) -> Option<usize> {
    answer.parse::<usize>().ok().filter(|n| (1..=count).contains(n)).map(|n| n - 1)
}

/// Detect models, optionally let the person choose, and save the result. Returns false when nothing usable was found.
pub fn setup(interactive: bool) -> R<bool> {
    let (models, notes) = detect();
    for n in &notes {
        println!("{n}");
    }
    let builders: Vec<&str> = if which("claude").is_some() { vec!["claude", "local"] } else { vec!["local"] };
    if models.is_empty() && builders == ["local"] {
        println!("No local model server and no Claude Code were found. Install either one (Ollama, LM Studio,\nllama.cpp, vLLM, Unsloth, or Claude Code) and run forge setup again.\nApps that use no model still work without one.");
        return Ok(false);
    }
    if models.is_empty() {
        println!("No local model server was found, so apps that call a model will not work. Claude Code can still build apps.");
    }
    let mut roles = pick(&models, vram_gb());
    if interactive && !models.is_empty() {
        println!("\nModels found:");
        for (i, m) in models.iter().enumerate() {
            println!("  {}. {}", i + 1, describe(m));
        }
        println!("\nPress Enter to keep the suggestion, or type a number.");
        for (role, why) in ROLES {
            let slot = roles.iter_mut().find(|(r, _)| r == role).expect("every role was picked");
            let default = models.iter().position(|m| m == &slot.1).unwrap_or(0) + 1;
            let answer = ask(&format!("  {role}: {why} [{default}]: "));
            match choice(&answer, models.len()) {
                Some(i) => slot.1 = models[i].clone(),
                None if !answer.is_empty() => println!("    Not a number from the list, so keeping {default}."),
                None => {}
            }
        }
    }
    let mut builder = builders[0];
    if interactive && builders.len() > 1 {
        println!("\nWho should build apps from your ideas?");
        println!("  1. Claude Code (best results; uses your Claude plan or API credit)");
        println!("  2. your local model (free; slower, and small models fail more builds)");
        if let Some(i) = choice(&ask("  Builder [1]: "), builders.len()) {
            builder = builders[i];
        }
    }
    let path = router::local_path();
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).s()?;
    }
    std::fs::write(&path, render(&roles, builder)).s()?;
    println!("\nSaved to {}:", path.display());
    println!("  builder {}", if builder == "claude" { "Claude Code" } else { "local model" });
    for (role, m) in &roles {
        println!("  {role:<7} {}", describe(m));
    }
    Ok(true)
}
