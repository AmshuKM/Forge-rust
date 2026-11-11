//! Sandbox: compiles and runs unapproved code in a locked Docker container. Fails closed when Docker is missing.
//! The container never has network access; when an app needs a model, Forge makes the call from outside.
use crate::runtime::Invoker;
use crate::util::{root, tail, which, Sx, R};
use crate::router;
use serde_json::Value;
use std::io::{BufRead, BufReader, Write};
use std::path::Path;
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};

pub fn available() -> bool {
    which("docker").is_some() && Command::new("docker").arg("info").stdout(Stdio::null()).stderr(Stdio::null()).status().map(|s| s.success()).unwrap_or(false)
}

fn setting(key: &str, default: &str) -> String {
    router::config(&root()).ok().and_then(|c| c["sandbox"][key].as_str().map(String::from)).unwrap_or_else(|| default.to_string())
}

/// Locked-down `docker run` arguments: read-only project, memory cap, no extra privileges.
fn docker(at: &Path, network: bool, interactive: bool) -> Command {
    let mut c = Command::new("docker");
    c.args(["run", "--rm", "--memory", &setting("memory", "3g"), "--cpus", "2", "--pids-limit", "512", "--read-only",
            "--tmpfs", "/tmp:exec,size=2g", "--cap-drop", "ALL", "--security-opt", "no-new-privileges",
            "-e", "CARGO_HOME=/tmp/cargo", "-e", "HOME=/tmp", "-v"]);
    c.arg(format!("{}:/src:ro", at.display())).args(["-w", "/src"]);
    if !network {
        c.args(["--network", "none"]);
    }
    if interactive {
        c.arg("-i");
    }
    c
}

/// Copies the app and the helper library out of the read-only mount so cargo can build them.
fn prepare(app: &str) -> String {
    format!("mkdir -p /tmp/w/apps && cp -r /src/sdk /tmp/w/sdk && cp -r /src/apps/{app} /tmp/w/apps/{app} && cd /tmp/w/apps/{app}")
}

fn finish(mut c: Command) -> (bool, String) {
    if !available() {
        return (false, "Docker is not running, so nothing was executed.".to_string());
    }
    match c.output() {
        Ok(out) => {
            let text = format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
            (out.status.success(), tail(&text, 6000))
        }
        Err(e) => (false, format!("docker could not be started: {e}")),
    }
}

/// The app's unit tests: no network, read-only project, memory-capped.
pub fn tests(at: &Path, app: &str) -> (bool, String) {
    let mut c = docker(at, false, false);
    c.arg(setting("image", "rust:1-slim")).args(["sh", "-c", &format!("{} && cargo test --offline 2>&1", prepare(app))]);
    finish(c)
}

pub fn scans(at: &Path) -> (bool, String) {
    let scans = router::config(&root()).ok().and_then(|c| c["sandbox"]["scans"].as_array().cloned()).unwrap_or_default();
    let (mut all_ok, mut report) = (true, Vec::new());
    for scan in scans {
        let parts: Vec<String> = scan.as_array().map(|a| a.iter().filter_map(|s| s.as_str().map(String::from)).collect()).unwrap_or_default();
        let Some((image, cmd)) = parts.split_first() else { continue };
        let mut c = docker(at, true, false); // scanners download their own rule sets
        c.args(["-v", "forge-scan-cache:/cache", "-e", "HOME=/cache", "-e", "XDG_CACHE_HOME=/cache"]).arg(image).args(cmd);
        let (ok, out) = finish(c);
        all_ok &= ok;
        report.push(format!("[{}] {image}\n{}", if ok { "pass" } else { "FAIL" }, tail(&out, 1200)));
    }
    (all_ok, report.join("\n\n"))
}

/// One long-lived container holding the built app: a line of input in, a line of step out.
pub struct Session {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
}

pub fn session(at: &Path, app: &str) -> R<Session> {
    if !available() {
        return Err("Docker is not running, so nothing was executed.".to_string());
    }
    let script = format!(
        "{prep} && if cargo build --release --offline >/tmp/build.log 2>&1; then echo READY; \
         else echo BUILD_FAILED; tail -c 3000 /tmp/build.log | tr '\\n' '\\r'; echo; exit 1; fi; \
         while IFS= read -r line; do \
           if out=$(printf '%s\\n' \"$line\" | timeout 10 ./target/release/{app} 2>/tmp/err); then printf '%s\\n' \"$out\"; \
           else printf 'CRASH '; head -c 800 /tmp/err | tr '\\n' ' '; echo; fi; \
         done", prep = prepare(app));
    let mut c = docker(at, false, true);
    c.arg(setting("image", "rust:1-slim")).args(["sh", "-c", &script]).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::null());
    let mut child = c.spawn().map_err(|e| format!("docker could not be started: {e}"))?;
    let stdin = child.stdin.take().ok_or("no pipe to the sandbox")?;
    let mut stdout = BufReader::new(child.stdout.take().ok_or("no pipe from the sandbox")?);
    let mut first = String::new();
    stdout.read_line(&mut first).s()?;
    if first.trim() != "READY" {
        let mut log = String::new();
        let _ = stdout.read_line(&mut log);
        let _ = child.kill();
        let _ = child.wait();
        return Err(format!("the app does not compile in the sandbox:\n{}", log.replace('\r', "\n").trim()));
    }
    Ok(Session { child, stdin, stdout })
}

impl Invoker for Session {
    fn invoke(&mut self, input: &Value) -> R<Value> {
        writeln!(self.stdin, "{input}").and_then(|_| self.stdin.flush()).map_err(|e| format!("Crash: the sandbox stopped: {e}"))?;
        let mut line = String::new();
        self.stdout.read_line(&mut line).map_err(|e| format!("Crash: the sandbox stopped: {e}"))?;
        if let Some(err) = line.strip_prefix("CRASH") {
            let err = err.trim();
            return Err(if err.is_empty() { "Timeout: the app took longer than 10 seconds or printed nothing".to_string() } else { format!("Crash: {err}") });
        }
        serde_json::from_str(line.trim()).map_err(|_| "BadStep: the app printed something that is not a step".to_string())
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Golden cases against unapproved code, inside the container.
pub fn evals(at: &Path, app: &str) -> R<Value> {
    let mut s = session(at, app)?;
    crate::evals::run(app, at, &mut s)
}
