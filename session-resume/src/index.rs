use crate::extract::{apply_line, Bodies, Meta};
use crate::Res;
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
use std::fs::{self, File, OpenOptions};
use std::io::{BufRead, BufReader, Read, Seek, SeekFrom, Write};
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};

// A cache written by another version is rebuilt, never read.
const FORMAT: u64 = 1;
const BODY_EXT: [&str; 3] = ["user", "assistant", "tools"];

fn to_json(m: &Meta) -> Value {
    json!({
        "version": FORMAT,
        "size": m.size,
        "offset": m.offset,
        "source": m.source,
        "custom_title": m.custom_title,
        "ai_title": m.ai_title,
        "first_prompt": m.first_prompt,
        "cwd": m.cwd,
        "branch": m.branch,
        "last_ms": m.last_ms,
        "lens": m.lens,
    })
}

fn from_json(v: &Value) -> Option<Meta> {
    if v["version"].as_u64()? != FORMAT {
        return None;
    }
    let s = |k: &str| v[k].as_str().map(String::from);
    let lens = v["lens"].as_array()?;
    Some(Meta {
        size: v["size"].as_u64()?,
        offset: v["offset"].as_u64()?,
        source: s("source")?,
        custom_title: s("custom_title")?,
        ai_title: s("ai_title")?,
        first_prompt: s("first_prompt")?,
        cwd: s("cwd")?,
        branch: s("branch")?,
        last_ms: match &v["last_ms"] {
            Value::Null => None,
            n => Some(n.as_i64()?),
        },
        lens: [
            lens.first()?.as_u64()?,
            lens.get(1)?.as_u64()?,
            lens.get(2)?.as_u64()?,
        ],
    })
}

// Every `<projects>/<dir>/<id>.jsonl`, and nothing deeper: a subagent's transcript lives below.
pub fn transcripts(projects: &Path) -> Res<Vec<(String, PathBuf)>> {
    let dirs = fs::read_dir(projects).map_err(|e| {
        format!(
            "cannot read the projects directory {}: {e}",
            projects.display()
        )
    })?;
    // One session id in two directories is one session; the longer file has the later history.
    let mut best: HashMap<String, (u64, PathBuf)> = HashMap::new();
    for dir in dirs.flatten() {
        let Ok(files) = fs::read_dir(dir.path()) else {
            continue;
        };
        for f in files.flatten() {
            let path = f.path();
            let Some(id) = path
                .file_name()
                .and_then(|n| n.to_str())
                .and_then(|n| n.strip_suffix(".jsonl"))
                .filter(|id| !id.is_empty())
            else {
                continue;
            };
            let Ok(md) = fs::metadata(&path) else {
                continue;
            };
            // Equal lengths go to the lower path, so the choice does not follow directory order.
            let wins =
                |(len, p): &(u64, PathBuf)| md.len() > *len || (md.len() == *len && path < *p);
            if md.is_file() && best.get(id).is_none_or(wins) {
                best.insert(id.to_string(), (md.len(), path.clone()));
            }
        }
    }
    let mut all: Vec<_> = best.into_iter().map(|(id, (_, p))| (id, p)).collect();
    all.sort();
    Ok(all)
}

#[derive(Clone)]
pub struct Index {
    dir: PathBuf,
}

impl Index {
    // The index holds text from the transcripts, so only this user may read it.
    pub fn open(state_dir: &Path) -> Res<Index> {
        let dir = state_dir.join("index");
        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(&dir)
            .map_err(|e| format!("cannot create {}: {e}", dir.display()))?;
        // A directory that was already there (restored, or made by an older tool) keeps its mode
        // through `create`.
        fs::set_permissions(&dir, fs::Permissions::from_mode(0o700))
            .map_err(|e| format!("cannot restrict {}: {e}", dir.display()))?;
        Ok(Index { dir })
    }

    // Held while one session's cache is rewritten: a popup and `session-resume index` may run at
    // the same time, and two writers would interleave their appends. Dropping it unlocks.
    fn lock(&self) -> Res<File> {
        let path = self.dir.join(".lock");
        let f = OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .open(&path)
            .map_err(|e| format!("cannot open {}: {e}", path.display()))?;
        f.lock()
            .map_err(|e| format!("cannot lock {}: {e}", path.display()))?;
        Ok(f)
    }

