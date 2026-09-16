//! Managed local-provider servers: start and stop a runtime's own inference
//! server, detect which endpoints are already answering, and measure what
//! each one costs in memory.
//!
//! Two things are deliberately kept apart. A server Nexus **started** is
//! owned: it is tracked by child handle and stopped when Nexus stops it or
//! exits. A server that merely **answers** on the runtime's endpoint — one
//! started in another terminal, or a system service — is detected and used
//! but never stopped, because nothing here started it.
//!
//! Memory is advisory. Going over budget reports and warns; it never kills a
//! server, so a reply that is mid-generation is not yanked out from under the
//! user. `/local stop` stays the only thing that ends a server.

use std::collections::HashMap;
use std::process::Stdio;
use std::time::Duration;

use anyhow::{Context as _, Result};
use serde::{Deserialize, Serialize};
use tokio::process::{Child, Command};

use super::local::{LocalConfig, LocalRuntime};

/// Fraction of physical memory a local server may take before Nexus warns,
/// when no explicit `memory_budget_mb` is configured. Local inference is
/// supposed to leave the machine usable; past this the box starts swapping.
const DEFAULT_BUDGET_FRACTION: f64 = 0.70;

/// How long a readiness/liveness probe waits before calling an endpoint down.
const PROBE_TIMEOUT: Duration = Duration::from_millis(1500);

/// How a runtime's server is started.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Launch {
    /// A foreground server Nexus owns for as long as it runs.
    Owned {
        argv: Vec<String>,
        env: Vec<(String, String)>,
    },
    /// A helper command that hands the server to a background daemon Nexus
    /// does not own (LM Studio). Stopping runs the paired command rather than
    /// killing a child.
    Detached {
        start: Vec<String>,
        stop: Vec<String>,
    },
}

impl LocalRuntime {
    /// Whether starting this runtime's server requires naming a model.
    /// Ollama and LM Studio load models on demand through their own API;
    /// MLX and edge0 each serve exactly one model, chosen at launch.
    #[must_use]
    pub const fn serves_one_model(self) -> bool {
        matches!(self, Self::Mlx | Self::Edge0)
    }

    /// The command that starts this runtime's server on `port`.
    ///
    /// # Errors
    /// Reports the missing model for runtimes that serve exactly one.
    pub fn launch(self, model: Option<&str>, port: u16) -> Result<Launch> {
        let model = || -> Result<String> {
            model
                .map(str::to_owned)
                .filter(|model| !model.trim().is_empty())
                .with_context(|| {
                    format!(
                        "{} serves one model at a time — pick a local model in /model first",
                        self.label()
                    )
                })
        };
        Ok(match self {
            // Ollama takes its listen address from the environment, not argv.
            Self::Ollama => Launch::Owned {
                argv: vec!["ollama".into(), "serve".into()],
                env: vec![("OLLAMA_HOST".into(), format!("127.0.0.1:{port}"))],
            },
            Self::Mlx => Launch::Owned {
                argv: vec![
                    "mlx_lm.server".into(),
                    "--model".into(),
                    model()?,
                    "--host".into(),
                    "127.0.0.1".into(),
                    "--port".into(),
                    port.to_string(),
                ],
                env: Vec::new(),
            },
            Self::Edge0 => Launch::Owned {
                argv: vec![
                    "edge0".into(),
                    "serve".into(),
                    model()?,
                    "--host".into(),
                    "127.0.0.1".into(),
                    "--port".into(),
                    port.to_string(),
                ],
                env: Vec::new(),
            },
            // `lms server start` returns once the daemon is up; the server it
            // starts belongs to LM Studio, so it is stopped the same way.
            Self::Lmstudio => Launch::Detached {
                start: vec![
                    "lms".into(),
                    "server".into(),
                    "start".into(),
                    "--port".into(),
                    port.to_string(),
                ],
                stop: vec!["lms".into(), "server".into(), "stop".into()],
            },
        })
    }
}

/// One runtime's live server state, as shown by `/local status` and the
/// `/local` picker.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RuntimeStatus {
    pub runtime: LocalRuntime,
    pub endpoint: String,
    pub port: Option<u16>,
    /// The endpoint answered a `/v1/models` probe.
    pub running: bool,
    /// Nexus started this server and will stop it on exit.
    pub managed: bool,
    /// Resident memory of the listener's whole process tree, in KiB.
    pub rss_kb: Option<u64>,
    /// `rss_kb` is over the configured (or default) budget.
    pub over_budget: bool,
    /// The budget in force when this was measured, in KiB.
    pub budget_kb: Option<u64>,
}

