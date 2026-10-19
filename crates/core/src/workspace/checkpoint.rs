//! Per-turn pre-images of the files an agent turn changes, and `/undo`.
//!
//! Layout beside `nexus.db`: `checkpoints/<session_id>/<turn_seq>/` holds
//! `manifest.json` (the workspace root and the changed files) and one
//! `<n>.pre` copy per file that existed before the turn first changed it. A
//! turn that changes nothing leaves no directory.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use sha2::{Digest, Sha256};

use super::{Access, PathError, Workspace};

/// Pre-images over this size are not stored; their changes cannot be undone.
const MAX_PRE_IMAGE: u64 = 10 * 1024 * 1024;

/// The checkpoint store and the turn currently being recorded.
pub(super) struct Checkpoints {
    dir: PathBuf,
    turn: Mutex<Option<Turn>>,
}

impl Checkpoints {
    pub(super) fn new(dir: PathBuf) -> Self {
        Self {
            dir,
            turn: Mutex::new(None),
        }
    }
}

struct Turn {
    session_id: String,
    /// Created on the turn's first change.
    dir: Option<PathBuf>,
    /// Entries in first-touch order, with whether the change completed.
    entries: Vec<(Entry, bool)>,
}

/// One file the turn changed, as stored in `manifest.json`.
#[derive(serde::Serialize, serde::Deserialize)]
struct Entry {
    /// Relative to the workspace root.
    path: String,
    existed: bool,
    /// File name of the pre-image in the turn directory.
    pre_image: Option<String>,
    /// The file's sha256 after the turn's last change; `None` when deleted.
    post_sha256: Option<String>,
}

/// A turn's `manifest.json`: the canonical root its paths are relative to.
#[derive(serde::Deserialize)]
struct Manifest {
    root: PathBuf,
    entries: Vec<Entry>,
}

/// What undoing one manifest entry will do, decided before anything is written.
enum Verdict {
    Restore { path: PathBuf, pre_image: PathBuf },
    Remove(PathBuf),
}

/// What `/undo` put back and what it left alone.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct UndoReport {
    pub restored: Vec<String>,
    pub skipped_changed_by_user: Vec<String>,
    pub removed_created: Vec<String>,
    /// Changed files whose pre-image exceeded the size guard.
    pub skipped_too_large: Vec<String>,
    /// Files whose restore or removal failed, with the error.
    pub failed: Vec<(String, String)>,
    /// Set when the latest checkpoint belongs to another workspace root; then
    /// nothing was touched and the checkpoint is kept.
    pub wrong_root: Option<PathBuf>,
}

impl UndoReport {
    /// The status line reporting this undo.
    pub fn summary(&self) -> String {
        if let Some(root) = &self.wrong_root {
            return format!(
                "the last agent turn changed files in {root} · switch back with /agent {root} to undo it",
                root = root.display()
            );
        }
        let mut line = format!(
            "undid {} file changes from the last agent turn · {} skipped (changed since",
            self.restored.len() + self.removed_created.len(),
            self.skipped_changed_by_user.len()
        );
        if !self.skipped_changed_by_user.is_empty() {
            let _ = write!(line, ": {}", self.skipped_changed_by_user.join(", "));
        }
        line.push(')');
        if !self.skipped_too_large.is_empty() {
            let _ = write!(
                line,
                " · {} not restorable (over 10 MiB: {})",
                self.skipped_too_large.len(),
                self.skipped_too_large.join(", ")
            );
        }
        if !self.failed.is_empty() {
            let failed: Vec<String> = self
                .failed
                .iter()
                .map(|(path, error)| format!("{path}: {error}"))
                .collect();
            let _ = write!(
                line,
                " · {} failed ({})",
                self.failed.len(),
                failed.join(", ")
            );
        }
        line.push_str(" · shell commands are not undone");
        line
    }
}

