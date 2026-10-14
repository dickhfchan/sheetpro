//! Autosave and crash recovery.
//!
//! A dirty session is snapshotted every `autosave_interval_secs` to
//! `<app dir>/autosave/<id>.xlsx` plus a `<id>.toml` sidecar (original path, title, time,
//! process id). Writes are atomic (temp file + rename); the directory is bounded by count
//! and total size (oldest snapshots go first). A clean save, a clean close or discarding the
//! changes deletes the session's snapshot, so whatever is left at the next launch belongs
//! to a session that ended without saving: the app offers to recover it.

use crate::Session;
use crate::settings::{unix_now, write_atomic};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// Snapshot directory bounds.
pub const MAX_SNAPSHOTS: usize = 20;
pub const MAX_TOTAL_BYTES: u64 = 256 << 20;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct SnapshotMeta {
    pub id: String,
    /// The workbook's own file, if it had one.
    pub original: Option<String>,
    pub title: String,
    pub saved_at: u64,
    pub pid: u32,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Snapshot {
    pub meta: SnapshotMeta,
    /// The `.xlsx` holding the snapshot.
    pub data: PathBuf,
    pub bytes: u64,
}

pub fn autosave_dir(app_dir: &Path) -> PathBuf {
    app_dir.join("autosave")
}

fn valid_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 64
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

/// Write a snapshot of `wb` as `id` (replacing that id's previous snapshot), then prune.
pub fn write_snapshot(
    dir: &Path,
    id: &str,
    wb: &sp_model::Workbook,
    original: Option<&Path>,
    title: &str,
) -> Result<PathBuf, String> {
    if !valid_id(id) {
        return Err(format!("invalid snapshot id '{id}'"));
    }
    std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    let data = dir.join(format!("{id}.xlsx"));
    // sp_io::save is itself atomic (temp file + rename).
    sp_io::save(wb, &data).map_err(|e| e.to_string())?;
    let meta = SnapshotMeta {
        id: id.to_string(),
        original: original.map(|p| p.display().to_string()),
        title: title.to_string(),
        saved_at: unix_now(),
        pid: std::process::id(),
    };
    let text = toml::to_string(&meta).map_err(|e| e.to_string())?;
    write_atomic(&dir.join(format!("{id}.toml")), text.as_bytes())?;
    prune(dir, MAX_SNAPSHOTS, MAX_TOTAL_BYTES);
    Ok(data)
}

/// Every complete snapshot in `dir`, newest first. Orphan files are ignored.
pub fn list(dir: &Path) -> Vec<Snapshot> {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for e in rd.flatten() {
        let p = e.path();
        if p.extension().and_then(|x| x.to_str()) != Some("toml") {
            continue;
        }
        let Some(meta) = std::fs::read_to_string(&p)
            .ok()
            .filter(|t| t.len() < 64 * 1024)
            .and_then(|t| toml::from_str::<SnapshotMeta>(&t).ok())
        else {
            continue;
        };
        if !valid_id(&meta.id) {
            continue;
        }
        let data = dir.join(format!("{}.xlsx", meta.id));
        let Ok(md) = std::fs::metadata(&data) else {
            continue;
        };
        out.push(Snapshot {
            meta,
            data,
            bytes: md.len(),
        });
    }
    out.sort_by(|a, b| {
        b.meta
            .saved_at
            .cmp(&a.meta.saved_at)
            .then_with(|| b.meta.id.cmp(&a.meta.id))
    });
    out
}

/// Delete a snapshot (both files). Missing files are fine.
pub fn discard(dir: &Path, id: &str) {
    if !valid_id(id) {
        return;
    }
    let _ = std::fs::remove_file(dir.join(format!("{id}.xlsx")));
    let _ = std::fs::remove_file(dir.join(format!("{id}.toml")));
}

/// Keep at most `max_count` snapshots and `max_bytes` in total, dropping the oldest; also
/// removes leftover temp files and data files without a sidecar.
pub fn prune(dir: &Path, max_count: usize, max_bytes: u64) {
    let snaps = list(dir);
    let mut total = 0u64;
    for (i, s) in snaps.iter().enumerate() {
        total = total.saturating_add(s.bytes);
        // Always keep the newest one, whatever its size.
        if i > 0 && (i >= max_count || total > max_bytes) {
            discard(dir, &s.meta.id);
        }
    }
    let keep: Vec<String> = list(dir).into_iter().map(|s| s.meta.id).collect();
    if let Ok(rd) = std::fs::read_dir(dir) {
        for e in rd.flatten() {
            let p = e.path();
            let stem = p
                .file_name()
                .and_then(|n| n.to_str())
                .and_then(|n| n.split('.').next())
                .unwrap_or_default()
                .to_string();
            let is_tmp = p
                .file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.ends_with(".tmp") || n.ends_with(".sp-tmp"));
            let is_data = p.extension().and_then(|x| x.to_str()) == Some("xlsx");
            // Temp files of a live writer are young; only clear stale ones.
            let stale = e
                .metadata()
                .and_then(|m| m.modified())
                .ok()
                .and_then(|t| t.elapsed().ok())
                .is_some_and(|age| age > Duration::from_secs(600));
            if (is_tmp && stale) || (is_data && !keep.contains(&stem)) {
                let _ = std::fs::remove_file(&p);
            }
        }
    }
}

