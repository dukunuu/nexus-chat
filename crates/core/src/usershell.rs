//! The user's shell setup and bounded, off-thread composer completion.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::{Duration, Instant};

/// Cached once per app, including failures (which are reported once).
#[derive(Debug)]
pub struct UserShell {
    /// Validated shell executable.
    pub shell: PathBuf,
    /// Only definitions and PATH, never startup chatter.
    pub snapshot: Option<PathBuf>,
    /// PATH captured after interactive startup.
    pub path: String,
    /// Alias and function names available for completion.
    pub names: Vec<String>,
    /// Why setup could not be captured.
    pub warning: Option<String>,
}

/// Select a supported executable, otherwise the POSIX fallback.
#[must_use]
pub fn select_shell(value: Option<&std::ffi::OsStr>) -> PathBuf {
    value
        .map(PathBuf::from)
        .filter(|path| {
            path.is_absolute()
                && matches!(
                    path.file_name().and_then(|s| s.to_str()),
                    Some("bash" | "zsh" | "sh")
                )
                && executable(path)
        })
        .unwrap_or_else(|| PathBuf::from("/bin/sh"))
}

fn executable(path: &Path) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        path.metadata()
            .is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
    }
    #[cfg(not(unix))]
    path.is_file()
}

/// Shell single-quoting, also used for paths containing spaces or apostrophes.
#[must_use]
pub fn quote(text: &str) -> String {
    format!("'{}'", text.replace('\'', "'\\''"))
}

/// Display a directory without exposing the user's home prefix repeatedly.
#[must_use]
pub fn display_dir(path: &Path) -> String {
    display_with_home(path, std::env::var_os("HOME").as_deref().map(Path::new))
}

fn display_with_home(path: &Path, home: Option<&Path>) -> String {
    let relative = home.and_then(|home| {
        path.strip_prefix(home)
            .ok()
            .map(Path::to_path_buf)
            .or_else(|| {
                path.strip_prefix(home.canonicalize().ok()?)
                    .ok()
                    .map(Path::to_path_buf)
            })
    });
    if let Some(rest) = relative {
        return if rest.as_os_str().is_empty() {
            "~".into()
        } else {
            format!("~/{}", rest.display())
        };
    }
    path.display().to_string()
}

impl UserShell {
    /// Capture the supported portions of interactive startup without a terminal.
    pub async fn load(data: &Path) -> Self {
        Self::build(
            select_shell(std::env::var_os("SHELL").as_deref()),
            data,
            None,
            Duration::from_secs(5),
        )
        .await
    }

    async fn build(shell: PathBuf, data: &Path, home: Option<&Path>, timeout: Duration) -> Self {
        let mut setup = Self {
            shell,
            snapshot: None,
            path: std::env::var("PATH").unwrap_or_default(),
            names: Vec::new(),
            warning: None,
        };
        match setup.capture(data, home, timeout).await {
            Ok(()) => {}
            Err(reason) => setup.warning = Some(reason),
        }
        setup
    }

    async fn capture(
        &mut self,
        data: &Path,
        home: Option<&Path>,
        timeout: Duration,
    ) -> Result<(), String> {
        std::fs::create_dir_all(data).map_err(|e| e.to_string())?;
        let stem = data.join(format!("shell-{}", uuid::Uuid::new_v4()));
        let definitions = stem.with_extension("sh");
        let path_file = stem.with_extension("path");
        let names_file = stem.with_extension("names");
        let result = self
            .dump(&definitions, &path_file, &names_file, home, timeout)
            .await;
        if result.is_ok() {
            let snapshot = data.join("shell-snapshot.sh");
            let path = std::fs::read_to_string(&path_file).map_err(|e| e.to_string());
            if let Ok(path) = &path {
                let definitions_text =
                    std::fs::read_to_string(&definitions).map_err(|e| e.to_string())?;
                private_write(
                    &definitions,
                    format!("{definitions_text}\nexport PATH={}\n", quote(path)).as_bytes(),
                )
                .map_err(|e| e.to_string())?;
                std::fs::rename(&definitions, &snapshot).map_err(|e| e.to_string())?;
                self.names = std::fs::read_to_string(&names_file)
                    .unwrap_or_default()
                    .lines()
                    .filter(|s| !s.starts_with('_') && !s.is_empty())
                    .map(str::to_string)
                    .collect();
                self.path.clone_from(path);
                self.snapshot = Some(snapshot);
            }
            path?;
        }
        for file in [&definitions, &path_file, &names_file] {
            let _ = std::fs::remove_file(file);
        }
        result
    }