impl Workspace {
    /// Record agent file changes under `dir` (checkpoints are off otherwise).
    #[must_use]
    pub fn with_checkpoints(mut self, dir: PathBuf) -> Self {
        self.checkpoints = Some(Checkpoints::new(dir));
        self
    }

    /// Start recording a new agent turn for `session_id`.
    pub fn begin_turn(&self, session_id: &str) {
        let Some(checkpoints) = &self.checkpoints else {
            return;
        };
        let mut turn = checkpoints.turn.lock().expect("checkpoint lock");
        if let Some(previous) = turn.take()
            && let Some(dir) = previous.dir
            && !previous.entries.iter().any(|(_, done)| *done)
        {
            let _ = std::fs::remove_dir_all(dir);
        }
        *turn = Some(Turn {
            session_id: session_id.to_string(),
            dir: None,
            entries: Vec::new(),
        });
    }

    /// Keep the pre-image of a resolved path the first time this turn touches it.
    pub(crate) fn before_change(&self, path: &Path) -> Result<(), PathError> {
        let Some(checkpoints) = &self.checkpoints else {
            return Ok(());
        };
        let mut guard = checkpoints.turn.lock().expect("checkpoint lock");
        let Some(turn) = guard.as_mut() else {
            return Ok(());
        };
        let rel = self.checkpoint_rel(path);
        if turn.entries.iter().any(|(entry, _)| entry.path == rel) {
            return Ok(());
        }
        let dir = if let Some(dir) = &turn.dir {
            dir.clone()
        } else {
            let session_dir = checkpoints.dir.join(&turn.session_id);
            let dir = session_dir.join((latest_seq(&session_dir)?.unwrap_or(0) + 1).to_string());
            std::fs::create_dir_all(&dir)?;
            turn.dir = Some(dir.clone());
            dir
        };
        let metadata = match std::fs::metadata(path) {
            Ok(metadata) => Some(metadata),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
            Err(error) => return Err(error.into()),
        };
        let pre_image = match &metadata {
            Some(metadata) if metadata.len() <= MAX_PRE_IMAGE => {
                let name = format!("{}.pre", turn.entries.len());
                std::fs::copy(path, dir.join(&name))?;
                Some(name)
            }
            _ => None,
        };
        turn.entries.push((
            Entry {
                path: rel,
                existed: metadata.is_some(),
                pre_image,
                post_sha256: None,
            },
            false,
        ));
        Ok(())
    }

    /// Record a path's contents after a completed change. A manifest that
    /// cannot be written leaves the change out of `/undo`, never wrongly in it.
    pub(crate) fn after_change(&self, path: &Path) {
        let Some(checkpoints) = &self.checkpoints else {
            return;
        };
        let mut guard = checkpoints.turn.lock().expect("checkpoint lock");
        let Some(turn) = guard.as_mut() else {
            return;
        };
        let rel = self.checkpoint_rel(path);
        let Some((entry, done)) = turn.entries.iter_mut().find(|(entry, _)| entry.path == rel)
        else {
            return;
        };
        let Ok(post) = file_sha256(path) else {
            return;
        };
        entry.post_sha256 = post;
        *done = true;
        let manifest: Vec<&Entry> = turn
            .entries
            .iter()
            .filter(|(_, done)| *done)
            .map(|(entry, _)| entry)
            .collect();
        let manifest = serde_json::json!({ "root": self.root, "entries": manifest });
        if let (Some(dir), Ok(json)) = (&turn.dir, serde_json::to_vec_pretty(&manifest)) {
            let _ = std::fs::write(dir.join("manifest.json"), json);
        }
    }

    /// Whether the turn recorded last for `session_id` changed any file.
    pub fn turn_changed_files(&self, session_id: &str) -> bool {
        self.checkpoints.as_ref().is_some_and(|checkpoints| {
            checkpoints
                .turn
                .lock()
                .expect("checkpoint lock")
                .as_ref()
                .is_some_and(|turn| {
                    turn.session_id == session_id && turn.entries.iter().any(|(_, done)| *done)
                })
        })
    }

