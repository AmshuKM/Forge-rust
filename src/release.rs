//! Decisions and release: approve -> trial on part of the traffic -> keep (merge + tag) or discard.
use crate::store::{self, params, ri, rs, Row};
use crate::util::{git, now, root, R};
use crate::{evals, improve, router, runtime};
use serde_json::{json, Value};
use std::sync::Mutex;

static LOCK: Mutex<()> = Mutex::new(());

fn proposal(pid: i64, status: &str) -> R<Row> {
    let mut rows = store::q("select * from proposals where id=? and status=?", params![pid, status])?;
    if rows.is_empty() {
        return Err(format!("Change {pid} is not {status}."));
    }
    Ok(rows.remove(0))
}

pub fn cleanup(pid: i64) {
    // either may already be gone
    let _ = git(&["worktree", "remove", "--force", &improve::worktree(pid).to_string_lossy()], &root());
    let _ = git(&["branch", "-D", &format!("proposal/{pid}")], &root());
}

/// Record a final outcome so later proposals can learn from it.
pub fn close(p: &Row, status: &str, issue_status: &str, reason: &str) {
    let _ = store::x("update proposals set status=?, reason=? where id=?", params![status, reason, ri(p, "id")]);
    if !p.get("issue_id").map_or(true, |v| v.is_null()) {
        let _ = store::x("update issues set status=?, seen_count=count where id=?", params![issue_status, ri(p, "issue_id")]);
    }
    store::remember(status, &format!("{} {}", rs(p, "app"), rs(p, "channel")), &format!("{} -> {status}. {reason}", rs(p, "summary")));
}

/// Merge and tag. If the merge fails nothing changes and the change goes back to awaiting.
fn promote(p: &Row, note: &str) {
    let pid = ri(p, "id");
    let merged = (|| -> R<()> {
        if git(&["rev-parse", "--abbrev-ref", "HEAD"], &root())?.trim() != "main" {
            return Err("the project folder is not on the main branch".to_string());
        }
        git(&["merge", "--no-ff", "-m", &format!("release: change {pid}: {}", rs(p, "summary")), &rs(p, "branch")], &root())?;
        git(&["tag", "-f", &format!("release-{pid}")], &root()).map(|_| ())
    })();
    if let Err(e) = merged {
        let _ = git(&["merge", "--abort"], &root());
        let _ = store::x("update proposals set status='awaiting', reason=? where id=?", params![format!("The merge failed, so nothing changed: {e}"), pid]);
        return;
    }
    if let Some(after) = serde_json::from_str::<Value>(&rs(p, "gate")).ok().and_then(|g| g.get("after").cloned()) {
        evals::save(&rs(p, "app"), "stable", &after);
    }
    close(p, "shipped", "fixed", note);
    cleanup(pid);
    let app = rs(p, "app");
    std::thread::spawn(move || {
        let _ = runtime::build_app(&root(), &app); // compile in the background, so the first visit is not slow
    });
}

pub fn approve(pid: i64) -> R<()> {
    let p = proposal(pid, "awaiting")?;
    if !root().join("apps").join(rs(&p, "app")).join("src").join("main.rs").exists() {
        // a brand-new app has no live traffic to trial against
        let _guard = LOCK.lock().unwrap_or_else(|e| e.into_inner());
        promote(&p, "New app: released without a trial.");
        return Ok(());
    }
    if store::kv_get("canary").is_object() {
        return Err("Another change is still in its trial. Wait for it to finish first.".to_string());
    }
    let cfg = router::config(&root())?;
    store::kv_set("canary", &json!({"proposal": pid, "path": improve::worktree(pid).to_string_lossy(),
        "pct": cfg["release"]["canary_percent"], "need": cfg["release"]["canary_requests"], "since": now()}))?;
    store::x("update proposals set status='canary', reason=null where id=?", params![pid]).map(|_| ())
}

pub fn reject(pid: i64, reason: &str) -> R<()> {
    let p = proposal(pid, "awaiting")?;
    close(&p, "rejected", "rejected", &format!("Rejected by the owner: {}", if reason.trim().is_empty() { "no reason given" } else { reason.trim() }));
    cleanup(pid);
    Ok(())
}

/// Called after each trial request: keep the change, discard it, or keep watching.
pub fn check() {
    let _guard = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let c = store::kv_get("canary");
    let Some(pid) = c["proposal"].as_i64() else { return };
    let Some(p) = store::q("select * from proposals where id=?", params![pid]).ok().and_then(|mut r| if r.is_empty() { None } else { Some(r.remove(0)) }) else { return };
    let rows = store::q("select fingerprint from traces where version=? and ts>=?", params![format!("canary:p{pid}"), c["since"].as_f64().unwrap_or(0.0)]).unwrap_or_default();
    let (n, bad) = (rows.len() as f64, rows.iter().filter(|r| !r["fingerprint"].is_null()).count() as f64);
    let stable = store::scalar("select avg(fingerprint is not null) from (select fingerprint from traces where version='stable' order by ts desc limit 50)", params![]).unwrap_or(0.0);
    let need = c["need"].as_f64().unwrap_or(6.0);
    if (bad >= 3.0 && bad / n > 0.5) || (n >= need && bad / n > stable + 0.1) {
        let _ = store::kv_set("canary", &Value::Null);
        close(&p, "rolled_back", "hold", &format!("Trial failed: {bad} of {n} requests failed. Nothing reached main."));
        cleanup(pid);
    } else if n >= need {
        let _ = store::kv_set("canary", &Value::Null);
        promote(&p, &format!("Trial passed: {bad} of {n} requests failed."));
    }
}

/// Manual rollback of a shipped change: a revert commit, so history stays intact.
pub fn undo(pid: i64) -> R<()> {
    let p = proposal(pid, "shipped")?;
    git(&["revert", "-m", "1", "--no-edit", &format!("release-{pid}")], &root()).map_err(|e| format!("Could not undo change {pid}: {e}"))?;
    close(&p, "rolled_back", "hold", "Undone by the owner after release.");
    Ok(())
}
