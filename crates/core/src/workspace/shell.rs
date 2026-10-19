//! One-shot workspace commands with layered escalation protection and saved logs.

use std::path::Path;

use super::Workspace;

pub(crate) async fn action(
    workspace: &Workspace,
    db_path: Option<&Path>,
    name: &str,
    args: &str,
    allow_unparseable: bool,
) -> String {
    #[cfg(unix)]
    {
        let args = match serde_json::from_str(args) {
            Ok(args) => args,
            Err(error) => return format!("invalid tool arguments: {error}"),
        };
        unix::action(workspace, db_path, name, &args, allow_unparseable)
            .await
            .unwrap_or_else(|error| format!("shell failed: {error}"))
    }
    #[cfg(not(unix))]
    {
        let _ = (workspace, db_path, name, args, allow_unparseable);
        "shell is only available on Unix".to_string()
    }
}

#[cfg(unix)]
mod unix {
    use std::fmt::Write as _;
    use std::fs::{self, File};
    use std::io::{BufRead as _, BufReader, Read as _, Seek as _, SeekFrom};
    use std::os::unix::fs::PermissionsExt as _;
    use std::os::unix::process::CommandExt as _;
    use std::path::{Path, PathBuf};
    use std::process::Stdio;
    use std::sync::Once;
    use std::time::Duration;

    use serde_json::Value;
    use tokio::process::Command;

    use super::Workspace;
    use crate::workspace::Access;
    use crate::workspace::escalation::{Escalation, check_command};

    #[cfg(not(test))]
    const MAX_LOG_BYTES: u64 = 64 * 1024 * 1024;
    #[cfg(test)]
    const MAX_LOG_BYTES: u64 = 64 * 1024;
    const OUTPUT_WINDOW: u64 = 1024 * 1024;
    const OUTPUT_EXCEEDED: &str =
        "output exceeded 64 MiB · killed; narrow the command (grep, head) or redirect to a file";
    static PRUNE_LOGS: Once = Once::new();

    const ENV_ALLOWLIST: &[&str] = &[
        "PATH",
        "HOME",
        "LANG",
        "LC_ALL",
        "LC_CTYPE",
        "TERM",
        "USER",
        "LOGNAME",
        "SHELL",
        "TMPDIR",
        "CARGO_HOME",
        "RUSTUP_HOME",
        "NVM_DIR",
        "VIRTUAL_ENV",
        "GOPATH",
        "GOROOT",
        "JAVA_HOME",
        "PYENV_ROOT",
        "SSL_CERT_FILE",
    ];
    const SHIM: &str = "#!/bin/sh\necho \"privilege escalation is blocked — ask the user to run it with !\" >&2\nexit 1\n";
    #[cfg(target_os = "macos")]
    const PROFILE: &str = "(version 1)(allow default)(deny process-exec (literal \"/usr/bin/sudo\") (literal \"/usr/bin/su\") (literal \"/usr/bin/login\") (literal \"/usr/sbin/sudo\"))";

    pub(super) async fn action(
        workspace: &Workspace,
        db_path: Option<&Path>,
        name: &str,
        args: &Value,
        allow_unparseable: bool,
    ) -> anyhow::Result<String> {
        let runs = runs_dir(db_path);
        PRUNE_LOGS.call_once(|| {
            let _ = prune_logs(&runs);
        });
        if name == "shell_log" {
            return log(&runs, args);
        }
        let cmd = args["cmd"].as_str().unwrap_or_default();
        if cmd.trim().is_empty() {
            return Ok("missing required field: cmd".to_string());
        }
        match check_command(cmd) {
            Err(Escalation::Blocked(word)) => {
                return Ok(format!(
                    "privilege escalation is blocked: {word} · ask the user to run it themselves (propose the command; they can run it with !)"
                ));
            }
            Err(Escalation::Unparseable(_)) if !allow_unparseable => {
                return Ok(
                    "could not parse this command safely · simplify it (no unbalanced quotes)"
                        .to_string(),
                );
            }
            Ok(()) | Err(Escalation::Unparseable(_)) => {}
        }
        let cwd = match workspace.resolve(args["cwd"].as_str().unwrap_or("."), Access::Read) {
            Ok(cwd) => cwd,
            Err(error) => return Ok(error.to_string()),
        };
        if !cwd.is_dir() {
            return Ok("invalid path: shell cwd must be a directory".to_string());
        }
        let shims = shims(&runs)?;
        let id = uuid::Uuid::new_v4().simple().to_string()[..12].to_string();
        let path = runs.join(format!("{id}.log"));
        let output = File::create_new(&path)?;
        let (mut command, sandbox_note) = command(cmd);
        environment(&mut command, &shims)?;
        // Both descriptors share one open-file offset: stdout and stderr are
        // recorded in kernel write order, without competing reader tasks.
        command
            .current_dir(cwd)
            .stdin(Stdio::null())
            .stdout(output.try_clone()?)
            .stderr(output)
            .kill_on_drop(true);
        session(&mut command);
        let mut child = command.spawn()?;
        let group = ProcessGroup(
            libc::pid_t::try_from(child.id().expect("spawned child pid")).expect("pid fits pid_t"),
        );
        let seconds = args["timeout_s"].as_u64().unwrap_or(120).min(600);
        let header = wait(&mut child, &group, &path, seconds).await?;
        // One-shot commands must not leave background descendants behind even
        // when the shell itself exits before them.
        drop(group);
        Ok(shape(
            &format!("{header} · shell action=log id={id}{sandbox_note}"),
            &path,
            &id,
        )?)
    }

