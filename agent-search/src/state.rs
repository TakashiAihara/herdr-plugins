use serde_json::{Map, Value};
use std::fs;
use std::io::{ErrorKind, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;
use std::time::{Duration, Instant};

pub struct State(pub Map<String, Value>);

// jq `//` treats null and false as absent.
pub fn present(v: Option<&Value>) -> Option<&Value> {
    v.filter(|v| !matches!(v, Value::Null | Value::Bool(false)))
}

fn shape_ok(m: &Map<String, Value>) -> bool {
    ["prio", "order"]
        .iter()
        .all(|k| present(m.get(*k)).is_none_or(Value::is_object))
}

// The state file's bytes: None when there is no file. A file that exists but cannot be read is an
// error, so a writer never overwrites a state it could not see.
pub fn read_raw(path: &str) -> Result<Option<Vec<u8>>, String> {
    match fs::read(path) {
        Ok(b) => Ok(Some(b)),
        Err(e) if matches!(e.kind(), ErrorKind::NotFound | ErrorKind::NotADirectory) => Ok(None),
        Err(_) => Err(format!("cannot read {path}")),
    }
}

impl State {
    // Missing, unparsable or wrong-shaped content is an empty state.
    pub fn parse(raw: Option<&[u8]>) -> State {
        let parsed = raw
            .and_then(|b| std::str::from_utf8(b).ok())
            .and_then(|s| serde_json::from_str::<Value>(s).ok());
        match parsed {
            Some(Value::Object(m)) if shape_ok(&m) => State(m),
            _ => State(Map::new()),
        }
    }

    // For commands that only read: an unreadable file shows as an empty state.
    pub fn load(path: &str) -> State {
        State::parse(read_raw(path).ok().flatten().as_deref())
    }

    pub fn get(&self, k: &str) -> Option<&Value> {
        self.0.get(k)
    }

    // `(.<map> // {})[key] // null`
    pub fn lookup(&self, map: &str, key: &str) -> Option<Value> {
        let m = present(self.0.get(map))?.as_object()?;
        present(m.get(key)).cloned()
    }
}

// Writes into a temp file in the state dir and renames it into place, so a failure leaves the file as it was.
pub fn write_bytes(dir: &str, path: &str, bytes: &[u8]) -> Result<(), String> {
    fs::create_dir_all(dir).map_err(|_| format!("cannot create {dir}"))?;
    let pid = std::process::id();
    let mut n = 0u32;
    let (tmp, mut file) = loop {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.subsec_nanos());
        let tmp = format!("{dir}/.state.{pid}.{nanos}.{n}");
        match fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&tmp)
        {
            Ok(f) => break (tmp, f),
            Err(e) if e.kind() == ErrorKind::AlreadyExists && n < 100 => n += 1,
            Err(_) => return Err(format!("cannot write {dir}")),
        }
    };
    let done = file
        .write_all(bytes)
        .and_then(|_| file.flush())
        .and_then(|_| fs::rename(Path::new(&tmp), path));
    if done.is_err() {
        let _ = fs::remove_file(&tmp);
        return Err(format!("cannot write {path}"));
    }
    Ok(())
}

pub fn write(dir: &str, path: &str, v: &Value) -> Result<(), String> {
    let text =
        serde_json::to_string_pretty(v).map_err(|_| "cannot update the state".to_string())?;
    write_bytes(dir, path, format!("{text}\n").as_bytes())
}

// Puts back what `read_raw` saw: the old bytes, or no file at all.
pub fn rollback(dir: &str, path: &str, before: Option<&[u8]>) -> Result<(), String> {
    match before {
        Some(b) => write_bytes(dir, path, b),
        None => match fs::remove_file(path) {
            Err(e) if e.kind() != ErrorKind::NotFound => Err(format!("cannot write {path}")),
            _ => Ok(()),
        },
    }
}

// A lock file next to the state file (`<state>.lock`), so two processes do not read-modify-write
// the state at once. shortcut: two waiters that both judge one stale lock stale can remove each
// other's fresh lock; a lock is held for milliseconds, so upgrade to flock if that is ever seen.
pub struct Lock(String);

impl Lock {
    pub fn acquire(dir: &str, path: &str) -> Result<Lock, String> {
        Lock::acquire_with(dir, path, Duration::from_secs(2), Duration::from_secs(10))
    }

    pub fn acquire_with(
        dir: &str,
        path: &str,
        wait: Duration,
        stale: Duration,
    ) -> Result<Lock, String> {
        fs::create_dir_all(dir).map_err(|_| format!("cannot create {dir}"))?;
        let lock = format!("{path}.lock");
        let start = Instant::now();
        loop {
            match fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(&lock)
            {
                Ok(_) => return Ok(Lock(lock)),
                Err(e) if e.kind() == ErrorKind::AlreadyExists => {
                    let age = fs::metadata(&lock)
                        .and_then(|m| m.modified())
                        .ok()
                        .and_then(|t| t.elapsed().ok());
                    if age.is_some_and(|a| a > stale) {
                        let _ = fs::remove_file(&lock);
                        continue;
                    }
                    if start.elapsed() >= wait {
                        return Err(format!("cannot lock {path}: {lock} is held"));
                    }
                    std::thread::sleep(Duration::from_millis(20));
                }
                Err(_) => return Err(format!("cannot write {dir}")),
            }
        }
    }
}

impl Drop for Lock {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dir(name: &str) -> String {
        let d = std::env::temp_dir().join(format!("agent-search-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap_or_else(|e| panic!("{e}"));
        d.to_string_lossy().into_owned()
    }

    #[test]
    fn a_held_lock_blocks_until_released_and_a_stale_one_is_taken_over() {
        let d = dir("lock");
        let path = format!("{d}/state.json");
        let first = Lock::acquire(&d, &path).unwrap_or_else(|e| panic!("{e}"));
        let short = Duration::from_millis(100);
        let err = Lock::acquire_with(&d, &path, short, Duration::from_secs(60)).err();
        assert!(err.is_some_and(|e| e.starts_with("cannot lock ")));
        drop(first);
        assert!(!Path::new(&format!("{path}.lock")).exists());
        let held = Lock::acquire(&d, &path).unwrap_or_else(|e| panic!("{e}"));
        std::mem::forget(held);
        let taken = Lock::acquire_with(&d, &path, short, Duration::ZERO);
        assert!(taken.is_ok());
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn an_unreadable_state_is_an_error_and_a_missing_one_is_empty() {
        let d = dir("raw");
        assert_eq!(read_raw(&format!("{d}/none.json")), Ok(None));
        assert_eq!(read_raw(&format!("{d}/none/sub.json")), Ok(None));
        assert_eq!(read_raw(&d), Err(format!("cannot read {d}")));
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn rollback_restores_the_old_bytes_or_removes_the_file() {
        let d = dir("rollback");
        let path = format!("{d}/state.json");
        write(&d, &path, &Value::Object(Map::new())).unwrap_or_else(|e| panic!("{e}"));
        let before = read_raw(&path).unwrap_or_default();
        write_bytes(&d, &path, b"{\"x\":1}").unwrap_or_else(|e| panic!("{e}"));
        rollback(&d, &path, before.as_deref()).unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(read_raw(&path), Ok(before));
        rollback(&d, &path, None).unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(read_raw(&path), Ok(None));
        let _ = fs::remove_dir_all(&d);
    }
}
