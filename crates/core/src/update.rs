//! Startup update check + auto-update: compare the running version against
//! the latest release on crates.io and, when a newer one exists, install it
//! via `cargo install` in a detached background process so the TUI boots
//! immediately and the new binary is live on the next launch. Best-effort
//! by design — any failure (offline, index hiccup, missing cargo) is
//! silent or falls back to a plain notice; the app never blocks or fails
//! startup on it. `NEXUS_NO_UPDATE=1` opts out of the auto-install.

use std::io::Write as _;
use std::path::{Path, PathBuf};

/// The version this binary was built from (Cargo.toml at compile time).
pub const CURRENT: &str = env!("CARGO_PKG_VERSION");

/// How long an in-flight auto-update marker stays valid. A cold `cargo
/// install` of this crate's dependency tree takes minutes; anything older
/// than this is assumed finished (or dead) and may be retried.
const MARKER_STALE_AFTER: std::time::Duration = std::time::Duration::from_mins(30);

/// The crates.io sparse index doc for `nexus-chat` — one small NDJSON line
/// per published version, newest last, no auth, no API quota.
const SPARSE_INDEX: &str = "https://index.crates.io/ne/xu/nexus-chat";

/// Fetch the newest non-yanked published version, or `None` on any failure
/// (offline, timeout, malformed index). Runs in a background task — the UI
/// is never blocked on this.
pub async fn latest_version() -> Option<String> {
    let body = reqwest::Client::new()
        .get(SPARSE_INDEX)
        .timeout(std::time::Duration::from_secs(4))
        .send()
        .await
        .ok()?
        .text()
        .await
        .ok()?;
    body.lines()
        .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
        .filter(|v| {
            !v.get("yanked")
                .and_then(serde_json::Value::as_bool)
                .unwrap_or(false)
        })
        .filter_map(|v| {
            v.get("vers")
                .and_then(serde_json::Value::as_str)
                .map(str::to_string)
        })
        .next_back()
}

/// `a > b` for the dotted versions crates.io publishes (`0.1.1`, `1.2.3`,
/// `0.1.2-alpha.1`). Numeric components compare numerically (`0.1.10` >
/// `0.1.9` — string order would get that wrong), missing components count
/// as 0, and pre-release extras compare alphabetically after the numbers.
pub fn version_gt(a: &str, b: &str) -> bool {
    compare(a, b).is_gt()
}

fn compare(a: &str, b: &str) -> std::cmp::Ordering {
    let core_a = a.split('-').next().unwrap_or(a);
    let core_b = b.split('-').next().unwrap_or(b);
    let core = compare_core(core_a, core_b);
    if core != std::cmp::Ordering::Equal {
        return core;
    }
    // Same numeric core: a release (no pre-release suffix) beats any
    // pre-release of it ("0.1.2" > "0.1.2-alpha.1").
    let pre_a = a.strip_prefix(core_a).and_then(|s| s.strip_prefix('-'));
    let pre_b = b.strip_prefix(core_b).and_then(|s| s.strip_prefix('-'));
    match (pre_a, pre_b) {
        (None, None) => std::cmp::Ordering::Equal,
        (None, Some(_)) => std::cmp::Ordering::Greater,
        (Some(_), None) => std::cmp::Ordering::Less,
        (Some(x), Some(y)) => compare_pre(x, y),
    }
}

/// Dotted numeric core: missing components count as 0.
fn compare_core(a: &str, b: &str) -> std::cmp::Ordering {
    let pa: Vec<&str> = a.split('.').collect();
    let pb: Vec<&str> = b.split('.').collect();
    for i in 0..pa.len().max(pb.len()) {
        let x = pa.get(i).copied().unwrap_or("0");
        let y = pb.get(i).copied().unwrap_or("0");
        let ord = match (x.parse::<u64>(), y.parse::<u64>()) {
            (Ok(xn), Ok(yn)) => xn.cmp(&yn),
            _ => x.cmp(y),
        };
        if ord != std::cmp::Ordering::Equal {
            return ord;
        }
    }
    std::cmp::Ordering::Equal
}

/// Pre-release identifiers ("alpha.1", "beta"): numeric when both are,
/// otherwise byte order — close enough to semver for update notices.
fn compare_pre(a: &str, b: &str) -> std::cmp::Ordering {
    let pa: Vec<&str> = a.split('.').collect();
    let pb: Vec<&str> = b.split('.').collect();
    for i in 0..pa.len().max(pb.len()) {
        let x = pa.get(i).copied().unwrap_or("0");
        let y = pb.get(i).copied().unwrap_or("0");
        let ord = match (x.parse::<u64>(), y.parse::<u64>()) {
            (Ok(xn), Ok(yn)) => xn.cmp(&yn),
            _ => x.cmp(y),
        };
        if ord != std::cmp::Ordering::Equal {
            return ord;
        }
    }
    std::cmp::Ordering::Equal
}