impl RuntimeStatus {
    /// A compact `edge0 ●4.1GB ⚠` cell for the one-line status summary.
    #[must_use]
    pub fn summary(&self) -> String {
        let mut cell = format!(
            "{} {}",
            self.runtime.label(),
            if self.running { '●' } else { '○' }
        );
        if let Some(rss) = self.rss_kb {
            cell.push_str(&format_kb(rss));
        }
        if self.over_budget {
            cell.push_str(" ⚠");
        }
        cell
    }
}

/// Render KiB as a short human size: `4.1 GB`, `812 MB`.
#[must_use]
pub fn format_kb(kb: u64) -> String {
    #[allow(clippy::cast_precision_loss)]
    let mb = kb as f64 / 1024.0;
    if mb >= 1024.0 {
        format!("{:.1} GB", mb / 1024.0)
    } else if mb >= 1.0 {
        format!("{mb:.0} MB")
    } else {
        // A measured process is never "0 MB"; rounding must not say it is.
        "<1 MB".to_string()
    }
}

/// The servers Nexus started, and only those. Dropping the registry kills the
/// owned children (`kill_on_drop`), so quitting never leaves a multi-gigabyte
/// server behind; detached daemons are stopped explicitly.
#[derive(Default)]
pub struct ManagedServers {
    owned: HashMap<LocalRuntime, Child>,
    /// Runtimes whose detached daemon Nexus started, and so may stop.
    detached: Vec<LocalRuntime>,
}

impl ManagedServers {
    /// Whether Nexus started this runtime's server.
    #[must_use]
    pub fn manages(&self, runtime: LocalRuntime) -> bool {
        self.owned.contains_key(&runtime) || self.detached.contains(&runtime)
    }

    /// The owned server's process id, when there is one.
    #[must_use]
    pub fn pid(&self, runtime: LocalRuntime) -> Option<u32> {
        self.owned.get(&runtime).and_then(Child::id)
    }

    /// Drop handles for owned servers that have already exited, so a crashed
    /// server stops being reported as managed.
    pub fn reap(&mut self) {
        self.owned
            .retain(|_, child| !matches!(child.try_wait(), Ok(Some(_))));
    }

    /// Start `runtime`'s server. Returns the argv that was launched, for the
    /// status line.
    ///
    /// # Errors
    /// Reports an already-managed runtime, a missing model, and a server
    /// binary that is not installed.
    pub fn start(
        &mut self,
        runtime: LocalRuntime,
        model: Option<&str>,
        port: u16,
    ) -> Result<String> {
        self.reap();
        anyhow::ensure!(
            !self.manages(runtime),
            "{} is already started by Nexus — /local stop {} first",
            runtime.label(),
            runtime.token()
        );
        let launch = runtime.launch(model, port)?;
        let (argv, env, detached) = match &launch {
            Launch::Owned { argv, env } => (argv.clone(), env.clone(), false),
            Launch::Detached { start, .. } => (start.clone(), Vec::new(), true),
        };
        let program = argv.first().context("empty launch command")?;
        let mut command = Command::new(program);
        command
            .args(&argv[1..])
            .envs(env)
            .stdin(Stdio::null())
            // A server's logs belong in its own terminal, not painted over
            // the TUI; failures surface through the readiness probe instead.
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .kill_on_drop(true);
        let mut child = command
            .spawn()
            .with_context(|| format!("starting {program}; is {} installed?", runtime.label()))?;
        if detached {
            // The helper exits as soon as the daemon is up; waiting on it here
            // would block the UI, so it is reaped in the background.
            if let Ok(handle) = tokio::runtime::Handle::try_current() {
                handle.spawn(async move {
                    let _ = child.wait().await;
                });
            }
            self.detached.push(runtime);
        } else {
            self.owned.insert(runtime, child);
        }
        Ok(argv.join(" "))
    }