/// Snapshots left by sessions that are no longer running (candidates for recovery).
pub fn recoverable(dir: &Path) -> Vec<Snapshot> {
    let me = std::process::id();
    list(dir)
        .into_iter()
        .filter(|s| s.meta.pid != me && !pid_alive(s.meta.pid))
        .collect()
}

/// Best effort: is another process with this id running? (Unix: `kill -0`.)
fn pid_alive(pid: u32) -> bool {
    if pid == 0 {
        return false;
    }
    if cfg!(unix) {
        std::process::Command::new("kill")
            .args(["-0", &pid.to_string()])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .is_ok_and(|s| s.success())
    } else {
        false
    }
}

/// Open a snapshot as a dirty session that points at the workbook's original file (so Save
/// overwrites it), or at no file when it never had one.
pub fn recover(snap: &Snapshot) -> Result<Session, String> {
    let (wb, _) = sp_io::load(&snap.data).map_err(|e| e.to_string())?;
    let mut s = Session::with_workbook(wb);
    s.path = snap.meta.original.as_ref().map(PathBuf::from);
    s.dirty = true;
    s.status = format!(
        "Recovered unsaved changes from autosave ({})",
        snap.meta.title
    );
    Ok(s)
}

/// Cheap "did anything change" marker for a session.
fn fingerprint(s: &Session) -> String {
    let total: usize = s.history.summary().values().sum();
    format!(
        "{:?}|{:?}|{}|{:?}",
        s.history.undo_label(),
        s.last_command,
        total,
        s.path
    )
}

/// What [`Autosaver::tick`] decided.
#[derive(Debug, Clone, PartialEq)]
pub enum Tick {
    Idle,
    /// A snapshot is due: write `wb` (cloned by the caller, possibly on another thread)
    /// with [`write_snapshot`] under this id.
    Snapshot {
        id: String,
    },
    /// The session is clean again: its snapshot was deleted.
    Discarded {
        id: String,
    },
}

/// Per-window autosave scheduler.
#[derive(Debug)]
pub struct Autosaver {
    pub id: String,
    pub dir: PathBuf,
    pub interval: Duration,
    last: Option<Instant>,
    last_print: Option<String>,
    has_snapshot: bool,
}

impl Autosaver {
    pub fn new(dir: PathBuf, id: String, interval: Duration) -> Self {
        Autosaver {
            id,
            dir,
            interval,
            last: None,
            last_print: None,
            has_snapshot: false,
        }
    }

    /// Call often (each frame is fine). Decides whether a snapshot is due; the caller
    /// writes it (and should call [`Autosaver::snapshot_now`] for a synchronous write).
    pub fn tick(&mut self, s: &Session, now: Instant) -> Tick {
        if !s.dirty {
            self.last = Some(now);
            if self.has_snapshot {
                discard(&self.dir, &self.id);
                self.has_snapshot = false;
                self.last_print = None;
                return Tick::Discarded {
                    id: self.id.clone(),
                };
            }
            return Tick::Idle;
        }
        if self.interval.is_zero() {
            return Tick::Idle;
        }
        let since = self.last.get_or_insert(now);
        if now.saturating_duration_since(*since) < self.interval {
            return Tick::Idle;
        }
        let print = fingerprint(s);
        self.last = Some(now);
        if self.last_print.as_ref() == Some(&print) && self.has_snapshot {
            return Tick::Idle;
        }
        self.last_print = Some(print);
        self.has_snapshot = true;
        Tick::Snapshot {
            id: self.id.clone(),
        }
    }

    /// Write this session's snapshot now (synchronously).
    pub fn snapshot_now(&mut self, s: &Session) -> Result<PathBuf, String> {
        self.has_snapshot = true;
        self.last_print = Some(fingerprint(s));
        write_snapshot(&self.dir, &self.id, &s.wb, s.path.as_deref(), &s.title())
    }