    async fn dump(
        &self,
        definitions: &Path,
        path: &Path,
        names: &Path,
        home: Option<&Path>,
        timeout: Duration,
    ) -> Result<(), String> {
        private_write(definitions, b"").map_err(|e| e.to_string())?;
        private_write(path, b"").map_err(|e| e.to_string())?;
        private_write(names, b"").map_err(|e| e.to_string())?;
        let dump = match self.shell.file_name().and_then(|s| s.to_str()) {
            Some("bash") => {
                r#"{ declare -f; alias -p; } >| "$NEXUS_DEFS"; { compgen -A function; compgen -A alias; } >| "$NEXUS_NAMES"; printf '%s' "$PATH" >| "$NEXUS_PATH""#
            }
            Some("zsh") => {
                r#"{ typeset -f; alias -L; typeset -p chpwd_functions 2>/dev/null; [[ -o extendedglob ]] && print 'setopt extendedglob'; [[ -o nullglob ]] && print 'setopt nullglob'; } >| "$NEXUS_DEFS"; printf '%s\n' ${(k)functions} ${(k)aliases} >| "$NEXUS_NAMES"; printf '%s' "$PATH" >| "$NEXUS_PATH""#
            }
            _ => r#"printf '%s' "$PATH" > "$NEXUS_PATH""#,
        };
        let mut command = tokio::process::Command::new(&self.shell);
        command
            .args(["-i", "-c", dump])
            .env("NEXUS_DEFS", definitions)
            .env("NEXUS_PATH", path)
            .env("NEXUS_NAMES", names)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .kill_on_drop(true);
        if let Some(home) = home {
            command.env("HOME", home).env("ZDOTDIR", home);
        }
        #[cfg(unix)]
        command.process_group(0);
        let mut child = command.spawn().map_err(|e| e.to_string())?;
        let group = child.id();
        let result = tokio::time::timeout(timeout, child.wait()).await;
        kill_group(group);
        match result {
            Ok(Ok(status)) if status.success() => Ok(()),
            Ok(Ok(status)) => Err(format!("shell setup exited {status}")),
            Ok(Err(error)) => Err(error.to_string()),
            Err(_) => {
                let _ = child.wait().await;
                Err("shell setup timed out".into())
            }
        }
    }

    /// Noninteractive wrapper: aliases expand in bash, and EXIT also records
    /// cwd for commands containing an explicit `exit`.
    #[must_use]
    pub fn wrapper(&self, cmd: &str, cwd: &Path) -> String {
        let aliases = if self.shell.file_name().is_some_and(|name| name == "bash") {
            "shopt -s expand_aliases\n"
        } else {
            ""
        };
        let source = self.snapshot.as_ref().map_or_else(String::new, |p| {
            format!(". {} 2>/dev/null || :\n", quote(&p.to_string_lossy()))
        });
        // The user's `cd` may be a function (zoxide `--cmd cd`); entering the
        // tracked directory must not run it or bump its ranking.
        let cd = if self
            .shell
            .file_name()
            .is_some_and(|name| name == "bash" || name == "zsh")
        {
            "builtin cd"
        } else {
            "cd"
        };
        format!(
            "{aliases}{source}{cd} -- {} || exit\ntrap 'pwd -P > \"$NEXUS_CWD_FILE\"' EXIT\neval {}\nexit $?",
            quote(&cwd.to_string_lossy()),
            quote(cmd)
        )
    }
}

fn private_write(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options.open(path)?.write_all(bytes)
}