// --- auto-update via `cargo install` ---

/// Marker file recording an in-flight auto-update (target version). Written
/// before `cargo install` is spawned and left in place — a fresh marker
/// blocks a second install; a stale one is reclaimed by the next launch.
fn marker_path(data_dir: &Path) -> PathBuf {
    data_dir.join("auto-update.marker")
}

/// Where the detached installer's output goes — `cargo install` runs
/// outside the TUI, so its progress is only visible here.
fn log_path(data_dir: &Path) -> PathBuf {
    data_dir.join("auto-update.log")
}

/// Is `path` a binary built by `cargo run`/`cargo build` (inside a target
/// dir)? Auto-update skips those — a dev build must not silently replace
/// itself with the registry release.
fn path_is_dev_build(path: &Path) -> bool {
    let s = path.to_string_lossy();
    s.contains("/target/debug/") || s.contains("/target/release/")
}

/// Is the running binary a dev build? Determined from the executable's own
/// path, so `cargo run` and `cargo build` artifacts never self-update.
fn is_dev_build() -> bool {
    std::env::current_exe().is_ok_and(|p| path_is_dev_build(&p))
}

/// Is `cargo` on PATH? Auto-update is only possible when it is.
fn cargo_available() -> bool {
    std::process::Command::new("cargo")
        .arg("--version")
        .output()
        .is_ok()
}

/// A marker written less than [`MARKER_STALE_AFTER`] ago means an install
/// is (or was, minutes ago) running; a missing or stale marker means the
/// slot is free.
fn marker_in_flight(marker: &Path) -> bool {
    let Ok(meta) = std::fs::metadata(marker) else {
        return false;
    };
    let Ok(modified) = meta.modified() else {
        return false;
    };
    marker_is_fresh(modified, std::time::SystemTime::now())
}

/// Freshness window for an update marker: `modified` within
/// [`MARKER_STALE_AFTER`] of `now`. Future timestamps (clock skew — the
/// marker was just written) count as fresh.
fn marker_is_fresh(modified: std::time::SystemTime, now: std::time::SystemTime) -> bool {
    match now.duration_since(modified) {
        Ok(age) => age < MARKER_STALE_AFTER,
        Err(_) => true,
    }
}

/// What [`try_start_auto_update`] decided.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AutoUpdateOutcome {
    /// `cargo install` was spawned in the background; a restart applies it.
    Started,
    /// A previous launch is already installing — don't start a second one.
    InFlight,
    /// Auto-update isn't possible here (dev build, no cargo, opted out);
    /// the caller should fall back to telling the user the manual command.
    Unavailable,
}

/// Start an automatic update to `latest` when the environment allows it:
/// the running binary must be a real install (not a dev build), `cargo`
/// must be on PATH, `NEXUS_NO_UPDATE` must be unset, and no other
/// auto-update may be in flight (marker guard). Spawns
/// `cargo install --force nexus-chat` fully detached — the caller returns
/// immediately and the install runs to completion (or failure, logged to
/// `auto-update.log` next to the marker) on its own, outliving this
/// process. The new binary is live on the next launch; the running one is
/// untouched (Unix keeps the old inode alive).
pub fn try_start_auto_update(data_dir: &Path, latest: &str) -> AutoUpdateOutcome {
    if std::env::var_os("NEXUS_NO_UPDATE").is_some() || is_dev_build() || !cargo_available() {
        return AutoUpdateOutcome::Unavailable;
    }
    let marker = marker_path(data_dir);
    if marker_in_flight(&marker) {
        return AutoUpdateOutcome::InFlight;
    }
    // Reclaim a stale marker from a dead install, then claim the slot
    // before spawning so a concurrent launch can't double-install.
    let _ = std::fs::remove_file(&marker);
    if std::fs::write(&marker, format!("{latest}\n")).is_err() {
        return AutoUpdateOutcome::Unavailable;
    }
    let mut cmd = std::process::Command::new("cargo");
    cmd.args(["install", "--force", "nexus-chat"]);
    // Detached: progress goes to the log, never to the TUI's terminal.
    if let Ok(log) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(log_path(data_dir))
    {
        let _ = writeln!(
            &log,
            "--- auto-update to v{latest} started at {} ---",
            chrono::Local::now().format("%Y-%m-%d %H:%M:%S")
        );
        if let Ok(out) = log.try_clone() {
            cmd.stdout(out);
            cmd.stderr(log);
        } else {
            cmd.stdout(std::process::Stdio::null());
            cmd.stderr(std::process::Stdio::null());
        }
    } else {
        cmd.stdout(std::process::Stdio::null());
        cmd.stderr(std::process::Stdio::null());
    }
    if cmd.spawn().is_err() {
        // Spawn failed (cargo vanished between check and spawn): free the
        // slot so the next launch can retry.
        let _ = std::fs::remove_file(&marker);
        return AutoUpdateOutcome::Unavailable;
    }
    AutoUpdateOutcome::Started
}