    /// The window closed cleanly (saved or changes discarded): drop its snapshot.
    pub fn finish(&mut self) {
        discard(&self.dir, &self.id);
        self.has_snapshot = false;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn temp_dir(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!(
            "sp-autosave-{tag}-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&d);
        d
    }

    fn edited() -> Session {
        let mut s = Session::new();
        s.execute("cell.set", &json!({"cell": "A1", "input": "42"}))
            .unwrap();
        s.execute("cell.set", &json!({"cell": "A2", "input": "=A1*2"}))
            .unwrap();
        s
    }

    #[test]
    fn snapshot_list_recover_discard() {
        let dir = temp_dir("basic");
        let s = edited();
        let p = write_snapshot(&dir, "w1", &s.wb, Some(Path::new("/x/book.xlsx")), "book").unwrap();
        assert!(p.exists());
        let l = list(&dir);
        assert_eq!(l.len(), 1);
        assert_eq!(l[0].meta.original.as_deref(), Some("/x/book.xlsx"));
        // Our own pid: not offered for recovery while we run.
        assert!(recoverable(&dir).is_empty());
        let r = recover(&l[0]).unwrap();
        assert!(r.dirty);
        assert_eq!(r.path, Some(PathBuf::from("/x/book.xlsx")));
        assert_eq!(
            r.wb.sheets[0].value(sp_core::CellAddr::new(1, 0)),
            &sp_core::Value::Number(84.0)
        );
        discard(&dir, "w1");
        assert!(list(&dir).is_empty());
        assert!(write_snapshot(&dir, "../evil", &s.wb, None, "x").is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn dead_process_snapshots_are_recoverable() {
        let dir = temp_dir("dead");
        let s = edited();
        write_snapshot(&dir, "old", &s.wb, None, "Book1").unwrap();
        // Rewrite the sidecar as if a crashed process (pid 0 never runs) wrote it.
        let meta = SnapshotMeta {
            id: "old".into(),
            original: None,
            title: "Book1".into(),
            saved_at: 1,
            pid: 0,
        };
        std::fs::write(dir.join("old.toml"), toml::to_string(&meta).unwrap()).unwrap();
        let r = recoverable(&dir);
        assert_eq!(r.len(), 1);
        assert!(recover(&r[0]).unwrap().path.is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn prune_bounds_count_and_orphans() {
        let dir = temp_dir("prune");
        let s = edited();
        for i in 0..5 {
            write_snapshot(&dir, &format!("s{i}"), &s.wb, None, "b").unwrap();
        }
        std::fs::write(dir.join("orphan.xlsx"), b"x").unwrap();
        std::fs::write(dir.join("junk.toml"), b"not = [valid").unwrap();
        prune(&dir, 2, u64::MAX);
        assert_eq!(list(&dir).len(), 2);
        assert!(!dir.join("orphan.xlsx").exists());
        prune(&dir, 10, 1);
        assert_eq!(
            list(&dir).len(),
            1,
            "newest kept even when over the byte cap"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn autosaver_schedules_and_cleans_up() {
        let dir = temp_dir("sched");
        let mut a = Autosaver::new(dir.clone(), "win1".into(), Duration::from_secs(10));
        let t0 = Instant::now();
        let mut s = Session::new();
        assert_eq!(a.tick(&s, t0), Tick::Idle);
        s.execute("cell.set", &json!({"cell": "A1", "input": "1"}))
            .unwrap();
        assert_eq!(a.tick(&s, t0 + Duration::from_secs(5)), Tick::Idle);
        let t = a.tick(&s, t0 + Duration::from_secs(11));
        assert_eq!(t, Tick::Snapshot { id: "win1".into() });
        a.snapshot_now(&s).unwrap();
        assert_eq!(list(&dir).len(), 1);
        // Nothing changed: no new snapshot.
        assert_eq!(a.tick(&s, t0 + Duration::from_secs(30)), Tick::Idle);
        s.execute("cell.set", &json!({"cell": "A1", "input": "2"}))
            .unwrap();
        assert!(matches!(
            a.tick(&s, t0 + Duration::from_secs(45)),
            Tick::Snapshot { .. }
        ));
        // Clean save → snapshot deleted.
        let book = dir.join("saved.xlsx");
        s.execute("file.save_as", &json!({"path": book.display().to_string()}))
            .unwrap();
        assert!(matches!(
            a.tick(&s, t0 + Duration::from_secs(50)),
            Tick::Discarded { .. }
        ));
        assert!(list(&dir).is_empty());
        // Interval 0 = off.
        let mut off = Autosaver::new(dir.clone(), "w2".into(), Duration::ZERO);
        s.execute("cell.set", &json!({"cell": "A1", "input": "3"}))
            .unwrap();
        assert_eq!(off.tick(&s, t0 + Duration::from_secs(999)), Tick::Idle);
        off.finish();
        let _ = std::fs::remove_dir_all(&dir);
    }
}