    fn file(&self, id: &str, ext: &str) -> PathBuf {
        self.dir.join(format!("{id}.{ext}"))
    }

    fn body_file(&self, id: &str, tier: usize) -> PathBuf {
        self.file(id, BODY_EXT[tier])
    }

    // The cached meta, or None when there is none, it is unreadable, or another version wrote it.
    pub fn meta(&self, id: &str) -> Option<Meta> {
        let text = fs::read_to_string(self.file(id, "meta.json")).ok()?;
        from_json(&serde_json::from_str(&text).ok()?)
    }

    fn bodies_intact(&self, id: &str, meta: &Meta) -> bool {
        (0..3).all(|t| fs::metadata(self.body_file(id, t)).is_ok_and(|m| m.len() >= meta.lens[t]))
    }

    // Up to date means nothing was appended since the last read; it takes stats and no reading.
    pub fn is_fresh(&self, id: &str, meta: &Meta, transcript: &Path) -> bool {
        fs::metadata(transcript).is_ok_and(|m| m.len() == meta.size)
            && transcript.to_string_lossy() == meta.source
            && self.bodies_intact(id, meta)
    }

    // The first `len` bytes of one body file; what an index of that length recorded.
    pub fn body(&self, id: &str, tier: usize, len: u64) -> Res<String> {
        let path = self.body_file(id, tier);
        let mut bytes = Vec::new();
        File::open(&path)
            .and_then(|f| f.take(len).read_to_end(&mut bytes))
            .map_err(|e| format!("cannot read {}: {e}", path.display()))?;
        Ok(String::from_utf8_lossy(&bytes).into_owned())
    }

    // Brings one session's cache up to date with its transcript and returns its meta and whether
    // anything was read. Only the bytes after the last complete line already read are parsed; a
    // transcript found shorter than it was at the last read (cut by another tool), one found at
    // another path, or a cache that cannot be trusted is rebuilt from the start. A transcript cut
    // and grown past its old size between two reads is not noticed.
    pub fn refresh(&self, id: &str, transcript: &Path) -> Res<(Meta, bool)> {
        let source = transcript.to_string_lossy().into_owned();
        // The size and the meta are both read under the lock: another process may just have
        // brought the cache up to date, and a size taken before that would look like a cut.
        let _lock = self.lock()?;
        let size = fs::metadata(transcript)
            .map_err(|e| format!("cannot stat {}: {e}", transcript.display()))?
            .len();
        let mut meta = match self.meta(id) {
            Some(m) if m.source != source || !self.bodies_intact(id, &m) => Meta::default(),
            Some(m) if size == m.size => return Ok((m, false)),
            Some(m) if size > m.size => m,
            _ => Meta::default(),
        };
        meta.source = source;
        let mut bodies = Bodies::default();
        let mut file = File::open(transcript)
            .map_err(|e| format!("cannot open {}: {e}", transcript.display()))?;
        file.seek(SeekFrom::Start(meta.offset))
            .map_err(|e| format!("cannot read {}: {e}", transcript.display()))?;
        let mut reader = BufReader::with_capacity(1 << 20, file);
        let mut buf = Vec::new();
        loop {
            buf.clear();
            let n = reader
                .read_until(b'\n', &mut buf)
                .map_err(|e| format!("cannot read {}: {e}", transcript.display()))?;
            // A last line without its newline may still be being written; it is read next time.
            if n == 0 || buf.last() != Some(&b'\n') {
                break;
            }
            meta.offset += n as u64;
            if let Ok(line) = std::str::from_utf8(&buf) {
                apply_line(&mut meta, &mut bodies, line);
            }
        }
        meta.size = size.max(meta.offset);
        for (t, text) in bodies.text.iter().enumerate() {
            let path = self.body_file(id, t);
            let mut f = OpenOptions::new()
                .append(true)
                .create(true)
                .mode(0o600)
                .open(&path)
                .map_err(|e| format!("cannot open {}: {e}", path.display()))?;
            // Cut back to what the old meta recorded: an earlier run may have died after writing
            // this body and before its meta.
            f.set_len(meta.lens[t])
                .and_then(|()| f.write_all(text.as_bytes()))
                .map_err(|e| format!("cannot write {}: {e}", path.display()))?;
            meta.lens[t] += text.len() as u64;
        }
        let tmp = self.file(id, "meta.json.tmp");
        let dest = self.file(id, "meta.json");
        OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(&tmp)
            .and_then(|mut f| f.write_all(to_json(&meta).to_string().as_bytes()))
            .and_then(|()| fs::rename(&tmp, &dest))
            .map_err(|e| format!("cannot write {}: {e}", dest.display()))?;
        Ok((meta, true))
    }