    /// Restore every file the latest checkpointed turn of `session_id` changed
    /// and the user has not changed since, then drop that checkpoint so the
    /// next undo goes one turn further back. `None` means nothing to undo; a
    /// checkpoint taken in another root is reported and left untouched.
    pub fn undo_last(&self, session_id: &str) -> Result<Option<UndoReport>, PathError> {
        let Some(checkpoints) = &self.checkpoints else {
            return Ok(None);
        };
        let mut turn = checkpoints.turn.lock().expect("checkpoint lock");
        let session_dir = checkpoints.dir.join(session_id);
        while let Some(seq) = latest_seq(&session_dir)? {
            let dir = session_dir.join(seq.to_string());
            let manifest = match std::fs::read(dir.join("manifest.json")) {
                Ok(json) => serde_json::from_slice::<Manifest>(&json)
                    .map_err(|error| PathError::Io(error.to_string()))?,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => Manifest {
                    root: self.root.clone(),
                    entries: Vec::new(),
                },
                Err(error) => return Err(error.into()),
            };
            if manifest.root != self.root {
                return Ok(Some(UndoReport {
                    wrong_root: Some(manifest.root),
                    ..UndoReport::default()
                }));
            }
            if turn
                .as_ref()
                .is_some_and(|turn| turn.dir.as_ref() == Some(&dir))
            {
                *turn = None;
            }
            if manifest.entries.is_empty() {
                std::fs::remove_dir_all(&dir)?;
                continue;
            }
            let mut report = UndoReport::default();
            let verdicts: Vec<(String, Verdict)> = manifest
                .entries
                .into_iter()
                .filter_map(|entry| self.undo_verdict(&dir, entry, &mut report))
                .collect();
            for (rel, verdict) in verdicts {
                let (path, result, done) = match verdict {
                    Verdict::Restore { path, pre_image } => {
                        let result = restore(&pre_image, &path);
                        (path, result, &mut report.restored)
                    }
                    Verdict::Remove(path) => {
                        let result = std::fs::remove_file(&path).map_err(PathError::from);
                        (path, result, &mut report.removed_created)
                    }
                };
                match result {
                    Ok(()) => {
                        self.forget_read(&path);
                        done.push(rel);
                    }
                    Err(error) => report.failed.push((rel, error.to_string())),
                }
            }
            std::fs::remove_dir_all(&dir)?;
            return Ok(Some(report));
        }
        Ok(None)
    }

    /// Decide what undoing one entry does, without writing; skips are
    /// recorded in `report` and yield `None`.
    fn undo_verdict(
        &self,
        dir: &Path,
        entry: Entry,
        report: &mut UndoReport,
    ) -> Option<(String, Verdict)> {
        let Ok(path) = self.resolve(&entry.path, Access::Write) else {
            report.skipped_changed_by_user.push(entry.path);
            return None;
        };
        if !matches!(file_sha256(&path), Ok(current) if current == entry.post_sha256) {
            report.skipped_changed_by_user.push(entry.path);
            return None;
        }
        let verdict = match (entry.existed, entry.pre_image) {
            (false, _) if entry.post_sha256.is_some() => Verdict::Remove(path),
            (false, _) => return None,
            (true, None) => {
                report.skipped_too_large.push(entry.path);
                return None;
            }
            (true, Some(name)) => Verdict::Restore {
                path,
                pre_image: dir.join(name),
            },
        };
        Some((entry.path, verdict))
    }

    fn checkpoint_rel(&self, path: &Path) -> String {
        path.strip_prefix(&self.root)
            .expect("confined workspace path")
            .to_string_lossy()
            .into_owned()
    }
}

/// The highest turn number stored for a session, if any.
fn latest_seq(session_dir: &Path) -> Result<Option<u64>, PathError> {
    let entries = match std::fs::read_dir(session_dir) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    let mut latest = None;
    for entry in entries {
        if let Some(seq) = entry?
            .file_name()
            .to_str()
            .and_then(|name| name.parse::<u64>().ok())
        {
            latest = latest.max(Some(seq));
        }
    }
    Ok(latest)
}

