//! Small shared helpers: errors as strings, paths, time, git, HTML escaping.
use chrono::{Local, TimeZone};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

/// Every fallible function returns a message a person can read.
pub type R<T> = Result<T, String>;

pub trait Sx<T> {
    fn s(self) -> R<T>;
}

impl<T, E: std::fmt::Display> Sx<T> for Result<T, E> {
    fn s(self) -> R<T> {
        self.map_err(|e| e.to_string())
    }
}

/// The project folder: where `config/`, `apps/` and `prompts/` live.
pub fn root() -> PathBuf {
    std::env::var("FORGE_ROOT").map(PathBuf::from).unwrap_or_else(|_| std::env::current_dir().unwrap_or_default())
}

pub fn now() -> f64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs_f64()).unwrap_or(0.0)
}

/// Local midnight, as a Unix time.
pub fn today() -> f64 {
    Local::now().date_naive().and_hms_opt(0, 0, 0).and_then(|t| t.and_local_timezone(Local).earliest())
        .map(|t| t.timestamp() as f64).unwrap_or(0.0)
}

fn stamp(ts: f64, format: &str, empty: &str) -> String {
    if ts <= 0.0 {
        return empty.to_string();
    }
    Local.timestamp_opt(ts as i64, 0).single().map(|t| t.format(format).to_string()).unwrap_or_else(|| empty.to_string())
}

pub fn when(ts: f64) -> String {
    stamp(ts, "%d %b %H:%M", "never")
}

pub fn clock(ts: f64) -> String {
    stamp(ts, "%H:%M", "--:--")
}

pub fn clock_s(ts: f64) -> String {
    stamp(ts, "%H:%M:%S", "--:--:--")
}

pub fn esc(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;").replace('\'', "&#x27;")
}

/// shortcut: FNV-1a, stable across runs but not cryptographic; upgrade if a hash ever guards anything.
pub fn hash(s: &str) -> String {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in s.bytes() {
        h ^= b as u64;
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    format!("{h:016x}")
}

pub fn random_hex(bytes: usize) -> String {
    let mut buf = vec![0u8; bytes];
    getrandom::getrandom(&mut buf).expect("the operating system could not supply random bytes");
    buf.iter().map(|b| format!("{b:02x}")).collect()
}

pub fn git(args: &[&str], cwd: &Path) -> R<String> {
    let out = Command::new("git").args(["-c", "user.name=forge", "-c", "user.email=forge@localhost"]).args(args)
        .current_dir(cwd).output().map_err(|e| format!("git could not be started: {e}"))?;
    if out.status.success() {
        return Ok(String::from_utf8_lossy(&out.stdout).into_owned());
    }
    let err = String::from_utf8_lossy(&out.stderr);
    let msg = if err.trim().is_empty() { String::from_utf8_lossy(&out.stdout).into_owned() } else { err.into_owned() };
    Err(format!("git {} failed: {}", args.first().unwrap_or(&""), msg.trim()))
}

pub fn read(path: &Path) -> R<String> {
    std::fs::read_to_string(path).map_err(|e| format!("could not read {}: {e}", path.display()))
}

/// Write `text` at `base/rel`. `rel` may never climb out of `base`.
pub fn write(base: &Path, rel: &str, text: &str) -> R<()> {
    let rel = rel.replace('\\', "/");
    if rel.starts_with('/') || rel.contains(':') || rel.split('/').any(|part| part == ".." || part.is_empty()) {
        return Err(format!("{rel} is outside the project"));
    }
    let path = base.join(&rel);
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).s()?;
    }
    std::fs::write(&path, text.replace("\r\n", "\n")).map_err(|e| format!("could not write {rel}: {e}"))
}

/// The full path of a program on PATH, trying the usual Windows extensions too.
pub fn which(name: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path) {
        for ext in ["", ".exe", ".cmd", ".bat"] {
            let candidate = dir.join(format!("{name}{ext}"));
            if candidate.is_file() {
                return Some(candidate);
            }
        }
    }
    None
}

/// The last `n` characters, cut on a character boundary.
pub fn tail(s: &str, n: usize) -> String {
    let count = s.chars().count();
    s.chars().skip(count.saturating_sub(n)).collect()
}

pub fn head(s: &str, n: usize) -> String {
    s.chars().take(n).collect()
}