    // Removes the cache of every session whose transcript is gone; returns how many files went.
    pub fn sweep(&self, live: &HashSet<&str>) -> Res<usize> {
        const SUFFIXES: [&str; 5] = [
            ".meta.json.tmp",
            ".meta.json",
            ".user",
            ".assistant",
            ".tools",
        ];
        let entries = fs::read_dir(&self.dir)
            .map_err(|e| format!("cannot read {}: {e}", self.dir.display()))?;
        let mut removed = 0;
        for e in entries.flatten() {
            let name = e.file_name();
            let Some(name) = name.to_str() else { continue };
            let Some(stem) = SUFFIXES.iter().find_map(|s| name.strip_suffix(s)) else {
                continue;
            };
            if !live.contains(stem) && fs::remove_file(e.path()).is_ok() {
                removed += 1;
            }
        }
        Ok(removed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::unique_dir;
    use std::os::unix::fs::PermissionsExt;

    struct Fx {
        root: PathBuf,
        projects: PathBuf,
        index: Index,
    }

    impl Fx {
        fn new(name: &str) -> Fx {
            let root = unique_dir(name);
            let projects = root.join("projects");
            fs::create_dir_all(projects.join("-w-a")).unwrap();
            let index = Index::open(&root.join("state")).unwrap();
            Fx {
                root,
                projects,
                index,
            }
        }

        fn transcript(&self, id: &str) -> PathBuf {
            self.projects.join("-w-a").join(format!("{id}.jsonl"))
        }

        fn write(&self, id: &str, text: &str) {
            fs::write(self.transcript(id), text).unwrap();
        }

        fn append(&self, id: &str, text: &str) {
            let mut f = OpenOptions::new()
                .append(true)
                .open(self.transcript(id))
                .unwrap();
            f.write_all(text.as_bytes()).unwrap();
        }

        fn refresh(&self, id: &str) -> (Meta, bool) {
            self.index.refresh(id, &self.transcript(id)).unwrap()
        }

        fn body(&self, id: &str, tier: usize) -> String {
            let len = self.index.meta(id).unwrap().lens[tier];
            self.index.body(id, tier, len).unwrap()
        }
    }

    impl Drop for Fx {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.root);
        }
    }

    fn line(kind: &str, ts: &str, content: &str) -> String {
        format!(
            "{{\"type\":\"{kind}\",\"timestamp\":\"{ts}\",\"cwd\":\"/w/a\",\"message\":{{\"content\":{content}}}}}\n"
        )
    }

    const T1: &str = "2026-11-04T14:44:21.665Z";
    const T2: &str = "2026-11-05T09:00:00.000Z";

    #[test]
    fn transcripts_are_the_jsonl_files_one_level_down_and_nothing_deeper() {
        let fx = Fx::new("list");
        fs::write(fx.transcript("s1"), "").unwrap();
        fs::write(fx.projects.join("-w-a/notes.txt"), "").unwrap();
        fs::create_dir_all(fx.projects.join("-w-a/s1/subagents")).unwrap();
        fs::write(fx.projects.join("-w-a/s1/subagents/agent-x.jsonl"), "").unwrap();
        fs::write(fx.projects.join("top.jsonl"), "").unwrap();
        fs::create_dir_all(fx.projects.join("-w-b")).unwrap();
        fs::write(fx.projects.join("-w-b/s2.jsonl"), "").unwrap();
        let got = transcripts(&fx.projects).unwrap();
        let ids: Vec<_> = got.iter().map(|(id, _)| id.as_str()).collect();
        assert_eq!(ids, ["s1", "s2"]);
    }