    /// Stop a server Nexus started. Servers it did not start are left alone.
    /// Signalling is synchronous so the caller stays sync; the wait that
    /// reaps the child, and the detached daemon's stop helper, run detached.
    ///
    /// `wait_for_exit` is for restarts: the killed server only releases the
    /// port as it is torn down, so starting its replacement immediately can
    /// race it onto a still-bound address. Reaping first removes the race.
    ///
    /// # Errors
    /// Reports a runtime Nexus is not managing.
    pub fn stop(&mut self, runtime: LocalRuntime, port: u16, wait_for_exit: bool) -> Result<()> {
        if let Some(mut child) = self.owned.remove(&runtime) {
            let _ = child.start_kill();
            if wait_for_exit {
                reap_blocking(&mut child);
                return Ok(());
            }
            if let Ok(handle) = tokio::runtime::Handle::try_current() {
                handle.spawn(async move {
                    let _ = child.wait().await;
                });
            }
            return Ok(());
        }
        if let Some(index) = self.detached.iter().position(|r| *r == runtime) {
            self.detached.remove(index);
            if let Ok(Launch::Detached { stop, .. }) = runtime.launch(None, port)
                && let Ok(handle) = tokio::runtime::Handle::try_current()
            {
                handle.spawn(async move {
                    let _ = Command::new(&stop[0])
                        .args(&stop[1..])
                        .stdin(Stdio::null())
                        .stdout(Stdio::null())
                        .stderr(Stdio::null())
                        .status()
                        .await;
                });
            }
            return Ok(());
        }
        anyhow::bail!(
            "Nexus did not start {} — stop it where you started it",
            runtime.label()
        )
    }

    /// Every runtime Nexus currently manages.
    #[must_use]
    pub fn managed_runtimes(&self) -> Vec<LocalRuntime> {
        self.owned
            .keys()
            .copied()
            .chain(self.detached.iter().copied())
            .collect()
    }

    /// Stop every server Nexus started and wait for the owned ones to go.
    /// This is the exit path: quitting must not leave gigabytes behind.
    pub async fn stop_all(&mut self) {
        for (_, mut child) in self.owned.drain() {
            let _ = child.kill().await;
            let _ = child.wait().await;
        }
        for runtime in std::mem::take(&mut self.detached) {
            let port = LocalConfig::for_runtime(runtime, None)
                .port()
                .unwrap_or_default();
            if let Ok(Launch::Detached { stop, .. }) = runtime.launch(None, port) {
                let _ = Command::new(&stop[0])
                    .args(&stop[1..])
                    .stdin(Stdio::null())
                    .stdout(Stdio::null())
                    .stderr(Stdio::null())
                    .status()
                    .await;
            }
        }
    }
}

/// Wait, briefly and synchronously, for a just-killed child to be reaped.
///
/// This blocks the caller, which is the point: it runs only on the restart
/// path, where the next step needs the port back. A `SIGKILL`ed process is
/// normally gone within a millisecond or two, and the bound caps the stall at
/// half a second even if something is wedged.
fn reap_blocking(child: &mut Child) {
    let deadline = std::time::Instant::now() + Duration::from_millis(500);
    while std::time::Instant::now() < deadline {
        match child.try_wait() {
            Ok(Some(_)) | Err(_) => return,
            Ok(None) => std::thread::sleep(Duration::from_millis(5)),
        }
    }
}

/// Whether an OpenAI-wire endpoint is answering. `/v1/models` is the one
/// route every supported runtime serves, so it doubles as the liveness check.
pub async fn probe(endpoint: &str) -> bool {
    let Ok(client) = reqwest::Client::builder().timeout(PROBE_TIMEOUT).build() else {
        return false;
    };
    let url = format!("{}/models", endpoint.trim_end_matches('/'));
    client
        .get(url)
        .send()
        .await
        .is_ok_and(|response| response.status().is_success())
}

/// The pid listening on a loopback TCP port, via `lsof`. Used only for
/// servers Nexus did not start; an owned server reports its child pid
/// directly. Absent `lsof` simply means no memory reading.
pub async fn listener_pid(port: u16) -> Option<u32> {
    let output = Command::new("lsof")
        .args(["-nP", "-sTCP:LISTEN", "-t", &format!("-iTCP:{port}")])
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .await
        .ok()?;
    output
        .stdout
        .split(|byte| *byte == b'\n')
        .filter_map(|line| std::str::from_utf8(line).ok())
        .find_map(|line| line.trim().parse().ok())
}

/// Resident memory of `root` and every process descended from it, in KiB.
///
/// The tree matters: Ollama's server spawns a `runner` child that holds the
/// weights, and edge0/MLX servers are Python parents of their own workers.
/// Charging only the listener would report a few megabytes for a server
/// holding gigabytes.
pub async fn tree_rss_kb(root: u32) -> Option<u64> {
    let output = Command::new("ps")
        .args(["-axo", "pid=,ppid=,rss="])
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .await
        .ok()?;
    let table = String::from_utf8_lossy(&output.stdout);
    Some(sum_tree(&table, root))
}