/// Run `cargo install --force nexus-chat` in the foreground, streaming its
/// output to the terminal — the deliberate `nexus update` path. Returns
/// the exit status; the caller decides how to report it.
pub fn install_now() -> anyhow::Result<std::process::ExitStatus> {
    let status = std::process::Command::new("cargo")
        .args(["install", "--force", "nexus-chat"])
        .status()
        .map_err(|e| {
            anyhow::anyhow!("running `cargo install nexus-chat`: {e} (is cargo on PATH?)")
        })?;
    Ok(status)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn numeric_components_beat_string_order() {
        assert!(version_gt("0.1.10", "0.1.9"));
        assert!(version_gt("1.0.0", "0.9.9"));
        assert!(version_gt("0.2.0", "0.1.99"));
    }

    #[test]
    fn missing_components_count_as_zero() {
        assert!(version_gt("0.2", "0.1.9"));
        assert!(!version_gt("0.1", "0.1.0"));
        assert!(version_gt("0.1.1", "0.1"));
    }

    #[test]
    fn equal_versions_are_not_greater() {
        assert!(!version_gt("0.1.1", "0.1.1"));
        assert!(version_gt("0.1.2", "0.1.1"));
    }

    #[test]
    fn prerelease_extras_compare() {
        assert!(version_gt("0.1.2", "0.1.2-alpha.1"));
        assert!(version_gt("0.1.2-beta", "0.1.2-alpha"));
    }

    #[test]
    fn dev_build_detection() {
        assert!(path_is_dev_build(Path::new(
            "/home/u/nexus-chat/target/debug/nexus"
        )));
        assert!(path_is_dev_build(Path::new(
            "/home/u/nexus-chat/target/release/nexus"
        )));
        assert!(!path_is_dev_build(Path::new("/home/u/.cargo/bin/nexus")));
        assert!(!path_is_dev_build(Path::new("/usr/local/bin/nexus")));
    }

    #[test]
    fn marker_freshness_window() {
        // A base time comfortably after the epoch so both directions of the
        // window are representable (SystemTime can't go below the epoch).
        let now = std::time::SystemTime::UNIX_EPOCH + std::time::Duration::from_hours(1);
        let fresh = now - std::time::Duration::from_mins(5);
        assert!(marker_is_fresh(fresh, now));
        let stale = now - std::time::Duration::from_mins(31);
        assert!(!marker_is_fresh(stale, now));
        // Clock skew: a future mtime counts as fresh (just written).
        assert!(marker_is_fresh(
            now + std::time::Duration::from_mins(1),
            now
        ));
    }

    #[test]
    fn stale_marker_is_reclaimed_and_fresh_one_blocks() {
        let dir = test_dir();
        let marker = marker_path(&dir);
        // Fresh marker → in flight.
        std::fs::write(&marker, "0.9.9\n").unwrap();
        assert!(marker_in_flight(&marker));
        // Age it past the window → not in flight (the next launch reclaims it).
        let file = std::fs::OpenOptions::new()
            .write(true)
            .open(&marker)
            .unwrap();
        file.set_modified(std::time::SystemTime::now() - std::time::Duration::from_mins(31))
            .unwrap();
        drop(file);
        assert!(!marker_in_flight(&marker));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A throwaway temp dir unique per test run (tests run in parallel).
    fn test_dir() -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "nexus-update-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::SystemTime::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn index_parse_takes_last_nonyanked() {
        let body = "{\"name\":\"nexus-chat\",\"vers\":\"0.1.0\",\"yanked\":false}\n\
            {\"name\":\"nexus-chat\",\"vers\":\"0.1.1\",\"yanked\":true}\n\
            {\"name\":\"nexus-chat\",\"vers\":\"0.1.2\",\"yanked\":false}\n";
        let lines = body
            .lines()
            .filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok())
            .filter(|v| {
                !v.get("yanked")
                    .and_then(serde_json::Value::as_bool)
                    .unwrap_or(false)
            })
            .filter_map(|v| {
                v.get("vers")
                    .and_then(serde_json::Value::as_str)
                    .map(str::to_string)
            })
            .next_back();
        assert_eq!(lines.as_deref(), Some("0.1.2"));
    }
}