    fn runs_dir(db_path: Option<&Path>) -> PathBuf {
        db_path.map_or_else(
            || std::env::temp_dir().join("nexus-runs"),
            |path| path.parent().unwrap_or(Path::new(".")).join("runs"),
        )
    }

    fn prune_logs(runs: &Path) -> std::io::Result<()> {
        for entry in fs::read_dir(runs)? {
            let entry = entry?;
            let path = entry.path();
            if path.extension().is_some_and(|extension| extension == "log")
                && entry.file_type()?.is_file()
                && entry
                    .metadata()?
                    .modified()?
                    .elapsed()
                    .is_ok_and(|age| age > Duration::from_hours(7 * 24))
            {
                fs::remove_file(path)?;
            }
        }
        Ok(())
    }

    async fn wait(
        child: &mut tokio::process::Child,
        group: &ProcessGroup,
        path: &Path,
        seconds: u64,
    ) -> std::io::Result<String> {
        let deadline = tokio::time::sleep(Duration::from_secs(seconds));
        tokio::pin!(deadline);
        let mut poll = tokio::time::interval(Duration::from_millis(250));
        let reason = loop {
            tokio::select! {
                status = child.wait() => {
                    let status = status?;
                    // Fast commands can exceed the cap between polling ticks.
                    if fs::metadata(path)?.len() > MAX_LOG_BYTES {
                        break OUTPUT_EXCEEDED.to_string();
                    }
                    return Ok(match status.code() {
                        Some(code) => format!("exit {code}"),
                        None => "killed by signal".to_string(),
                    });
                }
                () = &mut deadline => {
                    break format!("timed out after {seconds} s · use a shorter command or run it yourself with !");
                }
                _ = poll.tick() => {
                    if fs::metadata(path)?.len() > MAX_LOG_BYTES {
                        break OUTPUT_EXCEEDED.to_string();
                    }
                }
            }
        };
        group.kill();
        child.wait().await?;
        Ok(reason)
    }

    fn shims(runs: &Path) -> std::io::Result<PathBuf> {
        let dir = runs.join("shims");
        fs::create_dir_all(&dir)?;
        for name in ["sudo", "doas", "su", "pkexec", "run0"] {
            let path = dir.join(name);
            if !path.exists() {
                fs::write(&path, SHIM)?;
                fs::set_permissions(path, fs::Permissions::from_mode(0o755))?;
            }
        }
        dir.canonicalize()
    }

    fn environment(command: &mut Command, shims: &Path) -> anyhow::Result<()> {
        command.env_clear();
        for key in ENV_ALLOWLIST {
            if let Some(value) = std::env::var_os(key) {
                command.env(key, value);
            }
        }
        command.envs([
            ("CI", "1"),
            ("GIT_TERMINAL_PROMPT", "0"),
            ("PAGER", "cat"),
            ("GIT_PAGER", "cat"),
            ("NO_COLOR", "1"),
        ]);
        let path = std::env::var_os("PATH").unwrap_or_default();
        command.env(
            "PATH",
            std::env::join_paths(
                std::iter::once(shims.to_path_buf()).chain(std::env::split_paths(&path)),
            )?,
        );
        crate::config::scrub_secret_env(command.as_std_mut());
        Ok(())
    }