/// Sum the `pid ppid rss` rows reachable from `root`, following ppid edges.
fn sum_tree(table: &str, root: u32) -> u64 {
    let rows: Vec<(u32, u32, u64)> = table
        .lines()
        .filter_map(|line| {
            let mut fields = line.split_whitespace();
            let process = fields.next()?.parse().ok()?;
            let parent = fields.next()?.parse().ok()?;
            let rss = fields.next()?.parse().ok()?;
            Some((process, parent, rss))
        })
        .collect();
    let mut total = 0;
    let mut seen: Vec<u32> = Vec::new();
    let mut frontier = vec![root];
    while let Some(pid) = frontier.pop() {
        if seen.contains(&pid) {
            continue;
        }
        seen.push(pid);
        for (process, parent, rss) in &rows {
            if *process == pid {
                total += rss;
            } else if *parent == pid {
                frontier.push(*process);
            }
        }
    }
    total
}

/// Physical memory in KiB, for the default budget.
pub async fn total_memory_kb() -> Option<u64> {
    #[cfg(target_os = "macos")]
    {
        let output = Command::new("sysctl")
            .args(["-n", "hw.memsize"])
            .stdin(Stdio::null())
            .stderr(Stdio::null())
            .output()
            .await
            .ok()?;
        let bytes: u64 = String::from_utf8_lossy(&output.stdout)
            .trim()
            .parse()
            .ok()?;
        Some(bytes / 1024)
    }
    #[cfg(not(target_os = "macos"))]
    {
        let meminfo = tokio::fs::read_to_string("/proc/meminfo").await.ok()?;
        parse_mem_total(&meminfo)
    }
}

/// `MemTotal:  16303152 kB` from `/proc/meminfo`.
#[cfg_attr(target_os = "macos", allow(dead_code))]
fn parse_mem_total(meminfo: &str) -> Option<u64> {
    meminfo
        .lines()
        .find_map(|line| line.strip_prefix("MemTotal:"))
        .and_then(|rest| rest.split_whitespace().next()?.parse().ok())
}

/// Probe every runtime and price the ones that answer.
///
/// `configured` supplies the endpoint override and budget for whichever
/// runtime is selected; the others are probed at their default endpoints, so
/// a server left running under a runtime you are not currently using still
/// shows up as occupying memory.
pub async fn survey(
    configured: Option<&LocalConfig>,
    managed: &[(LocalRuntime, Option<u32>)],
) -> Vec<RuntimeStatus> {
    let budget = budget_kb(configured).await;
    let mut statuses = Vec::new();
    for runtime in LocalRuntime::ALL {
        let config = match configured {
            Some(config) if config.provider == runtime => config.clone(),
            _ => LocalConfig::for_runtime(runtime, None),
        };
        let endpoint = config.endpoint().to_string();
        let port = config.port();
        let running = probe(&endpoint).await;
        // A server Nexus owns reports its own child pid. A detached daemon it
        // started, and any server it did not start, is found on the port.
        let entry = managed.iter().find(|(candidate, _)| *candidate == runtime);
        let pid = match entry.and_then(|(_, pid)| *pid) {
            Some(pid) => Some(pid),
            None if running => match port {
                Some(port) => listener_pid(port).await,
                None => None,
            },
            None => None,
        };
        let rss_kb = match pid {
            Some(pid) if running => tree_rss_kb(pid).await.filter(|kb| *kb > 0),
            _ => None,
        };
        statuses.push(RuntimeStatus {
            runtime,
            endpoint,
            port,
            running,
            managed: entry.is_some(),
            rss_kb,
            over_budget: matches!((rss_kb, budget), (Some(rss), Some(budget)) if rss > budget),
            budget_kb: budget,
        });
    }
    statuses
}

