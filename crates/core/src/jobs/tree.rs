use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use anyhow::{Context, Result, ensure};
use sha2::{Digest as _, Sha256};

const COPY_LIMIT: u64 = 200 * 1024 * 1024;

pub(super) struct Tree {
    pub path: PathBuf,
    pub branch: Option<String>,
    source: PathBuf,
    before: BTreeMap<PathBuf, Vec<u8>>,
}

fn git(root: &Path, args: &[&str]) -> Result<Output> {
    let output = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(args)
        .env("GIT_TERMINAL_PROMPT", "0")
        .output()?;
    ensure!(
        output.status.success(),
        "git {}: {}",
        args.join(" "),
        String::from_utf8_lossy(&output.stderr)
    );
    Ok(output)
}

fn snapshot(root: &Path) -> Result<BTreeMap<PathBuf, Vec<u8>>> {
    fn visit(
        root: &Path,
        dir: &Path,
        total: &mut u64,
        files: &mut BTreeMap<PathBuf, Vec<u8>>,
    ) -> Result<()> {
        for entry in fs::read_dir(dir)? {
            let entry = entry?;
            let kind = entry.file_type()?;
            let path = entry.path();
            // A copied symlink could point back into the user's tree.
            ensure!(
                !kind.is_symlink(),
                "non-git copy refuses symlinks: {}",
                path.display()
            );
            if kind.is_dir() {
                visit(root, &path, total, files)?;
            } else if kind.is_file() {
                *total = total.saturating_add(entry.metadata()?.len());
                ensure!(
                    *total <= COPY_LIMIT,
                    "non-git workspace exceeds 200 MiB; use a git repository"
                );
                files.insert(path.strip_prefix(root)?.to_path_buf(), fs::read(path)?);
            }
        }
        Ok(())
    }
    let mut files = BTreeMap::new();
    visit(root, root, &mut 0, &mut files)?;
    Ok(files)
}

fn copy_directories(source: &Path, target: &Path) -> Result<()> {
    fs::create_dir(target)?;
    for entry in fs::read_dir(source)? {
        let entry = entry?;
        if entry.file_type()?.is_dir() {
            copy_directories(&entry.path(), &target.join(entry.file_name()))?;
        }
    }
    Ok(())
}

impl Tree {
    pub fn prepare(root: &Path, data: &Path, name: &str, run_id: &str) -> Result<Self> {
        let path = data.join("job-worktrees").join(run_id);
        fs::create_dir_all(path.parent().context("worktree parent")?)?;
        let is_git = Command::new("git")
            .arg("-C")
            .arg(root)
            .args(["rev-parse", "--is-inside-work-tree"])
            .output()
            .is_ok_and(|output| output.status.success() && output.stdout.starts_with(b"true"));
        let mut tree = Self {
            path,
            branch: None,
            source: root.to_path_buf(),
            before: BTreeMap::new(),
        };
        if is_git {
            let slug: String = name
                .chars()
                .map(|ch| if ch.is_ascii_alphanumeric() { ch } else { '-' })
                .take(60)
                .collect();
            let branch = format!("nexus/job/{slug}/{run_id}");
            git(
                root,
                &[
                    "worktree",
                    "add",
                    tree.path.to_str().context("non-UTF-8 worktree path")?,
                    "-b",
                    &branch,
                    "HEAD",
                ],
            )?;
            tree.branch = Some(branch);
        } else {
            tree.before = snapshot(root)?;
            copy_directories(root, &tree.path)?;
            for (relative, bytes) in &tree.before {
                let target = tree.path.join(relative);
                fs::write(&target, bytes)?;
                fs::set_permissions(&target, fs::metadata(root.join(relative))?.permissions())?;
            }
        }
        Ok(tree)
    }

    pub fn changes(&self) -> Result<String> {
        if self.branch.is_none() {
            return Ok(self
                .copy_diff()?
                .lines()
                .skip(1)
                .collect::<Vec<_>>()
                .join("\n"));
        }
        let tracked = git(&self.path, &["diff", "HEAD", "--stat", "--patch"])?;
        let untracked = git(&self.path, &["ls-files", "--others", "--exclude-standard"])?;
        let mut changes = String::from_utf8_lossy(&tracked.stdout).into_owned();
        for path in String::from_utf8_lossy(&untracked.stdout).lines() {
            let file = self.path.join(path);
            if file.symlink_metadata()?.is_file() {
                write!(changes, "new file {path}: ")?;
                for byte in Sha256::digest(fs::read(file)?) {
                    write!(changes, "{byte:02x}")?;
                }
                changes.push('\n');
            }
        }
        Ok(changes)
    }

    pub fn finish(&self, name: &str, run_id: &str) -> Result<String> {
        if self.branch.is_none() {
            return self.copy_diff();
        }
        git(&self.path, &["add", "-A"])?;
        let changed = Command::new("git")
            .arg("-C")
            .arg(&self.path)
            .args(["diff", "--cached", "--quiet"])
            .status()?;
        ensure!(matches!(changed.code(), Some(0 | 1)), "git diff failed");
        if !changed.success() {
            // Unattended: the repo's hooks (e.g. a slow pre-commit gate) and
            // commit signing (a passphrase prompt) must not block the job.
            // The branch is reviewed before it is merged anyway.
            git(
                &self.path,
                &[
                    "-c",
                    "commit.gpgsign=false",
                    "commit",
                    "--no-verify",
                    "-m",
                    &format!("nexus job {name}: {run_id}"),
                ],
            )?;
        }
        git(
            &self.source,
            &[
                "worktree",
                "remove",
                self.path.to_str().context("non-UTF-8 worktree path")?,
            ],
        )?;
        Ok(format!(
            "branch: {}",
            self.branch.as_deref().unwrap_or_default()
        ))
    }

    fn copy_diff(&self) -> Result<String> {
        let after = snapshot(&self.path)?;
        let paths: BTreeSet<_> = self.before.keys().chain(after.keys()).collect();
        let mut changes = Vec::new();
        for path in paths {
            let before = self.before.get(path);
            let next = after.get(path);
            if before == next {
                continue;
            }
            let lines = |bytes: Option<&Vec<u8>>| {
                bytes.map_or(0, |bytes| String::from_utf8_lossy(bytes).lines().count())
            };
            let mut diff = format!(
                "--- a/{}\n+++ b/{}\n@@ -1,{} +1,{} @@\n",
                path.display(),
                path.display(),
                lines(before),
                lines(next)
            );
            for (prefix, bytes) in [('-', before), ('+', next)] {
                if let Some(bytes) = bytes {
                    for line in String::from_utf8_lossy(bytes).lines() {
                        writeln!(diff, "{prefix}{line}")?;
                    }
                }
            }
            changes.push(diff);
        }
        Ok(format!(
            "copy: {}\n{} files changed\n{}",
            self.path.display(),
            changes.len(),
            changes.join("\n")
        ))
    }
}
