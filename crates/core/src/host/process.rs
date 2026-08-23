//! Host-side child-process management: `cloudflared` quick tunnels and
//! platform sleep inhibitors. These helpers are never started by unit tests;
//! the CLI opts into them only for `nexus host --tunnel` or normal hosting.

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use anyhow::{Context as _, Result, bail};
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::{Child, Command};

/// The supported per-user service manager for the current platform.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ServiceManager {
    /// Linux user-level `systemd` service.
    Systemd,
    /// macOS per-user `launchd` agent.
    Launchd,
}

/// Return the service manager supported by this build target.
#[must_use]
pub const fn service_manager() -> Option<ServiceManager> {
    #[cfg(target_os = "linux")]
    {
        return Some(ServiceManager::Systemd);
    }
    #[cfg(target_os = "macos")]
    {
        return Some(ServiceManager::Launchd);
    }
    #[allow(unreachable_code)]
    None
}

/// A child process that is killed when hosting stops.
pub struct ChildGuard {
    child: Option<Child>,
}

impl ChildGuard {
    /// Kill the guarded process and wait for it to exit.
    pub async fn stop(&mut self) {
        if let Some(child) = self.child.as_mut() {
            let _ = child.kill().await;
            let _ = child.wait().await;
        }
        self.child = None;
    }
}

/// A quick `trycloudflare.com` tunnel process. Named-tunnel configuration is
/// deliberately supplied by the CLI/setup layer; this type only owns the
/// sidecar lifecycle and URL discovery.
pub struct Tunnel {
    child: Option<Child>,
    public_url: Option<String>,
}

impl Tunnel {
    /// Start a quick tunnel to a loopback host port. URL discovery is bounded;
    /// a running process with no parsed URL is still returned so callers can
    /// report "started, URL unknown" instead of hanging forever.
    pub async fn quick(port: u16) -> Result<Self> {
        let mut child = Command::new("cloudflared")
            .args([
                "tunnel",
                "--no-autoupdate",
                "--url",
                &format!("http://127.0.0.1:{port}"),
            ])
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .stdin(Stdio::null())
            .kill_on_drop(true)
            .spawn()
            .context("starting cloudflared quick tunnel")?;
        let stderr = child
            .stderr
            .take()
            .context("capturing cloudflared output")?;
        let mut lines = BufReader::new(stderr).lines();
        let deadline = tokio::time::sleep(Duration::from_secs(15));
        tokio::pin!(deadline);
        let mut public_url = None;
        loop {
            tokio::select! {
                line = lines.next_line() => match line {
                    Ok(Some(line)) => {
                        if let Some(url) = parse_trycloudflare_url(&line) {
                            public_url = Some(url);
                            break;
                        }
                    }
                    _ => break,
                },
                () = &mut deadline => break,
            }
        }
        // Keep draining stderr after discovery so a verbose sidecar cannot
        // block on a full pipe. The task ends when the child is killed.
        tokio::spawn(async move { while lines.next_line().await.ok().flatten().is_some() {} });
        Ok(Self {
            child: Some(child),
            public_url,
        })
    }

    /// Start a named tunnel from a generated `cloudflared` config.
    pub fn named(config: &std::path::Path, tunnel_id: &str) -> Result<Self> {
        let mut child = Command::new("cloudflared")
            .args(["tunnel", "--config"])
            .arg(config)
            .args(["run", tunnel_id])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .stdin(Stdio::null())
            .kill_on_drop(true)
            .spawn()
            .context("starting cloudflared named tunnel")?;
        // Named tunnels use the hostname selected during setup, so there is
        // no quick-tunnel URL to parse here.
        if child.try_wait().ok().flatten().is_some() {
            bail!("cloudflared named tunnel exited during startup");
        }
        Ok(Self {
            child: Some(child),
            public_url: None,
        })
    }

    /// The parsed public URL, if cloudflared printed one during startup.
    pub fn public_url(&self) -> Option<&str> {
        self.public_url.as_deref()
    }

    /// Wait until the sidecar exits, returning its success status.
    pub async fn wait(&mut self) -> Option<bool> {
        let child = self.child.as_mut()?;
        Some(child.wait().await.is_ok_and(|status| status.success()))
    }

    /// Kill the sidecar and wait for it to exit.
    pub async fn stop(&mut self) {
        if let Some(child) = self.child.as_mut() {
            let _ = child.kill().await;
            let _ = child.wait().await;
        }
        self.child = None;
    }
}

/// Start the platform's sleep inhibitor. Unsupported platforms return
/// `Ok(None)`; a missing Linux `systemd-inhibit` is an actionable error for
/// the caller, which may choose to continue with a warning.
pub fn sleep_guard() -> Result<Option<ChildGuard>> {
    #[cfg(target_os = "macos")]
    {
        let child = Command::new("caffeinate")
            .args(["-dimsu"])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .spawn()
            .context("starting macOS caffeinate")?;
        return Ok(Some(ChildGuard { child: Some(child) }));
    }
    #[cfg(target_os = "linux")]
    {
        let child = Command::new("systemd-inhibit")
            .args([
                "--what=sleep",
                "--who=nexus",
                "--mode=block",
                "sleep",
                "infinity",
            ])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .spawn()
            .context("starting systemd-inhibit")?;
        return Ok(Some(ChildGuard { child: Some(child) }));
    }
    #[allow(unreachable_code)]
    Ok(None)
}