/// The memory a local server may take before Nexus warns, in KiB: the
/// configured `memory_budget_mb`, else a fraction of physical memory.
pub async fn budget_kb(configured: Option<&LocalConfig>) -> Option<u64> {
    if let Some(mb) = configured.and_then(|config| config.memory_budget_mb) {
        return Some(mb.saturating_mul(1024));
    }
    let total = total_memory_kb().await?;
    #[allow(
        clippy::cast_precision_loss,
        clippy::cast_sign_loss,
        clippy::cast_possible_truncation
    )]
    Some((total as f64 * DEFAULT_BUDGET_FRACTION) as u64)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn launch_commands_carry_the_port_and_require_a_model_where_it_is_served() {
        let Launch::Owned { argv, env } = LocalRuntime::Ollama.launch(None, 11434).unwrap() else {
            panic!("ollama is an owned server");
        };
        assert_eq!(argv, ["ollama", "serve"]);
        assert_eq!(env, [("OLLAMA_HOST".to_string(), "127.0.0.1:11434".into())]);

        let Launch::Owned { argv, .. } =
            LocalRuntime::Edge0.launch(Some("edge0-8b"), 8000).unwrap()
        else {
            panic!("edge0 is an owned server");
        };
        assert_eq!(
            argv,
            [
                "edge0",
                "serve",
                "edge0-8b",
                "--host",
                "127.0.0.1",
                "--port",
                "8000"
            ]
        );

        let Launch::Detached { start, stop } = LocalRuntime::Lmstudio.launch(None, 1234).unwrap()
        else {
            panic!("LM Studio hands off to its own daemon");
        };
        assert_eq!(start, ["lms", "server", "start", "--port", "1234"]);
        assert_eq!(stop, ["lms", "server", "stop"]);

        // A one-model runtime with nothing selected explains itself rather
        // than launching a server with no weights.
        for runtime in [LocalRuntime::Mlx, LocalRuntime::Edge0] {
            assert!(runtime.serves_one_model());
            let error = runtime.launch(None, 8000).unwrap_err().to_string();
            assert!(error.contains("serves one model"), "{error}");
            assert!(runtime.launch(Some("  "), 8000).is_err());
        }
        assert!(!LocalRuntime::Ollama.serves_one_model());
    }

    #[test]
    fn tree_rss_sums_descendants_not_just_the_listener() {
        // A listener holding little, with the weights in a runner child and a
        // grandchild below it — plus an unrelated tree that must not count.
        let table = "\
  100     1   4096
  200   100 1048576
  300   200  524288
  400     1  999999
";
        assert_eq!(sum_tree(table, 100), 4096 + 1_048_576 + 524_288);
        assert_eq!(sum_tree(table, 200), 1_048_576 + 524_288);
        assert_eq!(sum_tree(table, 999), 0);
        // Garbage rows are skipped, not panicked on.
        assert_eq!(sum_tree("not a row\n  100 1 8\n", 100), 8);
    }

    #[test]
    fn sizes_and_meminfo_are_human_readable() {
        assert_eq!(format_kb(512), "<1 MB");
        assert_eq!(format_kb(1536), "2 MB");
        assert_eq!(format_kb(4096 * 1024), "4.0 GB");
        assert_eq!(format_kb(800 * 1024), "800 MB");
        assert_eq!(
            parse_mem_total("MemFree: 12 kB\nMemTotal:  16303152 kB\n"),
            Some(16_303_152)
        );
        assert_eq!(parse_mem_total("MemFree: 12 kB\n"), None);
    }

    /// The `ps`/`sysctl`/`/proc` readings are format-sensitive, so exercise
    /// them against this very process on the real platform. Hermetic: no
    /// network, no writes, nothing spawned.
    #[cfg(unix)]
    #[tokio::test]
    async fn measures_this_process_on_the_real_platform() {
        let rss = tree_rss_kb(std::process::id())
            .await
            .expect("ps reports this process");
        assert!(rss > 0, "the test process has resident memory");
        let total = total_memory_kb()
            .await
            .expect("physical memory is readable");
        assert!(total > rss, "a process cannot outweigh physical memory");
        // The default budget is a slice of the machine, not all of it.
        let budget = budget_kb(None).await.expect("a default budget exists");
        assert!(budget > 0 && budget < total, "{budget} of {total}");
    }

    /// A port with nothing on it is down, and reports so rather than hanging.
    #[tokio::test]
    async fn a_closed_port_is_not_running() {
        assert!(!probe("http://127.0.0.1:9/v1").await);
        assert_eq!(listener_pid(9).await, None);
    }

    #[test]
    fn status_summary_marks_running_and_over_budget() {
        let status = RuntimeStatus {
            runtime: LocalRuntime::Edge0,
            endpoint: "http://localhost:8000/v1".into(),
            port: Some(8000),
            running: true,
            managed: true,
            rss_kb: Some(4300 * 1024),
            over_budget: true,
            budget_kb: Some(3000 * 1024),
        };
        assert_eq!(status.summary(), "edge0 ●4.2 GB ⚠");
        let stopped = RuntimeStatus {
            running: false,
            managed: false,
            rss_kb: None,
            over_budget: false,
            ..status
        };
        assert_eq!(stopped.summary(), "edge0 ○");
    }
}
