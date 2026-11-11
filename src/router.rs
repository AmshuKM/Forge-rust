//! Model router: task type -> model chain, with fallback, a daily spend cap, a cache and call logging.
//! Talks to any OpenAI-compatible endpoint (Ollama, Unsloth, LM Studio, cloud APIs).
use crate::store::{self, params};
use crate::util::{hash, now, read, root, today, Sx, R};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::Duration;

/// This computer's models. Never in Git, so it always comes from the main folder, not a change's copy.
pub fn local_path() -> PathBuf {
    std::env::var("FORGE_LOCAL_CONFIG").map(PathBuf::from).unwrap_or_else(|_| root().join("config").join("local.toml"))
}

pub fn config(at: &Path) -> R<Value> {
    let mut cfg: Value = toml::from_str(&read(&at.join("config").join("routing.toml"))?).map_err(|e| format!("config/routing.toml: {e}"))?;
    if local_path().is_file() {
        let local: Value = toml::from_str(&read(&local_path())?).map_err(|e| format!("config/local.toml: {e}"))?;
        for (key, value) in local.as_object().cloned().unwrap_or_default() {
            match (cfg.get_mut(&key).and_then(|v| v.as_object_mut()), value.as_object()) {
                (Some(base), Some(extra)) => base.extend(extra.clone()),
                _ => cfg[key.as_str()] = value,
            }
        }
    }
    for key in ["endpoints", "models"] {
        if !cfg[key].is_object() {
            cfg[key] = json!({});
        }
    }
    Ok(cfg)
}

pub fn spent_today() -> f64 {
    store::scalar("select coalesce(sum(usd),0) from calls where ts>=?", params![today()]).unwrap_or(0.0)
}

type Poster = Box<dyn Fn(&str, &Value) -> R<Value> + Send>;
/// Tests replace the network with this.
pub static POSTER: Mutex<Option<Poster>> = Mutex::new(None);

fn post(url: &str, body: &Value, key: Option<&str>, timeout_s: u64) -> R<Value> {
    if let Some(fake) = POSTER.lock().unwrap_or_else(|e| e.into_inner()).as_ref() {
        return fake(url, body);
    }
    let mut req = ureq::post(url).timeout(Duration::from_secs(timeout_s));
    if let Some(key) = key {
        req = req.set("Authorization", &format!("Bearer {key}"));
    }
    req.send_json(body.clone()).s()?.into_json::<Value>().s()
}

fn log(trace_id: &str, task: &str, model: &str, t0: f64, tin: i64, tout: i64, usd: f64, ok: bool, error: Option<&str>) {
    let _ = store::x("insert into calls(ts,trace_id,task,model,ms,tin,tout,usd,ok,error) values(?,?,?,?,?,?,?,?,?,?)",
        params![now(), trace_id, task, model, ((now() - t0) * 1000.0) as i64, tin, tout, usd, ok as i64, error]);
}

/// The reply text from the first model in the task's chain that answers.
pub fn call(task: &str, messages: &Value, at: &Path, trace_id: &str, cache: bool) -> R<String> {
    if std::env::var("FORGE_MOCK").is_ok() {
        // no model installed: a fixed stand-in so the whole pipeline can be tried
        let last = messages.as_array().and_then(|m| m.last()).and_then(|m| m["content"].as_str()).unwrap_or("");
        return Ok(format!("MOCK REPLY: {}", crate::util::tail(last, 200)));
    }
    let cfg = config(at)?;
    let chain: Vec<String> = cfg["tasks"][task].as_array().map(|a| a.iter().filter_map(|v| v.as_str().map(String::from)).collect())
        .ok_or_else(|| format!("RouterError: no models are listed for the task '{task}' in config/routing.toml"))?;
    if cfg["models"].as_object().map(|m| m.is_empty()).unwrap_or(true) {
        return Err("RouterError: No models are set up on this computer. Run: forge setup".to_string());
    }
    // shortcut: exact-match cache, not semantic; upgrade when near-duplicate requests are common
    let chain_models: Vec<&Value> = chain.iter().map(|n| &cfg["models"][n.as_str()]).collect();
    let cache_key = hash(&json!([chain_models, messages]).to_string());
    if cache {
        if let Some(hit) = store::q("select v from cache where k=?", params![cache_key])?.first() {
            return Ok(store::rs(hit, "v"));
        }
    }
    let mut errors = Vec::new();
    for name in &chain {
        // each model is tried once, so this loop always ends
        let m = &cfg["models"][name.as_str()];
        if !m.is_object() {
            errors.push(format!("{name}: not set up (run forge setup)"));
            continue;
        }
        let ep = &cfg["endpoints"][m["endpoint"].as_str().unwrap_or("")];
        let (usd_in, usd_out) = (m["usd_in"].as_f64().unwrap_or(0.0), m["usd_out"].as_f64().unwrap_or(0.0));
        if (usd_in > 0.0 || usd_out > 0.0) && spent_today() >= cfg["budget"]["daily_usd"].as_f64().unwrap_or(0.0) {
            errors.push(format!("{name}: daily budget reached"));
            continue;
        }
        let key = match ep["key_env"].as_str() {
            Some(var) => match std::env::var(var) {
                Ok(k) if !k.is_empty() => Some(k),
                _ => {
                    errors.push(format!("{name}: no API key set in {var}"));
                    continue;
                }
            },
            None => None,
        };
        let base = ep["base"].as_str().unwrap_or("").trim_end_matches('/').to_string();
        let mut body = json!({"model": m["model"], "messages": messages, "temperature": 0});
        if let Some(extra) = m["extra"].as_object() {
            body.as_object_mut().expect("body is an object").extend(extra.clone());
        }
        let timeout = m["timeout_s"].as_u64().or(cfg["budget"]["timeout_s"].as_u64()).unwrap_or(240);
        let t0 = now();
        let reply = post(&format!("{base}/chat/completions"), &body, key.as_deref(), timeout).and_then(|r| {
            let text = r["choices"][0]["message"]["content"].as_str().unwrap_or("").to_string();
            if text.trim().is_empty() { Err("empty reply".to_string()) } else { Ok((text, r)) }
        });
        match reply {
            Err(e) => {
                // any failure means: try the next model
                log(trace_id, task, name, t0, 0, 0, 0.0, false, Some(&e));
                errors.push(format!("{name}: {e}"));
            }
            Ok((text, r)) => {
                let (tin, tout) = (r["usage"]["prompt_tokens"].as_i64().unwrap_or(0), r["usage"]["completion_tokens"].as_i64().unwrap_or(0));
                log(trace_id, task, name, t0, tin, tout, (tin as f64 * usd_in + tout as f64 * usd_out) / 1e6, true, None);
                if cache {
                    let _ = store::x("insert or replace into cache(k,v,ts) values(?,?,?)", params![cache_key, text, now()]);
                }
                return Ok(text);
            }
        }
    }
    Err(format!("RouterError: {}", errors.join("; ")))
}
