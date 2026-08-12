//! Startup update check: compare the running version against the latest
//! release on crates.io and let the UI surface an "update available" notice.
//! Best-effort by design — any failure (offline, index hiccup) is silent;
//! the app must never block or fail startup on it.

/// The version this binary was built from (Cargo.toml at compile time).
pub const CURRENT: &str = env!("CARGO_PKG_VERSION");

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
