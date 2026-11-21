use serde_json::{json, Value};
use std::fs;
use std::io::{ErrorKind, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;

// What the state file says about the applied preset: a name, an explicit "nothing is applied"
// (`{"preset":null}`, written by `clear`), or no opinion at all.
pub enum Saved {
    Unset,
    Cleared,
    Named(String),
}

// Missing, unreadable or wrong-shaped content counts as Unset.
pub fn read(path: &str) -> Saved {
    let v = fs::read_to_string(path)
        .ok()
        .and_then(|s| serde_json::from_str::<Value>(&s).ok());
    match v.as_ref().and_then(|v| v.get("preset")) {
        Some(Value::String(s)) => Saved::Named(s.clone()),
        Some(Value::Null) => Saved::Cleared,
        _ => Saved::Unset,
    }
}

// Removes the state file; a missing file is already the wanted end state.
pub fn remove(path: &str) -> Result<(), String> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == ErrorKind::NotFound => Ok(()),
        Err(_) => Err(format!("cannot remove {path}")),
    }
}

pub fn write(dir: &str, path: &str, preset: Option<&str>) -> Result<(), String> {
    let text = json!({ "preset": preset }).to_string();
    write_bytes(dir, path, format!("{text}\n").as_bytes())
}

// Writes into a temp file in the state dir and renames it into place, so a failure leaves the file
// as it was.
fn write_bytes(dir: &str, path: &str, bytes: &[u8]) -> Result<(), String> {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_name_a_null_and_everything_else_read_back_as_written() {
        let dir = crate::testutil::unique_dir("state");
        let path = dir.join("state.json").to_string_lossy().into_owned();
        write(&dir.to_string_lossy(), &path, Some("prio")).unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(
            fs::read_to_string(&path).unwrap_or_default(),
            "{\"preset\":\"prio\"}\n"
        );
        assert!(matches!(read(&path), Saved::Named(n) if n == "prio"));
        write(&dir.to_string_lossy(), &path, None).unwrap_or_else(|e| panic!("{e}"));
        assert!(matches!(read(&path), Saved::Cleared));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn remove_deletes_the_file_and_a_missing_one_is_fine() {
        let dir = crate::testutil::unique_dir("state-remove");
        let path = dir.join("state.json").to_string_lossy().into_owned();
        write(&dir.to_string_lossy(), &path, Some("prio")).unwrap_or_else(|e| panic!("{e}"));
        remove(&path).unwrap_or_else(|e| panic!("{e}"));
        assert!(matches!(read(&path), Saved::Unset));
        remove(&path).unwrap_or_else(|e| panic!("a missing file is not an error: {e}"));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn missing_broken_and_foreign_content_is_unset() {
        let dir = crate::testutil::unique_dir("state-unset");
        let path = dir.join("state.json");
        assert!(matches!(read(&path.to_string_lossy()), Saved::Unset));
        for text in ["garbage", "{}", "{\"preset\":5}"] {
            fs::write(&path, text).unwrap_or_else(|e| panic!("{e}"));
            assert!(
                matches!(read(&path.to_string_lossy()), Saved::Unset),
                "{text}"
            );
        }
        let _ = fs::remove_dir_all(&dir);
    }
}