fn kill_group(group: Option<u32>) {
    #[cfg(unix)]
    if let Some(pid) = group.and_then(|pid| libc::pid_t::try_from(pid).ok()) {
        // SAFETY: only the process group created for this child is signalled.
        unsafe { libc::kill(-pid, libc::SIGKILL) };
    }
    #[cfg(not(unix))]
    let _ = group;
}

/// A shell word and its byte range in the composer. Quotes are decoded, not evaluated.
#[derive(Debug, PartialEq, Eq)]
pub struct Word {
    /// Decoded prefix up to the cursor.
    pub text: String,
    /// Replacement starts here.
    pub start: usize,
    /// Replacement ends here (including any closing quote).
    pub end: usize,
}

/// Split shell words, respecting quoted and backslash-escaped spaces.
#[must_use]
pub fn words(text: &str) -> Vec<Word> {
    let mut out = Vec::new();
    let mut word = String::new();
    let mut start = None;
    let mut quote = None;
    let mut escaped = false;
    for (i, ch) in text.char_indices() {
        if escaped {
            word.push(ch);
            escaped = false;
            continue;
        }
        if ch == '\\' && quote != Some('\'') {
            start.get_or_insert(i);
            escaped = true;
            continue;
        }
        if quote == Some(ch) {
            quote = None;
            continue;
        }
        if quote.is_none() && matches!(ch, '\'' | '"') {
            start.get_or_insert(i);
            quote = Some(ch);
            continue;
        }
        if quote.is_none() && ch.is_whitespace() {
            if let Some(start) = start.take() {
                out.push(Word {
                    text: std::mem::take(&mut word),
                    start,
                    end: i,
                });
            }
        } else {
            start.get_or_insert(i);
            word.push(ch);
        }
    }
    if let Some(start) = start {
        out.push(Word {
            text: word,
            start,
            end: text.len(),
        });
    }
    out
}

/// Programs that need the real terminal (a password prompt or a full-screen
/// UI): the TUI hands them the terminal instead of capturing their output.
#[must_use]
pub fn terminal_program(cmd: &str) -> Option<String> {
    words(cmd)
        .first()
        .filter(|word| {
            matches!(
                word.text.as_str(),
                "sudo"
                    | "doas"
                    | "su"
                    | "pkexec"
                    | "ssh"
                    | "vim"
                    | "nvim"
                    | "vi"
                    | "nano"
                    | "less"
                    | "more"
                    | "top"
                    | "htop"
                    | "btop"
                    | "man"
            )
        })
        .map(|word| word.text.clone())
}

/// A completion request's replacements, already escaped for shell insertion.
#[derive(Debug, Default)]
pub struct Completion {
    /// Byte range to replace.
    pub range: std::ops::Range<usize>,
    /// Sorted, deduplicated alternatives.
    pub candidates: Vec<String>,
}

/// Complete off the UI thread; even a slow filesystem or zoxide cannot stall keys.
pub async fn complete(setup: &UserShell, text: &str, cursor: usize, cwd: &Path) -> Completion {
    let home = std::env::var_os("HOME").map(PathBuf::from);
    let setup_path = setup.path.clone();
    let names = setup.names.clone();
    let text = text.to_string();
    let cwd = cwd.to_path_buf();
    let local_cwd = cwd.clone();
    let (mut completion, query) = tokio::task::spawn_blocking(move || {
        complete_local(
            &setup_path,
            &names,
            &text,
            cursor,
            &local_cwd,
            home.as_deref(),
        )
    })
    .await
    .unwrap_or_default();
    if let Some(query) = query {
        let zoxide = std::env::split_paths(&setup.path)
            .map(|dir| dir.join("zoxide"))
            .find(|p| executable(p));
        if let Some(zoxide) = zoxide {
            let mut command = tokio::process::Command::new(zoxide);
            command
                .args(["query", "--list", "--"])
                .args(query)
                .env("PATH", &setup.path)
                .current_dir(&cwd)
                .stdin(Stdio::null())
                .stderr(Stdio::null())
                .kill_on_drop(true);
            if let Ok(Ok(output)) =
                tokio::time::timeout(Duration::from_millis(300), command.output()).await
                && output.status.success()
            {
                completion.candidates.extend(
                    String::from_utf8_lossy(&output.stdout)
                        .lines()
                        .take(10)
                        .filter(|s| Path::new(s).is_dir())
                        .map(|s| escape(&format!("{}/", s.trim_end_matches('/')))),
                );
                completion.candidates.sort();
                completion.candidates.dedup();
            }
        }
    }
    completion
}