/// Install the host service for the current user and return its file path.
///
/// This writes only the service definition; it does not start a service or
/// invoke a platform service manager. That keeps installation reversible and
/// safe in headless environments. The CLI prints the activation command.
pub fn install_service(port: u16, tunnel: bool) -> Result<PathBuf> {
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .context("HOME is not set; cannot locate the user service directory")?;
    let binary = std::env::current_exe().context("locating the nexus executable")?;
    let manager =
        service_manager().context("host service installation is unsupported on this platform")?;
    let path = service_path(&config_home(&home), &home, manager);
    write_service(&path, manager, &binary, port, tunnel)
}

/// Write a service definition below `home`. This split-out form keeps service
/// rendering tests hermetic and avoids touching XDG directories in tests.
pub fn install_service_at(home: &Path, binary: &Path, port: u16, tunnel: bool) -> Result<PathBuf> {
    let manager =
        service_manager().context("host service installation is unsupported on this platform")?;
    let path = service_path(&home.join(".config"), home, manager);
    write_service(&path, manager, binary, port, tunnel)
}

fn write_service(
    path: &Path,
    manager: ServiceManager,
    binary: &Path,
    port: u16,
    tunnel: bool,
) -> Result<PathBuf> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creating service directory {}", parent.display()))?;
    }
    let contents = match manager {
        ServiceManager::Systemd => systemd_unit(binary, port, tunnel),
        ServiceManager::Launchd => launchd_plist(binary, port, tunnel),
    };
    std::fs::write(path, contents).with_context(|| format!("writing {}", path.display()))?;
    Ok(path.to_path_buf())
}

/// Remove the current user's host service definition, returning whether it
/// existed. It does not stop an already-running service.
pub fn uninstall_service() -> Result<bool> {
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .context("HOME is not set; cannot locate the user service directory")?;
    let Some(manager) = service_manager() else {
        bail!("host service installation is unsupported on this platform");
    };
    let path = service_path(&config_home(&home), &home, manager);
    match std::fs::remove_file(&path) {
        Ok(()) => Ok(true),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error).with_context(|| format!("removing {}", path.display())),
    }
}

/// Where a service definition lives. `config_home` is the systemd user-unit
/// root (`$XDG_CONFIG_HOME`), `home` the launchd agent root — kept as separate
/// arguments so callers with a synthetic layout (tests) never depend on the
/// ambient environment.
fn service_path(config_home: &Path, home: &Path, manager: ServiceManager) -> PathBuf {
    match manager {
        ServiceManager::Systemd => config_home.join("systemd/user").join("nexus-host.service"),
        ServiceManager::Launchd => home
            .join("Library/LaunchAgents")
            .join("chat.nexus.host.plist"),
    }
}

/// `$XDG_CONFIG_HOME` when set to an absolute path, else `<home>/.config`.
/// A relative value is ignored, as the XDG base-directory spec requires.
fn config_home_from(configured: Option<&std::ffi::OsStr>, home: &Path) -> PathBuf {
    configured
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
        .unwrap_or_else(|| home.join(".config"))
}

fn config_home(home: &Path) -> PathBuf {
    config_home_from(std::env::var_os("XDG_CONFIG_HOME").as_deref(), home)
}

/// Quote a path for a systemd `ExecStart=` command line: backslashes and
/// quotes are escaped, and a literal `%` is doubled so systemd does not read
/// it as the start of a specifier (`%h`, `%i`, …).
fn systemd_quote(path: &Path) -> String {
    let escaped = path
        .display()
        .to_string()
        .replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('%', "%%");
    format!("\"{escaped}\"")
}

fn systemd_unit(binary: &Path, port: u16, tunnel: bool) -> String {
    let tunnel = if tunnel { " --tunnel" } else { "" };
    format!(
        "[Unit]\nDescription=Nexus host daemon\nAfter=network-online.target\nWants=network-online.target\n\n[Service]\nExecStart={} host --port {} --no-sleep-guard{}\nRestart=on-failure\nRestartSec=5\n\n[Install]\nWantedBy=default.target\n",
        systemd_quote(binary),
        port,
        tunnel
    )
}

fn xml_escape(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

fn launchd_plist(binary: &Path, port: u16, tunnel: bool) -> String {
    let tunnel = if tunnel {
        "\n    <string>--tunnel</string>"
    } else {
        ""
    };
    format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n<plist version=\"1.0\">\n<dict>\n  <key>Label</key>\n  <string>chat.nexus.host</string>\n  <key>ProgramArguments</key>\n  <array>\n    <string>{}</string>\n    <string>host</string>\n    <string>--port</string>\n    <string>{port}</string>\n    <string>--no-sleep-guard</string>{tunnel}\n  </array>\n  <key>RunAtLoad</key>\n  <true/>\n  <key>KeepAlive</key>\n  <true/>\n</dict>\n</plist>\n",
        xml_escape(&binary.display().to_string())
    )
}

