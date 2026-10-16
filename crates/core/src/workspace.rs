//! Confined paths and read stamps for a user-selected agent workspace.

use std::collections::HashMap;
use std::fmt;
use std::io::Read as _;
use std::path::{Component, Path, PathBuf};
use std::sync::Mutex;
use std::time::SystemTime;

use sha2::{Digest, Sha256};

/// A canonical workspace root, its hard exclusions, and the files last read.
pub struct Workspace {
    root: PathBuf,
    deny: Vec<PathBuf>,
    stamps: Mutex<HashMap<PathBuf, FileStamp>>,
}

/// The identity of a file's contents at read time.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FileStamp {
    pub len: u64,
    pub mtime: SystemTime,
    pub sha256: [u8; 32],
}

/// Whether a resolved path will be read or changed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Access {
    Read,
    Write,
}

/// A path rejected by the workspace, or a filesystem failure.
#[derive(Debug)]
pub enum PathError {
    Invalid(String),
    Outside(PathBuf),
    Denied(PathBuf, &'static str),
    Io(String),
}

impl fmt::Display for PathError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Invalid(reason) => write!(
                f,
                "invalid path: {reason} · use a non-empty workspace path without NUL"
            ),
            Self::Outside(path) => write!(
                f,
                "path escapes the workspace: \"{}\" · use a path inside the workspace",
                path.display()
            ),
            Self::Denied(path, reason) => write!(
                f,
                "path denied: {} ({reason}) · choose another workspace path",
                path.display()
            ),
            Self::Io(reason) => write!(
                f,
                "workspace I/O failed: {reason} · check the path and its permissions"
            ),
        }
    }
}

impl std::error::Error for PathError {}

impl From<std::io::Error> for PathError {
    fn from(error: std::io::Error) -> Self {
        Self::Io(error.to_string())
    }
}

impl Workspace {
    /// Open an existing directory and canonicalize its hard-denied directories.
    pub fn open(root: &Path, deny: Vec<PathBuf>) -> Result<Self, PathError> {
        let root = root.canonicalize()?;
        if !root.is_dir() {
            return Err(PathError::Invalid(format!(
                "{} is not a directory",
                root.display()
            )));
        }
        let deny = deny
            .into_iter()
            .map(|path| path.canonicalize().map_err(PathError::from))
            .collect::<Result<_, _>>()?;
        Ok(Self {
            root,
            deny,
            stamps: Mutex::new(HashMap::new()),
        })
    }

