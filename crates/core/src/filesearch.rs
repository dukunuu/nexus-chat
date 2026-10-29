//! Finding files on this machine to import into a space: the newest files in
//! the usual drop folders, Spotlight (`mdfind`) on macOS with a bounded walk
//! of home as the fallback, and completion for typed `/…` and `~/…` paths.

use std::io::{BufRead as _, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant, SystemTime};

/// One candidate: a file to import, or a directory to complete into.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Found {
    pub path: PathBuf,
    pub is_dir: bool,
}

/// Folders `recent` lists, relative to home.
const DROP_FOLDERS: &[&str] = &["Downloads", "Desktop", "Documents"];
/// How long the fallback walk looks before returning what it has.
const WALK_BUDGET: Duration = Duration::from_millis(1500);
/// Candidates read from Spotlight or the walk before ranking.
const CANDIDATES_MAX: usize = 2000;
/// Entries one completed directory may contribute.
const COMPLETIONS_MAX: usize = 500;

/// The newest `max` files directly inside home's Downloads, Desktop and
/// Documents, newest first. Hidden files are skipped.
#[must_use]
pub fn recent(home: &Path, max: usize) -> Vec<Found> {
    let mut files: Vec<(SystemTime, PathBuf)> = DROP_FOLDERS
        .iter()
        .filter_map(|dir| std::fs::read_dir(home.join(dir)).ok())
        .flatten()
        .flatten()
        .filter(|entry| !hidden(&entry.file_name().to_string_lossy()))
        .filter_map(|entry| {
            let meta = entry.metadata().ok()?;
            let modified = meta.modified().unwrap_or(SystemTime::UNIX_EPOCH);
            meta.is_file().then(|| (modified, entry.path()))
        })
        .collect();
    files.sort_by_key(|(modified, _)| std::cmp::Reverse(*modified));
    files
        .into_iter()
        .take(max)
        .map(|(_, path)| Found {
            path,
            is_dir: false,
        })
        .collect()
}

/// Up to `max` files under `home` whose names contain every word of `query`
/// (case-insensitive), best first: names starting with the first word, then
/// shallower paths. Spotlight answers when `spotlight` is set and it finds
/// anything; otherwise a gitignore-aware walk looks for a moment. Hidden
/// paths and `~/Library` are left out either way.
#[must_use]
pub fn search(home: &Path, query: &str, max: usize, spotlight: bool) -> Vec<Found> {
    let words: Vec<String> = query.split_whitespace().map(str::to_lowercase).collect();
    if words.is_empty() {
        return Vec::new();
    }
    let candidates = spotlight
        .then(|| spotlight_search(home, &words))
        .flatten()
        .filter(|paths| !paths.is_empty())
        .unwrap_or_else(|| walk(home, &words));
    let mut ranked: Vec<(bool, usize, PathBuf)> = candidates
        .into_iter()
        .filter(|path| visible(path, home) && name_matches(path, &words) && path.is_file())
        .map(|path| {
            let starts = file_name(&path).to_lowercase().starts_with(&words[0]);
            (!starts, path.components().count(), path)
        })
        .collect();
    ranked.sort();
    ranked.dedup_by(|a, b| a.2 == b.2);
    ranked
        .into_iter()
        .take(max)
        .map(|(_, _, path)| Found {
            path,
            is_dir: false,
        })
        .collect()
}

/// Entries completing a typed path (`~/Doc`, `/tmp/a`): the directory part
/// is listed and the entries whose names start with the rest are kept
/// (case-insensitive), directories first. Hidden entries show only once the
/// rest starts with a dot.
#[must_use]
pub fn complete(input: &str, home: &Path) -> Vec<Found> {
    let expanded = expand(input, home);
    let Some(slash) = expanded.rfind('/') else {
        return Vec::new();
    };
    let (dir, prefix) = (&expanded[..=slash], &expanded[slash + 1..]);
    let prefix_lower = prefix.to_lowercase();
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut found: Vec<Found> = entries
        .flatten()
        .filter_map(|entry| {
            let name = entry.file_name().to_string_lossy().into_owned();
            if hidden(&name) && !prefix.starts_with('.') {
                return None;
            }
            if !name.to_lowercase().starts_with(&prefix_lower) {
                return None;
            }
            let path = entry.path();
            Some(Found {
                is_dir: path.is_dir(),
                path,
            })
        })
        .take(COMPLETIONS_MAX)
        .collect();
    found.sort_by(|a, b| {
        b.is_dir.cmp(&a.is_dir).then_with(|| {
            file_name(&a.path)
                .to_lowercase()
                .cmp(&file_name(&b.path).to_lowercase())
        })
    });
    found
}

/// `~` and `~/…` expanded against `home`; any other input unchanged.
#[must_use]
pub fn expand(input: &str, home: &Path) -> String {
    match input.strip_prefix('~') {
        Some("") => format!("{}/", home.display()),
        Some(rest) if rest.starts_with('/') => format!("{}{rest}", home.display()),
        _ => input.to_string(),
    }
}

/// `path` for display, with `home` shown as `~`.
#[must_use]
pub fn tilde(path: &Path, home: &Path) -> String {
    match path.strip_prefix(home) {
        Ok(rest) if rest.as_os_str().is_empty() => "~".to_string(),
        Ok(rest) => format!("~/{}", rest.display()),
        Err(_) => path.display().to_string(),
    }
}

fn hidden(name: &str) -> bool {
    name.starts_with('.')
}

fn file_name(path: &Path) -> String {
    path.file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default()
}

fn name_matches(path: &Path, words: &[String]) -> bool {
    let name = file_name(path).to_lowercase();
    words.iter().all(|word| name.contains(word.as_str()))
}