/// Whether `cloudflared` can be launched from `PATH`.
pub fn cloudflared_available() -> bool {
    std::process::Command::new("cloudflared")
        .arg("--version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|status| status.success())
}

fn parse_trycloudflare_url(line: &str) -> Option<String> {
    let start = line.find("https://")?;
    let candidate = &line[start..];
    let end = candidate
        .find(|character: char| {
            character.is_whitespace() || matches!(character, '"' | '\'' | ')' | ']')
        })
        .unwrap_or(candidate.len());
    let url = &candidate[..end];
    url.contains(".trycloudflare.com")
        .then(|| url.trim_end_matches('/').to_string())
}

/// Probe the public host. A `401` from `/v1/snapshot` is healthy: it proves
/// the tunnel reached the daemon and only host authentication stopped it.
pub async fn health_check(base: &str) -> bool {
    let url = format!("{}/v1/snapshot", base.trim_end_matches('/'));
    reqwest::Client::new()
        .get(url)
        .timeout(Duration::from_secs(3))
        .send()
        .await
        .is_ok_and(|response| response.status().is_success() || response.status().as_u16() == 401)
}

/// Battery warning used by the macOS host CLI.
pub fn on_battery() -> Option<bool> {
    #[cfg(target_os = "macos")]
    {
        let output = std::process::Command::new("pmset")
            .args(["-g", "batt"])
            .output()
            .ok()?;
        let text = String::from_utf8_lossy(&output.stdout);
        return Some(text.contains("Battery Power") && !text.contains("AC Power"));
    }
    #[cfg(not(target_os = "macos"))]
    {
        None
    }
}

/// A small helper for callers that need a consistent missing-sidecar error.
pub fn require_cloudflared() -> Result<()> {
    if cloudflared_available() {
        Ok(())
    } else {
        bail!("cloudflared is not in PATH — install it or run without --tunnel")
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::{ServiceManager, parse_trycloudflare_url, service_path};

    #[test]
    fn parses_quick_tunnel_url_from_cloudflared_log() {
        assert_eq!(
            parse_trycloudflare_url("INF | https://quiet-river.trycloudflare.com").as_deref(),
            Some("https://quiet-river.trycloudflare.com")
        );
        assert!(parse_trycloudflare_url("no public URL yet").is_none());
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn installs_service_definition_under_requested_home() {
        let home =
            std::env::temp_dir().join(format!("nexus-service-test-{}", uuid::Uuid::new_v4()));
        let path = super::install_service_at(&home, Path::new("/opt/nexus"), 8643, false)
            .expect("installs service definition");
        assert!(path.is_file());
        assert!(
            std::fs::read_to_string(&path)
                .expect("reads service definition")
                .contains("ExecStart=\"/opt/nexus\" host --port 8643")
        );
        std::fs::remove_dir_all(home).expect("cleans service test directory");
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn renders_hermetic_systemd_service() {
        let path = service_path(
            Path::new("/home/test/.config"),
            Path::new("/home/test"),
            ServiceManager::Systemd,
        );
        assert_eq!(
            path,
            Path::new("/home/test/.config/systemd/user/nexus-host.service")
        );
        let unit = super::systemd_unit(Path::new("/opt/Nexus Chat/nexus"), 8643, true);
        assert!(unit.contains("ExecStart=\"/opt/Nexus Chat/nexus\" host --port 8643"));
        assert!(unit.contains("--no-sleep-guard --tunnel"));
        // A literal % in the path is a systemd specifier prefix unless doubled.
        let unit = super::systemd_unit(Path::new("/opt/100%nexus/nexus"), 8643, false);
        assert!(
            unit.contains("ExecStart=\"/opt/100%%nexus/nexus\""),
            "unescaped specifier: {unit}"
        );
    }

    #[test]
    fn xdg_config_home_overrides_the_default_but_only_when_absolute() {
        let home = Path::new("/home/test");
        assert_eq!(
            super::config_home_from(Some(std::ffi::OsStr::new("/custom/cfg")), home),
            Path::new("/custom/cfg")
        );
        assert_eq!(
            super::config_home_from(Some(std::ffi::OsStr::new("relative")), home),
            Path::new("/home/test/.config")
        );
        assert_eq!(
            super::config_home_from(None, home),
            Path::new("/home/test/.config")
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn renders_hermetic_launchd_service() {
        let path = service_path(
            Path::new("/Users/test/.config"),
            Path::new("/Users/test"),
            ServiceManager::Launchd,
        );
        assert_eq!(
            path,
            Path::new("/Users/test/Library/LaunchAgents/chat.nexus.host.plist")
        );
        let plist = super::launchd_plist(Path::new("/opt/Nexus & Chat/nexus"), 8643, true);
        assert!(plist.contains("/opt/Nexus &amp; Chat/nexus"));
        assert!(plist.contains("<string>--tunnel</string>"));
    }
}
