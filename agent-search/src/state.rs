use serde_json::{Map, Value};
use sha2::Digest;
use std::fs;
use std::io::{ErrorKind, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;
use std::time::{Duration, Instant};

pub struct State(pub Map<String, Value>);

// Every session of this plugin shares HERDR_PLUGIN_STATE_DIR, but each has its own
// HERDR_SOCKET_PATH, so the state file is named after the session's socket: one session's write,
// prune or lock never touches another's. The name holds the first 12 hex characters of the
// socket path's SHA-256; without a socket (a run outside Herdr) it stays `state.json`.
pub fn file_name(socket_path: Option<&str>) -> String {
    match socket_path {
        Some(p) => {
            let tag: String = sha2::Sha256::digest(p.as_bytes())[..6]
                .iter()
                .map(|b| format!("{b:02x}"))
                .collect();
            format!("state-{tag}.json")
        }
        None => "state.json".to_string(),
    }
}

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

// An exclusive OS lock on `<state>.lock`, next to the state file, so two processes do not
// read-modify-write the state at once. The kernel drops it when the holder exits, so a crash never
// leaves a stale lock; the file itself stays.
pub struct Lock(fs::File);

impl Lock {
    pub fn acquire(dir: &str, path: &str) -> Result<Lock, String> {
        Lock::acquire_with(dir, path, Duration::from_secs(2))
    }

    pub fn acquire_with(dir: &str, path: &str, wait: Duration) -> Result<Lock, String> {
        fs::create_dir_all(dir).map_err(|_| format!("cannot create {dir}"))?;
        let lock = format!("{path}.lock");
        let file = fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .open(&lock)
            .map_err(|_| format!("cannot write {lock}"))?;
        let start = Instant::now();
        loop {
            match file.try_lock() {
                Ok(()) => return Ok(Lock(file)),
                Err(fs::TryLockError::WouldBlock) if start.elapsed() < wait => {
                    std::thread::sleep(Duration::from_millis(20));
                }
                Err(fs::TryLockError::WouldBlock) => {
                    return Err(format!(
                        "cannot lock {path}: {lock} is held by another process"
                    ))
                }
                Err(fs::TryLockError::Error(_)) => return Err(format!("cannot lock {lock}")),
            }
        }
    }
}

impl Drop for Lock {
    fn drop(&mut self) {
        let _ = self.0.unlock();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dir(name: &str) -> String {
        crate::testutil::unique_dir(name)
            .to_string_lossy()
            .into_owned()
    }

    #[test]
    fn the_state_file_is_named_after_the_session_socket() {
        assert_eq!(file_name(None), "state.json");
        // sha256("abc") starts with ba7816bf8f01.
        assert_eq!(file_name(Some("abc")), "state-ba7816bf8f01.json");
        assert_ne!(
            file_name(Some("/run/a.sock")),
            file_name(Some("/run/b.sock"))
        );
    }

    #[test]
    fn a_held_lock_blocks_until_released_and_dropping_releases_it() {
        let d = dir("lock");
        let path = format!("{d}/state.json");
        let first = Lock::acquire(&d, &path).unwrap_or_else(|e| panic!("{e}"));
        let err = Lock::acquire_with(&d, &path, Duration::from_millis(100)).err();
        assert!(err.is_some_and(|e| e.starts_with("cannot lock ")));
        drop(first);
        assert!(Lock::acquire_with(&d, &path, Duration::from_millis(100)).is_ok());
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn a_waiter_gets_the_lock_when_the_holder_lets_go() {
        let d = dir("waiter");
        let path = format!("{d}/state.json");
        let first = Lock::acquire(&d, &path).unwrap_or_else(|e| panic!("{e}"));
        let releaser = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(150));
            drop(first);
        });
        assert!(Lock::acquire_with(&d, &path, Duration::from_secs(5)).is_ok());
        releaser
            .join()
            .unwrap_or_else(|_| panic!("releaser panicked"));
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn a_leftover_lock_file_does_not_block_anyone() {
        let d = dir("leftover");
        let path = format!("{d}/state.json");
        fs::write(format!("{path}.lock"), "").unwrap_or_else(|e| panic!("{e}"));
        assert!(Lock::acquire_with(&d, &path, Duration::from_millis(100)).is_ok());
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