/// Inside `home`, not under a hidden directory or `~/Library`.
fn visible(path: &Path, home: &Path) -> bool {
    let Ok(rest) = path.strip_prefix(home) else {
        return false;
    };
    !rest.starts_with("Library")
        && rest
            .components()
            .all(|part| !hidden(&part.as_os_str().to_string_lossy()))
}

/// Spotlight's file-name matches for the longest word (the rest are checked
/// while ranking); `None` when `mdfind` can't run.
fn spotlight_search(home: &Path, words: &[String]) -> Option<Vec<PathBuf>> {
    let longest = words.iter().max_by_key(|word| word.len())?;
    let mut child = Command::new("mdfind")
        .arg("-onlyin")
        .arg(home)
        .arg("-name")
        .arg(longest)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let stdout = child.stdout.take()?;
    let paths = BufReader::new(stdout)
        .lines()
        .map_while(Result::ok)
        .take(CANDIDATES_MAX)
        .map(PathBuf::from)
        .collect();
    let _ = child.kill();
    let _ = child.wait();
    Some(paths)
}

/// Files under `home` whose names hold every word, from a walk that skips
/// hidden and gitignored paths and `~/Library`, and stops after a moment.
fn walk(home: &Path, words: &[String]) -> Vec<PathBuf> {
    let started = Instant::now();
    let library = home.join("Library");
    let mut found = Vec::new();
    let entries = ignore::WalkBuilder::new(home)
        .hidden(true)
        .follow_links(false)
        .max_depth(Some(8))
        .filter_entry(move |entry| entry.path() != library)
        .build()
        .flatten();
    for entry in entries {
        if started.elapsed() > WALK_BUDGET || found.len() >= CANDIDATES_MAX {
            break;
        }
        if entry.file_type().is_some_and(|kind| kind.is_file()) && name_matches(entry.path(), words)
        {
            found.push(entry.into_path());
        }
    }
    found
}

#[cfg(test)]
mod tests {
    use super::*;

    struct TempHome(PathBuf);

    impl TempHome {
        fn new(files: &[&str]) -> Self {
            let root =
                std::env::temp_dir().join(format!("nexus-filesearch-{}", uuid::Uuid::new_v4()));
            std::fs::create_dir_all(&root).unwrap();
            let root = root.canonicalize().unwrap();
            for file in files {
                let path = root.join(file);
                std::fs::create_dir_all(path.parent().unwrap()).unwrap();
                std::fs::write(path, "x").unwrap();
            }
            Self(root)
        }

        fn names(&self, found: &[Found]) -> Vec<String> {
            found.iter().map(|f| tilde(&f.path, &self.0)).collect()
        }
    }

    impl Drop for TempHome {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn touch(path: &Path, secs_ago: u64) {
        let file = std::fs::File::options().write(true).open(path).unwrap();
        file.set_modified(SystemTime::now() - Duration::from_secs(secs_ago))
            .unwrap();
    }

    #[test]
    fn recent_lists_drop_folders_newest_first_without_hidden_files() {
        let home = TempHome::new(&[
            "Downloads/old.pdf",
            "Desktop/new.png",
            "Documents/mid.docx",
            "Downloads/.DS_Store",
            "Music/song.mp3",
        ]);
        touch(&home.0.join("Downloads/old.pdf"), 300);
        touch(&home.0.join("Documents/mid.docx"), 200);
        touch(&home.0.join("Desktop/new.png"), 100);
        assert_eq!(
            home.names(&recent(&home.0, 10)),
            [
                "~/Desktop/new.png",
                "~/Documents/mid.docx",
                "~/Downloads/old.pdf"
            ]
        );
        assert_eq!(recent(&home.0, 1).len(), 1);
    }

    #[test]
    fn search_walks_home_for_every_word_and_ranks_name_starts_first() {
        let home = TempHome::new(&[
            "work/q3/Quarterly Report.pdf",
            "Report draft quarterly.txt",
            "notes/report.md",
            ".secret/quarterly report.pdf",
            "Library/Caches/quarterly report.pdf",
        ]);
        assert_eq!(
            home.names(&search(&home.0, "quarterly REPORT", 10, false)),
            [
                "~/work/q3/Quarterly Report.pdf",
                "~/Report draft quarterly.txt"
            ]
        );
        assert!(search(&home.0, "  ", 10, false).is_empty());
    }

    #[test]
    fn complete_lists_matching_entries_dirs_first_and_hidden_on_a_dot() {
        let home = TempHome::new(&[
            "Documents/a.txt",
            "Downloads/b.txt",
            "docs.md",
            ".dotfile",
            "Music/c.mp3",
        ]);
        assert_eq!(
            home.names(&complete("~/do", &home.0)),
            ["~/Documents", "~/Downloads", "~/docs.md"]
        );
        assert!(complete("~/Documents/", &home.0)[0].path.ends_with("a.txt"));
        assert_eq!(home.names(&complete("~/.", &home.0)), ["~/.dotfile"]);
        assert_eq!(complete("~", &home.0).len(), 4);
        let absolute = format!("{}/Mu", home.0.display());
        assert_eq!(home.names(&complete(&absolute, &home.0)), ["~/Music"]);
        assert!(complete("~/missing/", &home.0).is_empty());
    }

    #[test]
    fn expand_and_tilde_round_trip_home() {
        let home = Path::new("/Users/me");
        assert_eq!(expand("~", home), "/Users/me/");
        assert_eq!(expand("~/Doc", home), "/Users/me/Doc");
        assert_eq!(expand("~other", home), "~other");
        assert_eq!(tilde(Path::new("/Users/me/Doc/a"), home), "~/Doc/a");
        assert_eq!(tilde(Path::new("/tmp/a"), home), "/tmp/a");
    }
}