fn complete_local(
    path: &str,
    names: &[String],
    text: &str,
    cursor: usize,
    cwd: &Path,
    home: Option<&Path>,
) -> (Completion, Option<Vec<String>>) {
    let Some(before) = text.get(1..cursor) else {
        return (Completion::default(), None);
    };
    let mut parsed = words(before);
    let at_boundary = before.chars().last().is_none_or(char::is_whitespace)
        && parsed.last().is_none_or(|word| word.end < before.len());
    if at_boundary {
        parsed.push(Word {
            text: String::new(),
            start: before.len(),
            end: before.len(),
        });
    }
    let Some(word) = parsed.last() else {
        return (Completion::default(), None);
    };
    let first = parsed.first().map_or("", |word| word.text.as_str());
    let dirs = parsed.len() > 1 && matches!(first, "cd" | "z" | "zi");
    let end = words(&text[1..])
        .into_iter()
        .find(|w| w.start == word.start)
        .map_or(cursor, |w| w.end + 1);
    let mut completion = Completion {
        range: word.start + 1..end,
        candidates: Vec::new(),
    };
    let deadline = Instant::now() + Duration::from_millis(35);
    let mut budget = 2000;
    let mut candidates = BTreeSet::new();
    if parsed.len() == 1 && !word.text.contains('/') && !word.text.starts_with('~') {
        for name in names
            .iter()
            .filter(|name| !name.starts_with('_') && name.starts_with(&word.text))
        {
            candidates.insert(escape(name));
        }
        for dir in std::env::split_paths(path) {
            scan(&dir, &mut budget, deadline, |entry| {
                let name = entry.file_name().to_string_lossy().into_owned();
                if name.starts_with(&word.text) && executable(&entry.path()) {
                    candidates.insert(escape(&name));
                }
            });
            if budget == 0 || Instant::now() >= deadline {
                break;
            }
        }
    } else {
        let (parent, prefix) = word
            .text
            .rsplit_once('/')
            .map_or(("", word.text.as_str()), |(dir, prefix)| {
                (&word.text[..=dir.len()], prefix)
            });
        let expanded = if parent == "~/" {
            home.unwrap_or(cwd).to_path_buf()
        } else if let Some(rest) = parent.strip_prefix("~/") {
            home.unwrap_or(cwd).join(rest)
        } else {
            cwd.join(parent)
        };
        if word.text == "~" {
            candidates.insert("~/".to_string());
        } else {
            scan(&expanded, &mut budget, deadline, |entry| {
                let name = entry.file_name().to_string_lossy().into_owned();
                let is_dir = entry.path().is_dir();
                if name.starts_with(prefix)
                    && (!name.starts_with('.') || prefix.starts_with('.'))
                    && (!dirs || is_dir)
                {
                    candidates.insert(escape(&format!(
                        "{parent}{name}{}",
                        if is_dir { "/" } else { "" }
                    )));
                }
            });
        }
    }
    completion.candidates = candidates.into_iter().collect();
    let query = (dirs && matches!(first, "z" | "zi")).then(|| {
        parsed
            .iter()
            .skip(1)
            .filter(|w| !w.text.is_empty())
            .map(|w| w.text.clone())
            .collect()
    });
    if query.is_some() {
        completion.range = parsed[1].start + 1..end;
    }
    (completion, query)
}

fn scan(
    dir: &Path,
    budget: &mut usize,
    deadline: Instant,
    mut visit: impl FnMut(std::fs::DirEntry),
) {
    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries {
            if *budget == 0 || Instant::now() >= deadline {
                break;
            }
            *budget -= 1;
            if let Ok(entry) = entry {
                visit(entry);
            }
        }
    }
}

fn escape(text: &str) -> String {
    text.chars().fold(String::new(), |mut out, ch| {
        if !ch.is_alphanumeric() && !matches!(ch, '/' | '.' | '_' | '-' | '~') {
            out.push('\\');
        }
        out.push(ch);
        out
    })
}

