//! Host-side child-process management: `cloudflared` quick tunnels and
//! platform sleep inhibitors. These helpers are never started by unit tests;
//! the CLI opts into them only for `nexus host --tunnel` or normal hosting.

use std::process::Stdio;
use std::time::Duration;

use anyhow::{Context as _, Result, bail};
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::{Child, Command};

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
            .spawn()
            .context("starting systemd-inhibit")?;
        return Ok(Some(ChildGuard { child: Some(child) }));
    }
    #[allow(unreachable_code)]
    Ok(None)
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
    use super::parse_trycloudflare_url;

    #[test]
    fn parses_quick_tunnel_url_from_cloudflared_log() {
        assert_eq!(
            parse_trycloudflare_url("INF | https://quiet-river.trycloudflare.com").as_deref(),
            Some("https://quiet-river.trycloudflare.com")
        );
        assert!(parse_trycloudflare_url("no public URL yet").is_none());
    }
}