    #[test]
    fn one_session_id_in_two_directories_is_listed_once_from_the_longer_file() {
        let fx = Fx::new("dup");
        fs::create_dir_all(fx.projects.join("-w-b")).unwrap();
        fs::write(fx.transcript("s1"), "ab").unwrap();
        fs::write(fx.projects.join("-w-b/s1.jsonl"), "abcd").unwrap();
        let got = transcripts(&fx.projects).unwrap();
        assert_eq!(got.len(), 1);
        assert!(got[0].1.ends_with("-w-b/s1.jsonl"), "{:?}", got[0].1);
    }

    #[test]
    fn a_missing_projects_directory_is_an_error_not_an_empty_list() {
        let fx = Fx::new("noproj");
        let err = transcripts(&fx.root.join("absent")).unwrap_err();
        assert!(err.contains("cannot read the projects directory"), "{err}");
    }

    #[test]
    fn a_first_refresh_reads_everything_and_writes_meta_and_three_bodies() {
        let fx = Fx::new("first");
        fx.write(
            "s1",
            &(line("user", T1, r#""Hello World""#)
                + &line("assistant", T2, r#"[{"type":"text","text":"Hi there"},{"type":"tool_use","name":"Bash","input":{"c":"ls"}}]"#)),
        );
        let (m, did) = fx.refresh("s1");
        assert!(did);
        assert_eq!(fx.body("s1", 0), "hello world\n");
        assert_eq!(fx.body("s1", 1), "hi there\n");
        assert_eq!(fx.body("s1", 2), "bash {\"c\":\"ls\"}\n");
        assert_eq!(m.lens, [12, 9, 16]);
        assert_eq!(m.offset, fs::metadata(fx.transcript("s1")).unwrap().len());
        assert_eq!(fx.index.meta("s1"), Some(m));
    }

    #[test]
    fn nothing_changed_means_nothing_is_read() {
        let fx = Fx::new("same");
        fx.write("s1", &line("user", T1, r#""a""#));
        let (m1, _) = fx.refresh("s1");
        let (m2, did) = fx.refresh("s1");
        assert!(!did);
        assert_eq!(m1, m2);
        assert!(fx.index.is_fresh("s1", &m2, &fx.transcript("s1")));
        fx.append("s1", &line("user", T2, r#""b""#));
        assert!(!fx.index.is_fresh("s1", &m2, &fx.transcript("s1")));
    }

    #[test]
    fn an_appended_transcript_reads_only_the_new_bytes_and_equals_a_full_rebuild() {
        let fx = Fx::new("append");
        let first = line("user", T1, r#""one""#);
        let second = line("assistant", T2, r#""two""#) + &line("user", T2, r#""three""#);
        fx.write("s1", &first);
        let (m1, _) = fx.refresh("s1");
        fx.append("s1", &second);
        let (m2, did) = fx.refresh("s1");
        assert!(did);
        assert_eq!(m1.offset, first.len() as u64);
        assert_eq!(m2.offset, (first.len() + second.len()) as u64);
        assert_eq!(fx.body("s1", 0), "one\nthree\n");
        assert_eq!(fx.body("s1", 1), "two\n");

        // The same transcript indexed from scratch elsewhere gives the same meta and bodies.
        let other = Fx::new("append-full");
        other.write("s1", &(first + &second));
        let (full, _) = other.refresh("s1");
        // Only where it was read from differs.
        let unplaced = |m: &Meta| Meta {
            source: String::new(),
            ..m.clone()
        };
        assert_ne!(m2.source, full.source);
        assert_eq!(unplaced(&m2), unplaced(&full));
        for t in 0..3 {
            assert_eq!(fx.body("s1", t), other.body("s1", t));
        }
    }

    #[test]
    fn a_partial_last_line_is_read_once_it_is_completed_not_lost_and_not_duplicated() {
        let fx = Fx::new("partial");
        let a = line("user", T1, r#""alpha""#);
        let b = line("user", T2, r#""beta""#);
        let cut = b.len() / 2;
        fx.write("s1", &(a.clone() + &b[..cut]));
        let (m, _) = fx.refresh("s1");
        assert_eq!(m.offset, a.len() as u64);
        assert_eq!(fx.body("s1", 0), "alpha\n");

        // The tail is still incomplete: reading again changes nothing.
        let (_, did) = fx.refresh("s1");
        assert!(!did);

        fx.append("s1", &b[cut..]);
        let (m, did) = fx.refresh("s1");
        assert!(did);
        assert_eq!(m.offset, (a.len() + b.len()) as u64);
        assert_eq!(fx.body("s1", 0), "alpha\nbeta\n");
        assert_eq!(m.last_ms, crate::extract::parse_ms(T2));
    }

    #[test]
    fn a_partial_tail_that_grows_but_is_still_incomplete_is_reread_without_duplicates() {
        let fx = Fx::new("partial2");
        let a = line("user", T1, r#""alpha""#);
        let b = line("user", T2, r#""beta""#);
        fx.write("s1", &(a.clone() + &b[..10]));
        fx.refresh("s1");
        fx.append("s1", &b[10..20]);
        let (_, did) = fx.refresh("s1");
        assert!(did);
        assert_eq!(fx.body("s1", 0), "alpha\n");
        fx.append("s1", &b[20..]);
        fx.refresh("s1");
        assert_eq!(fx.body("s1", 0), "alpha\nbeta\n");
    }

    #[test]
    fn a_transcript_cut_below_the_read_offset_is_rebuilt_from_the_start() {
        let fx = Fx::new("truncate");
        let a = line("user", T1, r#""kept""#);
        let b = line("user", T2, r#""dropped""#);
        fx.write("s1", &(a.clone() + &b));
        fx.refresh("s1");
        assert_eq!(fx.body("s1", 0), "kept\ndropped\n");
        fx.write("s1", &a);
        let (m, did) = fx.refresh("s1");
        assert!(did);
        assert_eq!(fx.body("s1", 0), "kept\n");
        assert_eq!((m.offset, m.size), (a.len() as u64, a.len() as u64));
        assert_eq!(m.last_ms, crate::extract::parse_ms(T1));
    }

    #[test]
    fn a_rebuilt_session_does_not_keep_the_old_title() {
        let fx = Fx::new("rebuild-meta");
        fx.write(
            "s1",
            &(line("user", T1, r#""old""#)
                + "{\"type\":\"custom-title\",\"customTitle\":\"Old title\"}\n"),
        );
        fx.refresh("s1");
        let short = line("user", T2, r#""n""#);
        fx.write("s1", &short);
        let (m, _) = fx.refresh("s1");
        assert_eq!(m.custom_title, "");
    }

    #[test]
    fn a_body_file_longer_than_its_meta_records_is_cut_back_before_appending() {
        let fx = Fx::new("cutback");
        fx.write("s1", &line("user", T1, r#""one""#));
        let (m, _) = fx.refresh("s1");
        // A run died after appending to the body and before writing its meta.
        let mut f = OpenOptions::new()
            .append(true)
            .open(fx.index.body_file("s1", 0))
            .unwrap();
        f.write_all(b"orphan from a dead run\n").unwrap();
        assert!(fx.index.bodies_intact("s1", &m));
        fx.append("s1", &line("user", T2, r#""two""#));
        fx.refresh("s1");
        assert_eq!(fx.body("s1", 0), "one\ntwo\n");
        assert_eq!(
            fs::read_to_string(fx.index.body_file("s1", 0)).unwrap(),
            "one\ntwo\n"
        );
    }

    #[test]
    fn a_body_file_shorter_than_its_meta_records_rebuilds_the_session() {
        let fx = Fx::new("short");
        fx.write(
            "s1",
            &(line("user", T1, r#""one""#) + &line("user", T2, r#""two""#)),
        );
        fx.refresh("s1");
        fs::write(fx.index.body_file("s1", 0), "on").unwrap();
        let m = fx.index.meta("s1").unwrap();
        assert!(!fx.index.is_fresh("s1", &m, &fx.transcript("s1")));
        let (_, did) = fx.refresh("s1");
        assert!(did);
        assert_eq!(fx.body("s1", 0), "one\ntwo\n");
    }

    #[test]
    fn a_meta_from_another_version_or_a_broken_one_is_rebuilt() {
        let fx = Fx::new("version");
        fx.write("s1", &line("user", T1, r#""one""#));
        fx.refresh("s1");
        let meta_path = fx.index.file("s1", "meta.json");
        let text = fs::read_to_string(&meta_path).unwrap();
        fs::write(&meta_path, text.replace("\"version\":1", "\"version\":99")).unwrap();
        assert_eq!(fx.index.meta("s1"), None);
        let (_, did) = fx.refresh("s1");
        assert!(did);
        assert_eq!(fx.body("s1", 0), "one\n");
        fs::write(&meta_path, "{broken").unwrap();
        assert_eq!(fx.index.meta("s1"), None);
        assert!(fx.refresh("s1").1);
    }

    #[test]
    fn the_cache_of_a_removed_transcript_is_swept_and_a_live_one_is_kept() {
        let fx = Fx::new("sweep");
        fx.write("s1", &line("user", T1, r#""a""#));
        fx.write("s2", &line("user", T1, r#""b""#));
        fx.refresh("s1");
        fx.refresh("s2");
        fs::write(fx.index.file("s2", "meta.json.tmp"), "x").unwrap();
        fs::write(fx.index.dir.join("unrelated.txt"), "keep").unwrap();
        let removed = fx.index.sweep(&HashSet::from(["s1"])).unwrap();
        assert_eq!(removed, 5);
        assert_eq!(fx.index.meta("s2"), None);
        assert!(fx.index.meta("s1").is_some());
        assert!(fx.index.body_file("s1", 2).exists());
        assert!(!fx.index.body_file("s2", 2).exists());
        assert!(fx.index.dir.join("unrelated.txt").exists());
    }

    #[test]
    fn the_index_directory_is_0700_and_every_file_in_it_is_0600() {
        let fx = Fx::new("modes");
        fx.write("s1", &line("user", T1, r#""secret text""#));
        fx.refresh("s1");
        let mode = |p: &Path| fs::metadata(p).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode(&fx.index.dir), 0o700);
        let mut files = 0;
        for e in fs::read_dir(&fx.index.dir).unwrap().flatten() {
            assert_eq!(mode(&e.path()), 0o600, "{:?}", e.path());
            files += 1;
        }
        // The meta, the three bodies and the lock file.
        assert_eq!(files, 5);
    }

    #[test]
    fn an_index_directory_left_open_to_others_is_closed_again_on_open() {
        let fx = Fx::new("reopen");
        fs::set_permissions(&fx.index.dir, fs::Permissions::from_mode(0o755)).unwrap();
        let again = Index::open(fx.index.dir.parent().unwrap()).unwrap();
        let mode = fs::metadata(&again.dir).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o700);
    }

    #[test]
    fn a_refresh_waits_while_another_process_holds_the_index_lock() {
        let fx = Fx::new("locked");
        fx.write("s1", &line("user", T1, r#""one""#));
        let held = fx.index.lock().unwrap();
        let (index, path) = (fx.index.clone(), fx.transcript("s1"));
        let (tx, rx) = std::sync::mpsc::channel();
        let worker = std::thread::spawn(move || {
            let _ = tx.send(index.refresh("s1", &path).map(|(m, _)| m.offset));
        });
        let early = rx.recv_timeout(std::time::Duration::from_millis(300));
        assert!(early.is_err(), "refreshed under a held lock: {early:?}");
        drop(held);
        let done = rx.recv_timeout(std::time::Duration::from_secs(10)).unwrap();
        worker.join().unwrap();
        assert_eq!(done, Ok(line("user", T1, r#""one""#).len() as u64));
    }

    #[test]
    fn a_refresh_that_waited_for_another_writer_sees_the_transcript_as_it_is_then() {
        let fx = Fx::new("waited");
        let one = line("user", T1, r#""one""#);
        let both = one.clone() + &line("user", T2, r#""two""#);
        // What another process leaves after indexing the two-line transcript.
        fx.write("s1", &both);
        fx.refresh("s1");
        let names = ["s1.meta.json", "s1.user", "s1.assistant", "s1.tools"];
        let cache: Vec<Vec<u8>> = names
            .iter()
            .map(|n| fs::read(fx.index.dir.join(n)).unwrap())
            .collect();
        for n in names {
            fs::remove_file(fx.index.dir.join(n)).unwrap();
        }
        fx.write("s1", &one);

        let held = fx.index.lock().unwrap();
        let (index, path) = (fx.index.clone(), fx.transcript("s1"));
        let (tx, rx) = std::sync::mpsc::channel();
        let worker = std::thread::spawn(move || {
            let _ = tx.send(index.refresh("s1", &path));
        });
        assert!(rx
            .recv_timeout(std::time::Duration::from_millis(300))
            .is_err());
        // While it waits, the transcript grows and the other writer indexes it.
        fx.write("s1", &both);
        for (n, bytes) in names.iter().zip(&cache) {
            fs::write(fx.index.dir.join(n), bytes).unwrap();
        }
        drop(held);
        let (m, did) = rx
            .recv_timeout(std::time::Duration::from_secs(10))
            .unwrap()
            .unwrap();
        worker.join().unwrap();
        assert!(!did, "it read again what the other writer had just indexed");
        assert_eq!(m.offset, both.len() as u64);
        assert_eq!(fx.body("s1", 0), "one\ntwo\n");
    }

    #[test]
    fn the_same_id_found_at_another_path_is_read_from_the_start_not_continued() {
        let fx = Fx::new("moved");
        fx.write("s1", &line("user", T1, r#""old place""#));
        fx.refresh("s1");
        let other = fx.projects.join("-w-b");
        fs::create_dir_all(&other).unwrap();
        let moved = other.join("s1.jsonl");
        // The same length as the first file: only the path tells them apart.
        let text = line("user", T2, r#""new place""#);
        fs::write(&moved, &text).unwrap();
        let cached = fx.index.meta("s1").unwrap();
        assert_eq!(cached.size, text.len() as u64);
        assert!(!fx.index.is_fresh("s1", &cached, &moved));
        let (m, did) = fx.index.refresh("s1", &moved).unwrap();
        assert!(did);
        assert_eq!(m.offset, text.len() as u64);
        assert_eq!(fx.body("s1", 0), "new place\n");
    }

    #[test]
    fn of_two_equally_long_transcripts_with_one_id_the_lower_path_is_taken() {
        let fx = Fx::new("tie");
        for dir in ["-w-z", "-w-m", "-w-q"] {
            fs::create_dir_all(fx.projects.join(dir)).unwrap();
            fs::write(fx.projects.join(dir).join("dup.jsonl"), "12345").unwrap();
        }
        let all = transcripts(&fx.projects).unwrap();
        let (_, path) = all.iter().find(|(id, _)| id == "dup").unwrap();
        assert_eq!(path, &fx.projects.join("-w-m").join("dup.jsonl"));
    }

    #[test]
    fn a_transcript_that_shrank_but_not_below_the_read_offset_is_still_rebuilt() {
        let fx = Fx::new("shrunk");
        let first = line("user", T1, r#""one""#);
        // The read stops at the end of the first line; the rest is a line still being written.
        fx.write("s1", &(first.clone() + "{\"type\":\"user\",\"unfinished"));
        let (m1, _) = fx.refresh("s1");
        assert_eq!(m1.offset, first.len() as u64);
        let rewritten = line("user", T2, r#""two""#);
        assert_eq!(rewritten.len(), first.len());
        fx.write("s1", &rewritten);
        let (m2, did) = fx.refresh("s1");
        assert!(did);
        assert_eq!(m2.offset, rewritten.len() as u64);
        assert_eq!(fx.body("s1", 0), "two\n");
    }

    #[test]
    fn a_session_with_no_user_or_assistant_line_is_not_listed_but_is_not_rescanned() {
        let fx = Fx::new("unlisted");
        fx.write(
            "s1",
            "{\"type\":\"attachment\"}\n{\"type\":\"ai-title\",\"aiTitle\":\"T\"}\n",
        );
        let (m, did) = fx.refresh("s1");
        assert!(did && !m.listed());
        assert_eq!(m.ai_title, "T");
        assert!(!fx.refresh("s1").1);
    }

    #[test]
    fn a_body_read_is_limited_to_the_length_the_meta_recorded() {
        let fx = Fx::new("limit");
        fx.write("s1", &line("user", T1, r#""one""#));
        fx.refresh("s1");
        let mut f = OpenOptions::new()
            .append(true)
            .open(fx.index.body_file("s1", 0))
            .unwrap();
        f.write_all(b"half-written").unwrap();
        assert_eq!(fx.body("s1", 0), "one\n");
        assert!(fx
            .index
            .body("absent", 0, 5)
            .unwrap_err()
            .contains("cannot read"));
    }
}