/// Longest shared prefix on Unicode character boundaries.
#[must_use]
pub fn common_prefix(candidates: &[String]) -> String {
    let Some(first) = candidates.first() else {
        return String::new();
    };
    let mut end = first.len();
    for candidate in &candidates[1..] {
        end = first
            .char_indices()
            .take_while(|(i, ch)| {
                *i < end && candidate.get(*i..).is_some_and(|s| s.starts_with(*ch))
            })
            .last()
            .map_or(0, |(i, ch)| i + ch.len_utf8());
    }
    let prefix = &first[..end];
    let trailing_slashes = prefix.chars().rev().take_while(|ch| *ch == '\\').count();
    if trailing_slashes.is_multiple_of(2) {
        prefix.to_string()
    } else {
        prefix.strip_suffix('\\').unwrap_or(prefix).to_string()
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    fn temp() -> PathBuf {
        let root = std::env::temp_dir().join(format!("nexus-shell-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(root.join("home/bin")).unwrap();
        root
    }

    fn script(path: &Path, text: &str) {
        std::fs::write(path, text).unwrap();
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700)).unwrap();
    }

    #[tokio::test]
    async fn bash_snapshot_restores_definitions_path_and_exit_status() {
        let root = temp();
        let home = root.join("home");
        std::fs::write(home.join(".bashrc"), "echo startup-noise\nalias greet='printf alias-ok'\nnexus_fn() { printf function-ok; }\nexport PATH=\"$HOME/bin:$PATH\"\n").unwrap();
        script(&home.join("bin/addon"), "#!/bin/sh\nprintf path-ok\n");
        let setup = UserShell::build(
            "/bin/bash".into(),
            &root.join("data"),
            Some(&home),
            Duration::from_secs(5),
        )
        .await;
        assert_eq!(setup.warning, None);
        assert!(setup.names.contains(&"greet".into()));
        assert!(setup.names.contains(&"nexus_fn".into()));
        assert!(
            setup
                .path
                .starts_with(&home.join("bin").to_string_lossy().into_owned())
        );
        let snapshot = setup.snapshot.as_ref().unwrap();
        assert_eq!(
            snapshot.metadata().unwrap().permissions().mode() & 0o777,
            0o600
        );
        let definitions = std::fs::read_to_string(snapshot).unwrap();
        assert!(!definitions.contains("startup-noise"));
        let cwd_file = root.join("cwd");
        let output = tokio::process::Command::new(&setup.shell)
            .args([
                "-c",
                &setup.wrapper("greet; nexus_fn; addon; exit 23", &home),
            ])
            .env("HOME", &home)
            .env("NEXUS_CWD_FILE", &cwd_file)
            .output()
            .await
            .unwrap();
        assert_eq!(output.status.code(), Some(23));
        assert_eq!(
            String::from_utf8(output.stdout).unwrap(),
            "alias-okfunction-okpath-ok"
        );
        assert_eq!(
            std::fs::read_to_string(cwd_file).unwrap(),
            format!("{}\n", home.canonicalize().unwrap().display())
        );
        let mut app = crate::app::App::new(
            crate::db::Db::open_in_memory().unwrap(),
            Some("test"),
            crate::space::Space {
                root: root.join("data"),
            },
        );
        app.interactive_ui = true;
        app.user_shell.set(setup).unwrap();
        app.run_user_command("greet; nexus_fn; addon".into())
            .unwrap();
        loop {
            if matches!(
                app.next_event().await,
                crate::app::AppEvent::HistoryInvalidated
            ) {
                break;
            }
        }
        let row: serde_json::Value =
            serde_json::from_str(&app.messages.last().unwrap().content).unwrap();
        assert_eq!(row["output"], "alias-okfunction-okpath-ok");
        assert_eq!(row["exit"], 0);
        drop(app);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn failed_and_timed_out_startup_leave_no_snapshot() {
        let root = temp();
        let home = root.join("home");
        // Only the hanging case gets the short timeout: under a loaded
        // parallel test run bash alone can take longer than 150 ms to start,
        // which turned "exited 9" into a spurious "timed out".
        for (rc, expected, timeout) in [
            ("exit 9", "shell setup exited", Duration::from_secs(10)),
            (
                "/bin/sleep 30",
                "shell setup timed out",
                Duration::from_millis(150),
            ),
        ] {
            std::fs::write(home.join(".bashrc"), rc).unwrap();
            let setup =
                UserShell::build("/bin/bash".into(), &root.join("data"), Some(&home), timeout)
                    .await;
            assert!(setup.snapshot.is_none());
            assert!(setup.warning.unwrap().starts_with(expected));
            assert_eq!(std::fs::read_dir(root.join("data")).unwrap().count(), 0);
        }
        std::fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn zsh_snapshot_preserves_only_safe_options_and_directory_hooks() {
        let shell = PathBuf::from("/bin/zsh");
        if !executable(&shell) {
            return;
        }
        let root = temp();
        let home = root.join("home");
        std::fs::write(home.join(".zshrc"), "alias greet='printf alias-ok'\nlearn_dir() { printf learned > learned; }\nchpwd_functions=(learn_dir)\nsetopt extendedglob nullglob noclobber\n").unwrap();
        let setup = UserShell::build(
            shell,
            &root.join("data"),
            Some(&home),
            Duration::from_secs(5),
        )
        .await;
        assert_eq!(setup.warning, None);
        let definitions = std::fs::read_to_string(setup.snapshot.as_ref().unwrap()).unwrap();
        assert!(definitions.contains("setopt extendedglob"));
        assert!(definitions.contains("setopt nullglob"));
        assert!(!definitions.contains("setopt noclobber"));
        let output = tokio::process::Command::new(&setup.shell)
            .args(["-c", &setup.wrapper("greet; cd bin", &home)])
            .env("HOME", &home)
            .env("ZDOTDIR", &home)
            .env("NEXUS_CWD_FILE", root.join("cwd"))
            .output()
            .await
            .unwrap();
        assert!(output.status.success());
        assert_eq!(output.stdout, b"alias-ok");
        assert!(home.join("bin/learned").exists());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn shell_selection_rejects_relative_unsupported_and_non_executable_paths() {
        assert_eq!(
            select_shell(Some(std::ffi::OsStr::new("bash"))),
            PathBuf::from("/bin/sh")
        );
        assert_eq!(
            select_shell(Some(std::ffi::OsStr::new("/bin/false"))),
            PathBuf::from("/bin/sh")
        );
        assert_eq!(
            select_shell(Some(std::ffi::OsStr::new("/bin/bash"))),
            PathBuf::from("/bin/bash")
        );
        let root = temp();
        let bash = root.join("bash");
        std::fs::write(&bash, "not executable").unwrap();
        assert_eq!(
            select_shell(Some(bash.as_os_str())),
            PathBuf::from("/bin/sh")
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn completion_handles_paths_quotes_dotfiles_home_and_directory_only_commands() {
        let root = temp();
        for name in ["folder", "foo bar", ".hidden"] {
            std::fs::create_dir(root.join(name)).unwrap();
        }
        std::fs::write(root.join("file"), "").unwrap();
        script(&root.join("home/bin/nexus-exec"), "#!/bin/sh\nexit 0\n");
        let path = root.join("home/bin").to_string_lossy().into_owned();
        let local = |text: &str| {
            complete_local(
                &path,
                &["nexus-alias".into(), "_nexus-private".into()],
                text,
                text.len(),
                &root,
                Some(&root.join("home")),
            )
            .0
        };
        assert_eq!(local("!nexus-").candidates, ["nexus-alias", "nexus-exec"]);
        assert_eq!(local("!echo fo").candidates, ["folder/", "foo\\ bar/"]);
        assert_eq!(local("!echo 'foo b").candidates, ["foo\\ bar/"]);
        assert_eq!(local("!echo foo\\ b").candidates, ["foo\\ bar/"]);
        assert!(
            !local("!echo ")
                .candidates
                .iter()
                .any(|s| s.starts_with('.'))
        );
        assert_eq!(local("!echo .h").candidates, [".hidden/"]);
        assert_eq!(local("!cd fi").candidates, Vec::<String>::new());
        assert_eq!(local("!cd fo").candidates, ["folder/", "foo\\ bar/"]);
        assert_eq!(local("!echo ~").candidates, ["~/"]);
        assert_eq!(local("!echo ~/b").candidates, ["~/bin/"]);
        assert_eq!(local("!echo home/../fol").candidates, ["home/../folder/"]);
        assert_eq!(
            display_with_home(&root.join("home/bin"), Some(&root.join("home"))),
            "~/bin"
        );
        assert_eq!(
            common_prefix(&["foo\\ bar/".into(), "foo\\ baz/".into()]),
            "foo\\ ba"
        );
        assert_eq!(common_prefix(&["écho".into(), "éclair".into()]), "éc");
        let linked_home = root.join("linked-home");
        std::os::unix::fs::symlink(root.join("home"), &linked_home).unwrap();
        assert_eq!(
            display_with_home(
                &root.join("home/bin").canonicalize().unwrap(),
                Some(&linked_home)
            ),
            "~/bin"
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn zoxide_queries_words_and_limits_results_and_wait_time() {
        let root = temp();
        let binary = root.join("home/bin/zoxide");
        let target = root.join("foo bar");
        std::fs::create_dir(&target).unwrap();
        script(
            &binary,
            &format!(
                "#!/bin/sh\nprintf '%s\\n' \"$@\" > {}\nprintf '%s\\n' {}\n",
                quote(&root.join("args").to_string_lossy()),
                quote(&target.to_string_lossy())
            ),
        );
        let setup = UserShell {
            shell: "/bin/sh".into(),
            snapshot: None,
            path: root.join("home/bin").to_string_lossy().into_owned(),
            names: Vec::new(),
            warning: None,
        };
        let fixture = tokio::process::Command::new(&binary)
            .args(["query", "--list", "--", "foo", "bar"])
            .env("PATH", &setup.path)
            .current_dir(&root)
            .output()
            .await
            .unwrap();
        assert!(fixture.status.success(), "{fixture:?}");
        assert!(executable(&binary));
        std::fs::remove_file(root.join("args")).unwrap();
        assert_eq!(
            complete_local(&setup.path, &[], "!z foo bar", 10, &root, None).1,
            Some(vec!["foo".into(), "bar".into()])
        );
        let result = complete(&setup, "!z foo bar", 10, &root).await;
        assert!(!result.candidates.is_empty(), "{result:?}");
        assert_eq!(
            std::fs::read_to_string(root.join("args")).unwrap(),
            "query\n--list\n--\nfoo\nbar\n"
        );
        assert_eq!(
            result.candidates,
            [escape(&format!("{}/", target.display()))]
        );
        assert_eq!(result.range, 3..10);
        let mut many = String::from("#!/bin/sh\n");
        for i in 0..12 {
            let dir = root.join(format!("target {i}"));
            std::fs::create_dir(&dir).unwrap();
            many.push_str(&format!(
                "printf '%s\\n' {}\n",
                quote(&dir.to_string_lossy())
            ));
        }
        script(&binary, &many);
        let many = complete(&setup, "!z targets", 10, &root).await;
        assert_eq!(many.candidates.len(), 10);
        assert!(
            !many
                .candidates
                .contains(&escape(&format!("{}/", root.join("target 11").display())))
        );
        script(&binary, "#!/bin/sh\n/bin/sleep 2\n");
        let start = Instant::now();
        assert!(
            complete(&setup, "!zi nowhere", 11, &root)
                .await
                .candidates
                .is_empty()
        );
        assert!(start.elapsed() < Duration::from_secs(1));
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn directory_scan_has_a_hard_entry_budget() {
        let root = temp();
        for i in 0..2100 {
            std::fs::write(root.join(format!("file{i}")), "").unwrap();
        }
        let (completion, _) = complete_local("", &[], "!echo file", 10, &root, None);
        assert!(completion.candidates.len() <= 2000);
        std::fs::remove_dir_all(root).unwrap();
    }
}
