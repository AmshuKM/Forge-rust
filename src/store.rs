//! SQLite storage: traces, model calls, feedback, issues, proposals, memory, settings.
use crate::util::{now, root, Sx, R};
use rusqlite::types::ValueRef;
use rusqlite::{Connection, ToSql};
use serde_json::{json, Map, Value};
use std::sync::Mutex;

pub use rusqlite::params;
pub type Row = Map<String, Value>;

const SCHEMA: &str = "
create table if not exists traces(id text primary key, ts real, app text, version text, ok int,
  error text, fingerprint text, ms int, input text, output text);
create table if not exists calls(id integer primary key, ts real, trace_id text, task text, model text,
  ms int, tin int, tout int, usd real, ok int, error text);
create table if not exists feedback(id integer primary key, ts real, trace_id text, rating int, text text,
  seen int default 0);
create table if not exists issues(id integer primary key, fingerprint text unique, kind text, app text,
  title text, detail text, count int default 0, seen_count int default 0, first_ts real, last_ts real,
  status text default 'open');
create table if not exists proposals(id integer primary key, ts real, issue_id int, app text, channel text,
  branch text, status text, attempts int default 0, summary text, diff text, gate text, reason text);
create table if not exists scorecards(id integer primary key, ts real, app text, version text, card text);
create table if not exists cache(k text primary key, v text, ts real);
create table if not exists kv(k text primary key, v text);
create virtual table if not exists memory using fts5(ts unindexed, kind, key, text);
";

static DB: Mutex<Option<(String, Connection)>> = Mutex::new(None);

fn with<T>(f: impl FnOnce(&Connection) -> R<T>) -> R<T> {
    let path = std::env::var("FORGE_DB").unwrap_or_else(|_| root().join(".forge").join("forge.db").to_string_lossy().into_owned());
    let mut guard = DB.lock().unwrap_or_else(|e| e.into_inner());
    if guard.as_ref().map(|(p, _)| p != &path).unwrap_or(true) {
        if path != ":memory:" {
            if let Some(dir) = std::path::Path::new(&path).parent() {
                std::fs::create_dir_all(dir).s()?;
            }
        }
        let conn = Connection::open(&path).s()?;
        conn.busy_timeout(std::time::Duration::from_secs(30)).s()?;
        conn.execute_batch(&format!("pragma journal_mode=wal; {SCHEMA}")).s()?;
        *guard = Some((path, conn));
    }
    f(&guard.as_ref().expect("database was just opened").1)
}

/// Forget the open database, so the next call opens a fresh one (tests use an in-memory database).
#[cfg(test)]
pub fn reset() {
    *DB.lock().unwrap_or_else(|e| e.into_inner()) = None;
}

pub fn q(sql: &str, args: &[&dyn ToSql]) -> R<Vec<Row>> {
    with(|c| {
        let mut st = c.prepare(sql).s()?;
        let names: Vec<String> = st.column_names().iter().map(|n| n.to_string()).collect();
        let rows = st.query_map(args, |r| {
            let mut m = Row::new();
            for (i, name) in names.iter().enumerate() {
                let v = match r.get_ref(i) {
                    Ok(ValueRef::Integer(x)) => json!(x),
                    Ok(ValueRef::Real(x)) => json!(x),
                    Ok(ValueRef::Text(t)) => json!(String::from_utf8_lossy(t)),
                    _ => Value::Null,
                };
                m.insert(name.clone(), v);
            }
            Ok(m)
        }).s()?;
        rows.collect::<Result<Vec<_>, _>>().s()
    })
}

/// Run a statement; returns the id of the last inserted row.
pub fn x(sql: &str, args: &[&dyn ToSql]) -> R<i64> {
    with(|c| {
        c.execute(sql, args).s()?;
        Ok(c.last_insert_rowid())
    })
}

pub fn rs(row: &Row, key: &str) -> String {
    row.get(key).and_then(|v| v.as_str()).unwrap_or("").to_string()
}

pub fn ri(row: &Row, key: &str) -> i64 {
    row.get(key).and_then(|v| v.as_i64().or_else(|| v.as_f64().map(|f| f as i64))).unwrap_or(0)
}

pub fn rf(row: &Row, key: &str) -> f64 {
    row.get(key).and_then(|v| v.as_f64()).unwrap_or(0.0)
}

/// A count or sum from a one-row, one-column query.
pub fn scalar(sql: &str, args: &[&dyn ToSql]) -> R<f64> {
    Ok(q(sql, args)?.first().and_then(|r| r.values().next().and_then(|v| v.as_f64())).unwrap_or(0.0))
}

pub fn kv_get(key: &str) -> Value {
    q("select v from kv where k=?", params![key]).ok().and_then(|rows| rows.first().map(|r| rs(r, "v")))
        .and_then(|text| serde_json::from_str(&text).ok()).unwrap_or(Value::Null)
}

pub fn kv_set(key: &str, value: &Value) -> R<()> {
    x("insert into kv(k,v) values(?,?) on conflict(k) do update set v=excluded.v", params![key, value.to_string()]).map(|_| ())
}

/// One check's progress for the build page. `ok = None` marks it started; `Some` marks it finished.
pub fn stage(pid: i64, name: &str, ok: Option<bool>) {
    let key = format!("stages:{pid}");
    let mut stages = kv_get(&key);
    if !stages.is_object() {
        stages = json!({});
    }
    let t = now();
    match ok {
        None => stages[name] = json!({"start": t}),
        Some(ok) => {
            if !stages[name].is_object() {
                stages[name] = json!({"start": t});
            }
            stages[name]["ok"] = json!(ok);
            stages[name]["end"] = json!(t);
        }
    }
    let _ = kv_set(&key, &stages);
}

pub fn remember(kind: &str, key: &str, text: &str) {
    let _ = x("insert into memory(ts,kind,key,text) values(?,?,?,?)", params![now(), kind, key, text]);
}

/// shortcut: keyword search (FTS5), not embeddings; upgrade if recall misses reworded problems.
pub fn recall(query: &str, n: i64) -> Vec<Row> {
    let mut words: Vec<String> = Vec::new();
    for w in query.split(|c: char| !(c.is_ascii_alphabetic() || c == '_')) {
        if w.len() >= 4 && !words.iter().any(|x| x == w) {
            words.push(w.to_string());
        }
    }
    words.truncate(12);
    if words.is_empty() {
        return Vec::new();
    }
    let matcher = words.iter().map(|w| format!("\"{w}\"")).collect::<Vec<_>>().join(" OR ");
    q("select kind, key, text from memory where memory match ? order by rank limit ?", params![matcher, n]).unwrap_or_default()
}