    /// The canonical directory all workspace paths are confined to.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Resolve a relative path, or an absolute path inside this workspace.
    /// Missing targets are resolved through their nearest existing ancestor.
    pub fn resolve(&self, rel: &str, access: Access) -> Result<PathBuf, PathError> {
        if rel.is_empty() || rel.contains('\0') {
            return Err(PathError::Invalid(format!("{rel:?}")));
        }
        let input = Path::new(rel);
        let relative = if input.is_absolute() {
            input
                .strip_prefix(&self.root)
                .map_err(|_| PathError::Outside(input.to_path_buf()))?
        } else {
            input
        };
        let mut path = self.root.clone();
        for component in relative.components() {
            match component {
                Component::Normal(name) => path.push(name),
                Component::CurDir => {}
                Component::ParentDir => {
                    if path == self.root {
                        return Err(PathError::Outside(input.to_path_buf()));
                    }
                    path.pop();
                }
                Component::RootDir | Component::Prefix(_) => {
                    return Err(PathError::Outside(input.to_path_buf()));
                }
            }
        }
        self.check_denied(&path, access)?;
        let mut ancestor = path.as_path();
        let metadata = loop {
            match std::fs::symlink_metadata(ancestor) {
                Ok(metadata) => break metadata,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                    ancestor = ancestor.parent().expect("workspace root exists");
                }
                Err(error) => return Err(error.into()),
            }
        };
        // symlink_metadata deliberately sees dangling links: canonicalizing one
        // must fail, rather than falling back to an apparently safe parent.
        let canonical = ancestor.canonicalize().map_err(|error| {
            if metadata.file_type().is_symlink() {
                PathError::Outside(path.clone())
            } else {
                error.into()
            }
        })?;
        if !canonical.starts_with(&self.root) {
            return Err(PathError::Outside(path));
        }
        let suffix = path.strip_prefix(ancestor).expect("path ancestor");
        let resolved = if suffix.as_os_str().is_empty() {
            canonical
        } else {
            canonical.join(suffix)
        };
        self.check_denied(&resolved, access)?;
        Ok(resolved)
    }

    fn check_denied(&self, path: &Path, access: Access) -> Result<(), PathError> {
        let reason = if path
            .strip_prefix(&self.root)
            .expect("confined workspace path")
            .components()
            .any(|component| component.as_os_str() == ".git")
        {
            Some("git internals — use shell git commands")
        } else if self.deny.iter().any(|deny| path.starts_with(deny)) {
            Some("nexus data or config directory")
        } else if access == Access::Write
            && path
                .file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| {
                    name == ".env"
                        || name.starts_with(".env.")
                        || [".pem", ".key", "id_rsa", "id_ed25519"]
                            .iter()
                            .any(|suffix| name.ends_with(suffix))
                })
        {
            Some("secrets file")
        } else {
            None
        };
        match reason {
            Some(reason) => Err(PathError::Denied(path.to_path_buf(), reason)),
            None => Ok(()),
        }
    }

    /// Read a confined file and compute its length, modification time and hash.
    pub fn stamp(&self, path: &Path) -> Result<FileStamp, PathError> {
        let path = self.resolve(&path.to_string_lossy(), Access::Read)?;
        let mut file = std::fs::File::open(path)?;
        let metadata = file.metadata()?;
        let mut bytes = Vec::new();
        file.read_to_end(&mut bytes)?;
        Self::stamp_bytes(&bytes, &metadata)
    }

    /// Stamp bytes already read using metadata from the same open file handle.
    pub fn stamp_bytes(bytes: &[u8], metadata: &std::fs::Metadata) -> Result<FileStamp, PathError> {
        Ok(FileStamp {
            len: metadata.len(),
            mtime: metadata.modified()?,
            sha256: Sha256::digest(bytes).into(),
        })
    }

    /// Remember the stamp returned by a successful read.
    pub fn record_read(&self, path: &Path, stamp: FileStamp) {
        self.stamps
            .lock()
            .expect("workspace stamps lock")
            .insert(path.to_path_buf(), stamp);
    }

    /// The stamp of the last successful read, if any.
    pub fn last_read(&self, path: &Path) -> Option<FileStamp> {
        self.stamps
            .lock()
            .expect("workspace stamps lock")
            .get(path)
            .cloned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct TempWorkspace(PathBuf);

    impl TempWorkspace {
        fn new() -> Self {
            let path =
                std::env::temp_dir().join(format!("nexus-workspace-{}", uuid::Uuid::new_v4()));
            std::fs::create_dir_all(&path).unwrap();
            Self(path.canonicalize().unwrap())
        }

        fn open(&self) -> Workspace {
            Workspace::open(&self.0, Vec::new()).unwrap()
        }
    }

    impl Drop for TempWorkspace {
        fn drop(&mut self) {
            std::fs::remove_dir_all(&self.0).unwrap();
        }
    }

    #[test]
    fn workspace_is_send_and_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<Workspace>();
    }

    #[test]
    fn resolves_inside_and_new_nested_paths() {
        let dir = TempWorkspace::new();
        let workspace = dir.open();
        std::fs::write(dir.0.join("file"), "hello").unwrap();
        for path in ["file", "./file", "nested/../file"] {
            assert_eq!(
                workspace.resolve(path, Access::Read).unwrap(),
                dir.0.join("file")
            );
        }
        assert_eq!(workspace.root(), dir.0);
        assert_eq!(
            workspace
                .resolve(dir.0.join("file").to_str().unwrap(), Access::Read)
                .unwrap(),
            dir.0.join("file")
        );
        assert_eq!(workspace.resolve(".", Access::Read).unwrap(), dir.0);
        assert_eq!(
            workspace.resolve("new/nested/file", Access::Write).unwrap(),
            dir.0.join("new/nested/file")
        );
    }

    #[test]
    fn rejects_invalid_outside_and_non_directory_roots() {
        let dir = TempWorkspace::new();
        let outside = TempWorkspace::new();
        let workspace = dir.open();
        for path in ["../x", "a/../../x", outside.0.to_str().unwrap()] {
            let error = workspace.resolve(path, Access::Read).unwrap_err();
            assert!(matches!(error, PathError::Outside(_)));
            assert!(error.to_string().contains("· use a path inside"));
        }
        for path in ["", "a\0b"] {
            assert!(matches!(
                workspace.resolve(path, Access::Read),
                Err(PathError::Invalid(_))
            ));
        }
        std::fs::write(dir.0.join("file"), "x").unwrap();
        assert!(Workspace::open(&dir.0.join("file"), Vec::new()).is_err());
    }

    #[test]
    fn hard_denies_and_write_only_secret_denies() {
        let dir = TempWorkspace::new();
        let denied = dir.0.join("nexus");
        std::fs::create_dir_all(&denied).unwrap();
        let workspace = Workspace::open(&dir.0, vec![denied]).unwrap();
        for access in [Access::Read, Access::Write] {
            for path in [".git/config", "nexus/new/file"] {
                assert!(matches!(
                    workspace.resolve(path, access),
                    Err(PathError::Denied(_, _))
                ));
            }
        }
        for path in [
            ".env",
            ".env.local",
            "server.pem",
            "private.key",
            "id_rsa",
            "id_ed25519",
        ] {
            assert!(workspace.resolve(path, Access::Read).is_ok());
            assert!(matches!(
                workspace.resolve(path, Access::Write),
                Err(PathError::Denied(_, "secrets file"))
            ));
        }
    }

    #[test]
    fn nested_git_internals_are_denied_for_reads_and_writes() {
        let dir = TempWorkspace::new();
        std::fs::create_dir_all(dir.0.join("sub/.git")).unwrap();
        std::fs::write(dir.0.join("sub/.git/config"), "config").unwrap();
        let workspace = dir.open();
        for access in [Access::Read, Access::Write] {
            for path in ["sub/.git/config", "sub/.git/hooks/pre-commit"] {
                assert!(matches!(
                    workspace.resolve(path, access),
                    Err(PathError::Denied(
                        _,
                        "git internals — use shell git commands"
                    ))
                ));
            }
            assert!(workspace.resolve("sub/.gitignore", access).is_ok());
        }
    }

    #[cfg(unix)]
    #[test]
    fn symlink_escapes_and_denied_aliases_are_rejected() {
        use std::os::unix::fs::symlink;

        let dir = TempWorkspace::new();
        let outside = TempWorkspace::new();
        symlink(&outside.0, dir.0.join("out")).unwrap();
        symlink(outside.0.join("missing"), dir.0.join("dangling")).unwrap();
        std::fs::create_dir_all(dir.0.join(".git")).unwrap();
        symlink(dir.0.join(".git"), dir.0.join("git-alias")).unwrap();
        std::fs::write(dir.0.join(".env"), "secret").unwrap();
        symlink(dir.0.join(".env"), dir.0.join("env-alias")).unwrap();
        let workspace = dir.open();
        for path in ["out/file", "dangling", "dangling/new/file"] {
            assert!(matches!(
                workspace.resolve(path, Access::Read),
                Err(PathError::Outside(_))
            ));
        }
        assert!(matches!(
            workspace.resolve("git-alias/config", Access::Read),
            Err(PathError::Denied(_, _))
        ));
        assert!(workspace.resolve("env-alias", Access::Read).is_ok());
        assert!(matches!(
            workspace.resolve("env-alias", Access::Write),
            Err(PathError::Denied(_, "secrets file"))
        ));
    }

    #[test]
    fn stamp_bytes_keeps_the_read_contents_after_the_file_changes() {
        let dir = TempWorkspace::new();
        let workspace = dir.open();
        let path = dir.0.join("file");
        std::fs::write(&path, "shown").unwrap();
        let mut file = std::fs::File::open(&path).unwrap();
        let metadata = file.metadata().unwrap();
        let mut bytes = Vec::new();
        file.read_to_end(&mut bytes).unwrap();
        std::fs::write(&path, "changed contents").unwrap();
        let stamp = Workspace::stamp_bytes(&bytes, &metadata).unwrap();
        assert_eq!(stamp.len, 5);
        assert_eq!(stamp.mtime, metadata.modified().unwrap());
        assert_eq!(stamp.sha256.as_slice(), Sha256::digest(b"shown").as_slice());
        assert_ne!(stamp, workspace.stamp(&path).unwrap());
    }

    #[test]
    fn stamps_round_trip_and_change_with_contents() {
        let dir = TempWorkspace::new();
        let workspace = dir.open();
        let path = dir.0.join("file");
        std::fs::write(&path, "one").unwrap();
        assert!(workspace.last_read(&path).is_none());
        let stamp = workspace.stamp(&path).unwrap();
        assert_eq!(stamp.len, 3);
        assert_eq!(stamp.sha256.as_slice(), Sha256::digest(b"one").as_slice());
        workspace.record_read(&path, stamp.clone());
        assert_eq!(workspace.last_read(&path), Some(stamp.clone()));
        std::fs::write(&path, "two").unwrap();
        let changed = workspace.stamp(&path).unwrap();
        assert_ne!(changed.sha256, stamp.sha256);
        assert_ne!(changed, stamp);
        assert_eq!(workspace.last_read(&path), Some(stamp));
    }
}