    fn command(cmd: &str) -> (Command, &'static str) {
        #[cfg(target_os = "macos")]
        let (mut command, note) = if Path::new("/usr/bin/sandbox-exec").exists() {
            let mut command = Command::new("/usr/bin/sandbox-exec");
            command.args(["-p", PROFILE, "sh"]);
            (command, "")
        } else {
            (
                Command::new("sh"),
                " · escalation sandbox unavailable on this machine",
            )
        };
        #[cfg(not(target_os = "macos"))]
        let (mut command, note) = (Command::new("sh"), "");
        command.args(["-c", cmd]);
        (command, note)
    }

    fn session(command: &mut Command) {
        // SAFETY: after fork, this closure uses only async-signal-safe system
        // calls (setsid and Linux's prctl syscall) and errno capture. It neither
        // allocates nor takes locks, and touches no parent-owned state.
        unsafe {
            command.as_std_mut().pre_exec(|| {
                if libc::setsid() == -1 {
                    return Err(std::io::Error::last_os_error());
                }
                #[cfg(target_os = "linux")]
                if libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) == -1 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
    }

    struct ProcessGroup(libc::pid_t);

    impl ProcessGroup {
        fn kill(&self) {
            // SAFETY: the positive pid came from our child, which made itself
            // session/group leader before exec. Negating it targets that group.
            unsafe { libc::kill(-self.0, libc::SIGKILL) };
        }
    }

    impl Drop for ProcessGroup {
        fn drop(&mut self) {
            self.kill();
        }
    }

    fn shape(header: &str, path: &Path, id: &str) -> std::io::Result<String> {
        let mut file = File::open(path)?;
        let length = file.metadata()?.len();
        let mut head_bytes = Vec::new();
        file.by_ref()
            .take(OUTPUT_WINDOW)
            .read_to_end(&mut head_bytes)?;
        let head = String::from_utf8_lossy(&head_bytes);
        let count = head.lines().count();
        if length <= OUTPUT_WINDOW && count <= 200 {
            return Ok(format!("{header}\n{head}"));
        }
        let mut tail_bytes = Vec::new();
        let (tail, omitted) = if length > OUTPUT_WINDOW {
            file.seek(SeekFrom::Start(length - OUTPUT_WINDOW))?;
            file.take(OUTPUT_WINDOW).read_to_end(&mut tail_bytes)?;
            (
                String::from_utf8_lossy(&tail_bytes),
                "many lines omitted".to_string(),
            )
        } else {
            (head.clone(), format!("{} lines omitted", count - 200))
        };
        let mut tail: Vec<_> = tail.lines().rev().take(160).collect();
        tail.reverse();
        Ok(format!(
            "{header}\n{}\n… [{omitted} · shell action=log id={id}] …\n{}",
            head.lines().take(40).collect::<Vec<_>>().join("\n"),
            tail.join("\n")
        ))
    }

    fn log(runs: &Path, args: &Value) -> anyhow::Result<String> {
        let id = args["id"].as_str().unwrap_or_default();
        if id.len() != 12 || !id.bytes().all(|c| c.is_ascii_hexdigit()) {
            return Ok(
                "invalid tool arguments: shell log id must be a 12-character hex id".to_string(),
            );
        }
        let offset = args["offset"].as_u64().unwrap_or(1).max(1);
        let skip = usize::try_from(offset - 1).unwrap_or(usize::MAX);
        let mut lines = BufReader::new(File::open(runs.join(format!("{id}.log")))?)
            .split(b'\n')
            .skip(skip)
            .take(401);
        let mut result = String::new();
        for line in lines.by_ref().take(400) {
            writeln!(result, "{}", String::from_utf8_lossy(&line?))?;
        }
        if let Some(next) = lines.next() {
            next?;
            let _ = write!(
                result,
                "… continue with offset={}",
                offset.saturating_add(400)
            );
        }
        Ok(result)
    }

    #[cfg(test)]
    mod tests {
        use std::sync::Arc;
        use std::time::Instant;

        use serde_json::json;

        use super::*;
        use crate::tools::{ToolBox, classify_tool_result, is_read_only_tool};

        struct Fixture {
            dir: PathBuf,
            toolbox: ToolBox,
        }

        impl Fixture {
            fn new() -> Self {
                let dir =
                    std::env::temp_dir().join(format!("nexus-shell-{}", uuid::Uuid::new_v4()));
                let root = dir.join("workspace");
                fs::create_dir_all(&root).unwrap();
                let mut toolbox = ToolBox::new(
                    dir.join("skills"),
                    None,
                    None,
                    "auto".to_string(),
                    Vec::new(),
                    Some(dir.join("test.db")),
                    None,
                    None,
                );
                toolbox.workspace = Some(Arc::new(Workspace::open(&root, Vec::new()).unwrap()));
                Self { dir, toolbox }
            }

            async fn run(&self, cmd: &str) -> String {
                self.toolbox
                    .run("shell", &json!({"action": "run", "cmd": cmd}).to_string())
                    .await
                    .0
            }
        }

        impl Drop for Fixture {
            fn drop(&mut self) {
                fs::remove_dir_all(&self.dir).unwrap();
            }
        }

        #[tokio::test]
        async fn exit_codes_and_combined_output() {
            let fixture = Fixture::new();
            let result = fixture.run("echo hi").await;
            assert!(result.starts_with("exit 0"), "{result}");
            assert!(result.contains("\nhi\n"), "{result}");
            assert!(fixture.run("exit 3").await.starts_with("exit 3"));
            let result = fixture
                .run("printf 'one\\n'; printf 'two\\n' >&2; printf 'three\\n'")
                .await;
            assert!(result.contains("\none\ntwo\nthree\n"), "{result}");
            assert!(
                fixture
                    .run("kill -TERM $$")
                    .await
                    .starts_with("killed by signal")
            );
            assert!(
                fixture
                    .run("read value; test -z \"$value\"")
                    .await
                    .starts_with("exit 0")
            );
        }

        #[tokio::test]
        async fn environment_is_allowlisted_and_noninteractive() {
            let fixture = Fixture::new();
            let result = fixture.run("env").await;
            assert!(result.starts_with("exit 0"), "{result}");
            for line in result.lines().skip(1) {
                let (key, _) = line.split_once('=').unwrap();
                assert!(
                    ENV_ALLOWLIST.contains(&key)
                        || [
                            "CI",
                            "GIT_TERMINAL_PROMPT",
                            "PAGER",
                            "GIT_PAGER",
                            "NO_COLOR",
                            "PWD",
                            "OLDPWD",
                            "SHLVL",
                            "_"
                        ]
                        .contains(&key),
                    "{key}"
                );
            }
            for entry in [
                "CI=1",
                "GIT_TERMINAL_PROMPT=0",
                "PAGER=cat",
                "GIT_PAGER=cat",
                "NO_COLOR=1",
            ] {
                assert!(result.lines().any(|line| line == entry), "{result}");
            }
            assert!(!result.contains("SUDO_ASKPASS="));
            assert!(fixture.run("test ! -t 0; test ! -t 1; test ! -t 2; if (echo x > /dev/tty) 2>/dev/null; then exit 9; fi").await.starts_with("exit 0"));
            #[cfg(target_os = "linux")]
            assert!(
                fixture
                    .run("grep '^NoNewPrivs:' /proc/self/status")
                    .await
                    .contains("NoNewPrivs:\t1")
            );
        }

        #[tokio::test]
        async fn scripts_hit_the_escalation_shims() {
            let fixture = Fixture::new();
            fs::write(fixture.dir.join("workspace/script.sh"), "sudo true\n").unwrap();
            let result = fixture.run("sh script.sh").await;
            assert!(result.starts_with("exit 1"), "{result}");
            assert!(
                result.contains("privilege escalation is blocked — ask the user to run it with !"),
                "{result}"
            );
            for name in ["sudo", "doas", "su", "pkexec", "run0"] {
                let metadata = fs::metadata(fixture.dir.join("runs/shims").join(name)).unwrap();
                assert_eq!(metadata.permissions().mode() & 0o777, 0o755);
            }
        }

        #[tokio::test]
        async fn unparseable_permission_never_bypasses_a_blocked_escalation() {
            let fixture = Fixture::new();
            let result = action(
                fixture.toolbox.workspace.as_ref().unwrap(),
                Some(&fixture.dir.join("test.db")),
                "shell_run",
                &json!({"cmd": "sudo true"}),
                true,
            )
            .await
            .unwrap();
            assert!(
                result.starts_with("privilege escalation is blocked"),
                "{result}"
            );
        }

        #[tokio::test]
        async fn refuses_escalation_bad_syntax_and_escaping_cwd() {
            let fixture = Fixture::new();
            for (cmd, prefix, kind) in [
                ("sudo true", "privilege escalation is blocked", "blocked"),
                (
                    "echo 'unterminated",
                    "could not parse this command",
                    "invalid_args",
                ),
            ] {
                let result = fixture.run(cmd).await;
                assert!(result.starts_with(prefix), "{result}");
                assert_eq!(classify_tool_result(&result, ""), Some(kind));
            }
            for cwd in ["..", "/", "missing"] {
                let result = fixture
                    .toolbox
                    .run(
                        "shell",
                        &json!({"action": "run", "cmd": "echo hi", "cwd": cwd}).to_string(),
                    )
                    .await
                    .0;
                assert!(
                    result.starts_with("path escapes") || result.starts_with("invalid path"),
                    "{result}"
                );
            }
            fs::create_dir(fixture.dir.join("workspace/sub")).unwrap();
            let result = fixture
                .toolbox
                .run(
                    "shell",
                    &json!({"action": "run", "cmd": "pwd", "cwd": "sub"}).to_string(),
                )
                .await
                .0;
            assert!(
                result.contains(
                    fixture
                        .dir
                        .join("workspace/sub")
                        .canonicalize()
                        .unwrap()
                        .to_str()
                        .unwrap()
                ),
                "{result}"
            );
        }

        fn running(pid: libc::pid_t) -> bool {
            // SAFETY: signal zero only probes existence; it does not signal.
            if unsafe { libc::kill(pid, 0) } == -1 {
                return false;
            }
            // A killed orphan may briefly remain a zombie until init reaps it.
            #[cfg(target_os = "linux")]
            if fs::read_to_string(format!("/proc/{pid}/stat"))
                .is_ok_and(|stat| stat.contains(") Z "))
            {
                return false;
            }
            true
        }

        async fn assert_sleep_gone(fixture: &Fixture) {
            let pid = fs::read_to_string(fixture.dir.join("workspace/sleep.pid"))
                .unwrap()
                .trim()
                .parse()
                .unwrap();
            for _ in 0..100 {
                if !running(pid) {
                    return;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
            assert!(!running(pid), "background sleep {pid} survived");
        }

        #[tokio::test]
        async fn timeout_kills_background_descendants() {
            let fixture = Fixture::new();
            let started = Instant::now();
            let result = fixture.toolbox.run("shell", &json!({
                "action": "run", "cmd": "sh -c 'sleep 30 & echo $! > sleep.pid; sleep 30'", "timeout_s": 1,
            }).to_string()).await.0;
            assert!(started.elapsed() < Duration::from_secs(5));
            assert!(result.starts_with("timed out after 1 s"), "{result}");
            assert_eq!(classify_tool_result(&result, ""), Some("upstream"));
            assert_sleep_gone(&fixture).await;
        }

        #[tokio::test]
        async fn cancellation_kills_background_descendants() {
            let fixture = Arc::new(Fixture::new());
            let task_fixture = Arc::clone(&fixture);
            let task = tokio::spawn(async move {
                task_fixture
                    .run("sleep 30 & echo $! > sleep.pid; wait")
                    .await
            });
            tokio::time::timeout(Duration::from_secs(3), async {
                while !fs::read_to_string(fixture.dir.join("workspace/sleep.pid"))
                    .is_ok_and(|pid| !pid.trim().is_empty())
                {
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
            })
            .await
            .unwrap();
            task.abort();
            assert!(task.await.unwrap_err().is_cancelled());
            assert_sleep_gone(&fixture).await;
        }

        #[tokio::test]
        async fn long_output_is_shaped_and_full_log_is_pageable() {
            let fixture = Fixture::new();
            let result = fixture
                .run("awk 'BEGIN { for (i=1; i<=650; i++) print i }'")
                .await;
            assert!(result.starts_with("exit 0"), "{result}");
            assert!(
                result.contains("\n39\n40\n… [450 lines omitted"),
                "{result}"
            );
            assert!(result.contains("…\n491\n492\n"), "{result}");
            assert!(result.ends_with("\n650"), "{result}");
            let id = result
                .split("id=")
                .nth(1)
                .unwrap()
                .split_whitespace()
                .next()
                .unwrap();
            for (offset, start, end) in [(1, 1, 400), (401, 401, 650)] {
                let page = fixture
                    .toolbox
                    .run(
                        "shell",
                        &json!({"action": "log", "id": id, "offset": offset}).to_string(),
                    )
                    .await
                    .0;
                let expected: Vec<_> = (start..=end).map(|i| i.to_string()).collect();
                assert_eq!(
                    page.lines().take(expected.len()).collect::<Vec<_>>(),
                    expected
                );
                assert_eq!(page.contains("… continue with offset=401"), offset == 1);
            }
            let result = fixture
                .run("awk 'BEGIN { for (i=0; i<10000; i++) printf \"x\" }'")
                .await;
            assert!(result.chars().count() <= 8000);
            assert!(result.ends_with("(tool result truncated)"));
            let result = fixture
                .toolbox
                .run("shell", r#"{"action":"log","id":"../escape"}"#)
                .await
                .0;
            assert!(result.starts_with("invalid tool arguments"));
        }

        #[tokio::test]
        async fn output_limit_kills_the_group_while_running_and_checks_fast_exits() {
            let fixture = Fixture::new();
            let started = Instant::now();
            let result = fixture
                .run("sleep 30 & echo $! > sleep.pid; yes | head -c 262144; wait")
                .await;
            assert!(started.elapsed() < Duration::from_secs(5));
            assert!(result.starts_with(OUTPUT_EXCEEDED), "{result}");
            assert_sleep_gone(&fixture).await;
            let result = fixture.run("yes | head -c 262144").await;
            assert!(result.starts_with(OUTPUT_EXCEEDED), "{result}");
        }

        #[test]
        fn large_log_shape_seeks_to_the_tail() {
            let fixture = Fixture::new();
            let path = fixture.dir.join("large.log");
            let lines: Vec<_> = (1..=16000)
                .map(|i| format!("{i:05} {}", "x".repeat(80)))
                .collect();
            fs::write(&path, format!("{}\n", lines.join("\n"))).unwrap();
            assert!(fs::metadata(&path).unwrap().len() > OUTPUT_WINDOW);
            let result = shape("exit 0", &path, "0123456789ab").unwrap();
            assert!(result.starts_with(&format!(
                "exit 0\n{}\n… [many lines omitted",
                lines[..40].join("\n")
            )));
            assert!(result.ends_with(&lines[lines.len() - 160..].join("\n")));
        }

        #[test]
        fn prunes_only_logs_older_than_seven_days_and_namespaces_temp_runs() {
            let fixture = Fixture::new();
            let runs = fixture.dir.join("runs");
            fs::create_dir(&runs).unwrap();
            for name in ["old.log", "recent.log", "keep.txt"] {
                fs::write(runs.join(name), "log contents").unwrap();
            }
            let old = std::time::SystemTime::now() - Duration::from_hours(8 * 24);
            for name in ["old.log", "keep.txt"] {
                File::options()
                    .write(true)
                    .open(runs.join(name))
                    .unwrap()
                    .set_times(fs::FileTimes::new().set_modified(old))
                    .unwrap();
            }
            fs::create_dir(runs.join("keep.log")).unwrap();
            prune_logs(&runs).unwrap();
            assert!(!runs.join("old.log").exists());
            assert!(runs.join("recent.log").exists());
            assert!(runs.join("keep.txt").exists());
            assert!(runs.join("keep.log").is_dir());
            assert_eq!(runs_dir(None), std::env::temp_dir().join("nexus-runs"));
        }

        #[tokio::test]
        async fn definitions_routing_and_workspace_gate() {
            let mut fixture = Fixture::new();
            for defs in [fixture.toolbox.defs(), fixture.toolbox.defs_compact()] {
                let fs_index = defs.iter().position(|def| def.name == "fs").unwrap();
                assert_eq!(defs[fs_index + 1].name, "shell");
                assert_eq!(
                    defs[fs_index + 1].parameters["properties"]["action"]["enum"],
                    json!(["run", "log"])
                );
            }
            for name in ["shell", "shell_run"] {
                assert!(!is_read_only_tool(
                    name,
                    r#"{"action":"run","cmd":"echo hi"}"#
                ));
            }
            for name in ["shell", "shell_log"] {
                assert!(is_read_only_tool(
                    name,
                    r#"{"action":"log","id":"0123456789ab"}"#
                ));
            }
            fixture.toolbox.workspace = None;
            for defs in [fixture.toolbox.defs(), fixture.toolbox.defs_compact()] {
                assert!(!defs.iter().any(|def| def.name == "shell"));
            }
            for (name, args) in [
                ("shell", r#"{"action":"run","cmd":"echo hi"}"#),
                ("shell_run", r#"{"cmd":"echo hi"}"#),
                ("shell_log", r#"{"id":"0123456789ab"}"#),
            ] {
                assert_eq!(
                    fixture.toolbox.run(name, args).await.0,
                    "shell needs an agent workspace · switch with /agent <path>"
                );
            }
        }
    }
}
