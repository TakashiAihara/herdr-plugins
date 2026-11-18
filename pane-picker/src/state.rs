use serde_json::{Map, Value};
use std::fs;
use std::io::Write;
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;

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

impl State {
    pub fn load(path: &str) -> State {
        let parsed = fs::read_to_string(path)
            .ok()
            .and_then(|s| serde_json::from_str::<Value>(&s).ok());
        match parsed {
            Some(Value::Object(m)) if shape_ok(&m) => State(m),
            _ => State(Map::new()),
        }
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

pub fn write(dir: &str, path: &str, v: &Value) -> Result<(), String> {
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
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists && n < 100 => n += 1,
            Err(_) => return Err(format!("cannot write {dir}")),
        }
    };
    let text =
        serde_json::to_string_pretty(v).map_err(|_| "cannot update the state".to_string())?;
    let done = writeln!(file, "{text}")
        .and_then(|_| file.flush())
        .and_then(|_| fs::rename(Path::new(&tmp), path));
    if done.is_err() {
        let _ = fs::remove_file(&tmp);
        return Err(format!("cannot write {path}"));
    }
    Ok(())
}