/// A file's hex sha256, or `None` when it does not exist.
fn file_sha256(path: &Path) -> std::io::Result<Option<String>> {
    match std::fs::read(path) {
        Ok(bytes) => Ok(Some(Sha256::digest(bytes).iter().fold(
            String::new(),
            |mut hex, byte| {
                let _ = write!(hex, "{byte:02x}");
                hex
            },
        ))),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error),
    }
}

/// Atomically replace `path` with a copy of `pre_image`, keeping its permissions.
fn restore(pre_image: &Path, path: &Path) -> Result<(), PathError> {
    let parent = path.parent().expect("workspace file parent");
    std::fs::create_dir_all(parent)?;
    let temp = parent.join(format!(".nexus-{}.tmp", uuid::Uuid::new_v4()));
    let result = std::fs::copy(pre_image, &temp).and_then(|_| std::fs::rename(&temp, path));
    if result.is_err() {
        let _ = std::fs::remove_file(&temp);
    }
    result.map_err(PathError::from)
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Dirs {
        base: PathBuf,
        root: PathBuf,
    }

    impl Dirs {
        fn new() -> Self {
            let base =
                std::env::temp_dir().join(format!("nexus-checkpoint-{}", uuid::Uuid::new_v4()));
            let root = base.join("workspace");
            std::fs::create_dir_all(&root).unwrap();
            let root = root.canonicalize().unwrap();
            Self { base, root }
        }

        fn open(&self) -> Workspace {
            Workspace::open(&self.root, Vec::new())
                .unwrap()
                .with_checkpoints(self.base.join("checkpoints"))
        }

        fn path(&self, rel: &str) -> PathBuf {
            self.root.join(rel)
        }
    }

    impl Drop for Dirs {
        fn drop(&mut self) {
            std::fs::remove_dir_all(&self.base).unwrap();
        }
    }

    fn change(workspace: &Workspace, path: &Path, content: Option<&str>) {
        workspace.before_change(path).unwrap();
        match content {
            Some(content) => std::fs::write(path, content).unwrap(),
            None => std::fs::remove_file(path).unwrap(),
        }
        workspace.after_change(path);
    }

    #[test]
    fn undo_restores_edits_deletes_creations_and_moves() {
        let dirs = Dirs::new();
        let workspace = dirs.open();
        std::fs::write(dirs.path("edited"), "original").unwrap();
        std::fs::write(dirs.path("deleted"), "keep me").unwrap();
        std::fs::write(dirs.path("source"), "moved").unwrap();
        workspace.begin_turn("s1");
        change(&workspace, &dirs.path("edited"), Some("changed"));
        change(&workspace, &dirs.path("created"), Some("new"));
        change(&workspace, &dirs.path("deleted"), None);
        workspace.before_change(&dirs.path("source")).unwrap();
        workspace.before_change(&dirs.path("target")).unwrap();
        std::fs::rename(dirs.path("source"), dirs.path("target")).unwrap();
        workspace.after_change(&dirs.path("source"));
        workspace.after_change(&dirs.path("target"));
        assert!(workspace.turn_changed_files("s1"));
        assert!(!workspace.turn_changed_files("s2"));

        let report = workspace.undo_last("s1").unwrap().unwrap();
        assert_eq!(report.restored, ["edited", "deleted", "source"]);
        assert_eq!(report.removed_created, ["created", "target"]);
        assert!(report.skipped_changed_by_user.is_empty());
        assert_eq!(std::fs::read(dirs.path("edited")).unwrap(), b"original");
        assert_eq!(std::fs::read(dirs.path("deleted")).unwrap(), b"keep me");
        assert_eq!(std::fs::read(dirs.path("source")).unwrap(), b"moved");
        assert!(!dirs.path("created").exists());
        assert!(!dirs.path("target").exists());
        assert_eq!(
            report.summary(),
            "undid 5 file changes from the last agent turn · 0 skipped (changed since) · shell commands are not undone"
        );
        assert!(workspace.undo_last("s1").unwrap().is_none());
    }

    #[test]
    fn several_changes_to_one_file_keep_the_original_pre_image() {
        let dirs = Dirs::new();
        let workspace = dirs.open();
        std::fs::write(dirs.path("file"), "one").unwrap();
        workspace.begin_turn("s1");
        change(&workspace, &dirs.path("file"), Some("two"));
        change(&workspace, &dirs.path("file"), Some("three"));
        let turn_dir = dirs.base.join("checkpoints/s1/1");
        let stored: Vec<_> = std::fs::read_dir(&turn_dir)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().into_string().unwrap())
            .filter(|name| name != "manifest.json")
            .collect();
        assert_eq!(stored, ["0.pre"]);
        assert_eq!(std::fs::read(turn_dir.join("0.pre")).unwrap(), b"one");
        let report = workspace.undo_last("s1").unwrap().unwrap();
        assert_eq!(report.restored, ["file"]);
        assert_eq!(std::fs::read(dirs.path("file")).unwrap(), b"one");
        assert!(!turn_dir.exists());
    }

    #[test]
    fn files_changed_by_the_user_afterwards_are_skipped_and_listed() {
        let dirs = Dirs::new();
        let workspace = dirs.open();
        std::fs::write(dirs.path("mine"), "original").unwrap();
        std::fs::write(dirs.path("agent"), "original").unwrap();
        workspace.begin_turn("s1");
        change(&workspace, &dirs.path("mine"), Some("agent edit"));
        change(&workspace, &dirs.path("agent"), Some("agent edit"));
        change(&workspace, &dirs.path("gone"), Some("created"));
        std::fs::write(dirs.path("mine"), "user edit").unwrap();
        std::fs::write(dirs.path("gone"), "user kept it").unwrap();
        let report = workspace.undo_last("s1").unwrap().unwrap();
        assert_eq!(report.restored, ["agent"]);
        assert_eq!(report.skipped_changed_by_user, ["mine", "gone"]);
        assert_eq!(std::fs::read(dirs.path("mine")).unwrap(), b"user edit");
        assert_eq!(std::fs::read(dirs.path("gone")).unwrap(), b"user kept it");
        assert_eq!(std::fs::read(dirs.path("agent")).unwrap(), b"original");
        assert_eq!(
            report.summary(),
            "undid 1 file changes from the last agent turn · 2 skipped (changed since: mine, gone) · shell commands are not undone"
        );
    }

    #[test]
    fn undo_walks_back_one_turn_at_a_time_and_skips_empty_turns() {
        let dirs = Dirs::new();
        let workspace = dirs.open();
        std::fs::write(dirs.path("file"), "v0").unwrap();
        workspace.begin_turn("s1");
        change(&workspace, &dirs.path("file"), Some("v1"));
        workspace.begin_turn("s1");
        change(&workspace, &dirs.path("file"), Some("v2"));
        workspace.begin_turn("s1");
        assert!(!workspace.turn_changed_files("s1"));
        assert!(!dirs.base.join("checkpoints/s1/3").exists());
        workspace.undo_last("s1").unwrap().unwrap();
        assert_eq!(std::fs::read(dirs.path("file")).unwrap(), b"v1");
        workspace.undo_last("s1").unwrap().unwrap();
        assert_eq!(std::fs::read(dirs.path("file")).unwrap(), b"v0");
        assert!(workspace.undo_last("s1").unwrap().is_none());
    }

    #[test]
    fn changes_outside_a_turn_are_not_recorded() {
        let dirs = Dirs::new();
        let workspace = dirs.open();
        change(&workspace, &dirs.path("file"), Some("new"));
        assert!(workspace.undo_last("s1").unwrap().is_none());
        assert!(!dirs.base.join("checkpoints").exists());
        let plain = Workspace::open(&dirs.root, Vec::new()).unwrap();
        plain.begin_turn("s1");
        change(&plain, &dirs.path("file"), Some("newer"));
        assert!(plain.undo_last("s1").unwrap().is_none());
    }

    #[test]
    fn checkpoints_from_another_root_are_reported_and_kept() {
        let dirs = Dirs::new();
        let workspace = dirs.open();
        std::fs::write(dirs.path("file"), "original").unwrap();
        workspace.begin_turn("s1");
        change(&workspace, &dirs.path("file"), Some("changed"));
        let other_root = dirs.base.join("other");
        std::fs::create_dir_all(&other_root).unwrap();
        let other = Workspace::open(&other_root, Vec::new())
            .unwrap()
            .with_checkpoints(dirs.base.join("checkpoints"));
        let report = other.undo_last("s1").unwrap().unwrap();
        assert_eq!(report.wrong_root.as_deref(), Some(dirs.root.as_path()));
        assert_eq!(
            report.summary(),
            format!(
                "the last agent turn changed files in {root} · switch back with /agent {root} to undo it",
                root = dirs.root.display()
            )
        );
        assert!(dirs.base.join("checkpoints/s1/1/manifest.json").exists());
        assert_eq!(std::fs::read(dirs.path("file")).unwrap(), b"changed");
        let report = workspace.undo_last("s1").unwrap().unwrap();
        assert_eq!(report.restored, ["file"]);
        assert_eq!(std::fs::read(dirs.path("file")).unwrap(), b"original");
    }

    #[cfg(unix)]
    #[test]
    fn a_failed_restore_does_not_stop_the_others_and_is_reported() {
        use std::os::unix::fs::PermissionsExt as _;

        let dirs = Dirs::new();
        let workspace = dirs.open();
        std::fs::create_dir_all(dirs.path("locked")).unwrap();
        for rel in ["a", "locked/b", "c"] {
            std::fs::write(dirs.path(rel), "original").unwrap();
        }
        workspace.begin_turn("s1");
        for rel in ["a", "locked/b", "c"] {
            change(&workspace, &dirs.path(rel), Some("changed"));
        }
        let locked = dirs.path("locked");
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o555)).unwrap();
        let report = workspace.undo_last("s1").unwrap().unwrap();
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert_eq!(report.restored, ["a", "c"]);
        assert_eq!(report.failed.len(), 1);
        assert_eq!(report.failed[0].0, "locked/b");
        assert_eq!(std::fs::read(dirs.path("a")).unwrap(), b"original");
        assert_eq!(std::fs::read(dirs.path("c")).unwrap(), b"original");
        assert_eq!(std::fs::read(dirs.path("locked/b")).unwrap(), b"changed");
        assert!(!dirs.base.join("checkpoints/s1/1").exists());
        assert!(report.summary().contains(&format!(
            " · 1 failed (locked/b: {}) · shell commands are not undone",
            report.failed[0].1
        )));
    }

    #[test]
    fn oversized_pre_images_are_not_stored_and_undo_skips_them() {
        let dirs = Dirs::new();
        let workspace = dirs.open();
        let big = vec![b'x'; usize::try_from(MAX_PRE_IMAGE).unwrap() + 1];
        std::fs::write(dirs.path("big"), &big).unwrap();
        workspace.begin_turn("s1");
        change(&workspace, &dirs.path("big"), Some("small"));
        assert!(!dirs.base.join("checkpoints/s1/1/0.pre").exists());
        let report = workspace.undo_last("s1").unwrap().unwrap();
        assert_eq!(report.skipped_too_large, ["big"]);
        assert!(report.restored.is_empty());
        assert_eq!(std::fs::read(dirs.path("big")).unwrap(), b"small");
        assert!(
            report
                .summary()
                .contains("1 not restorable (over 10 MiB: big)")
        );
    }
}
