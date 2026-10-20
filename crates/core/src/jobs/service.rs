//! Per-user jobs daemon service files, independent of the host lifecycle.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};

pub(crate) use super::tree::{Tree, git};

fn path(home: &Path, config: &Path) -> Result<PathBuf> {
    if cfg!(target_os = "macos") {
        Ok(home.join("Library/LaunchAgents/chat.nexus.jobs.plist"))
    } else if cfg!(target_os = "linux") {
        Ok(config.join("systemd/user/nexus-jobs.service"))
    } else {
        bail!("jobs services are supported on macOS and Linux")
    }
}

fn contents(binary: &Path) -> String {
    let binary = binary.display().to_string();
    if cfg!(target_os = "macos") {
        let escaped = binary
            .replace('&', "&amp;")
            .replace('<', "&lt;")
            .replace('>', "&gt;")
            .replace('"', "&quot;")
            .replace('\'', "&apos;");
        format!(
            "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<plist version=\"1.0\"><dict>\n<key>Label</key><string>chat.nexus.jobs</string>\n<key>ProgramArguments</key><array><string>{escaped}</string><string>jobs</string><string>daemon</string></array>\n<key>RunAtLoad</key><true/>\n<key>KeepAlive</key><true/>\n</dict></plist>\n"
        )
    } else {
        let escaped = binary
            .replace('\\', "\\\\")
            .replace('"', "\\\"")
            .replace('%', "%%");
        format!(
            "[Unit]\nDescription=Nexus jobs daemon\nAfter=network-online.target\nWants=network-online.target\n\n[Service]\nExecStart=\"{escaped}\" jobs daemon\nRestart=on-failure\nRestartSec=5\n\n[Install]\nWantedBy=default.target\n"
        )
    }
}

/// Write a service file with explicit directories (no XDG access in tests).
pub fn install_service_at(home: &Path, config: &Path, binary: &Path) -> Result<PathBuf> {
    let path = path(home, config)?;
    std::fs::create_dir_all(path.parent().context("service parent")?)?;
    std::fs::write(&path, contents(binary))?;
    Ok(path)
}

fn directories() -> Result<(PathBuf, PathBuf)> {
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .context("HOME is not set")?;
    let config = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
        .unwrap_or_else(|| home.join(".config"));
    Ok((home, config))
}

/// Write (but do not load) the user's daemon service definition.
pub fn install_service() -> Result<PathBuf> {
    let (home, config) = directories()?;
    install_service_at(&home, &config, &std::env::current_exe()?)
}

/// Remove the service definition; an already loaded service must be stopped separately.
pub fn uninstall_service() -> Result<bool> {
    let (home, config) = directories()?;
    let path = path(&home, &config)?;
    match std::fs::remove_file(path) {
        Ok(()) => Ok(true),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error.into()),
    }
}
