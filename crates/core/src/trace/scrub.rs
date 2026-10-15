//! Secret scrubbing for device-local traces, without regular expressions.

/// Replaces configured secrets and recognizable credential formats.
pub struct Scrubber {
    secrets: Vec<String>,
}

impl Scrubber {
    /// Keep only non-empty secrets of at least eight characters, longest first.
    pub fn new(mut secrets: Vec<String>) -> Self {
        secrets.retain(|secret| secret.chars().count() >= 8);
        secrets.sort_by_key(|secret| std::cmp::Reverse(secret.len()));
        secrets.dedup();
        Self { secrets }
    }

    /// Scrub secrets while retaining surrounding text and bearer prefixes.
    pub fn scrub(&self, text: &str) -> String {
        let mut result = String::with_capacity(text.len());
        let mut offset = 0;
        let mut copied = 0;
        while offset < text.len() {
            let rest = &text[offset..];
            let exact = self
                .secrets
                .iter()
                .find(|secret| rest.starts_with(secret.as_str()))
                .map(|secret| (0, secret.len()));
            let at_boundary = offset == 0 || !key_char(text.as_bytes()[offset - 1]);
            // Match whole credentials before redacting secrets inside their framing.
            let matched = pattern(rest, at_boundary)
                .into_iter()
                .chain(exact)
                .max_by_key(|&(_, end)| end);
            if let Some((keep, end)) = matched {
                result.push_str(&text[copied..offset + keep]);
                result.push_str("[REDACTED]");
                offset += end;
                copied = offset;
            } else {
                // Advance on UTF-8 boundaries, including in ordinary prose.
                offset += rest.chars().next().map_or(1, char::len_utf8);
            }
        }
        result.push_str(&text[copied..]);
        result
    }
}

fn key_char(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-')
}

fn pattern(text: &str, at_boundary: bool) -> Option<(usize, usize)> {
    if !at_boundary {
        return None;
    }
    if let Some(end) = private_key_end(text) {
        return Some((0, end));
    }
    for prefix in ["sk-", "ghp_", "gho_", "ghs_", "github_pat_"] {
        if let Some(rest) = text.strip_prefix(prefix) {
            let length = rest.bytes().take_while(|&byte| key_char(byte)).count();
            if length >= 20 && rest[..length].bytes().any(|byte| byte.is_ascii_digit()) {
                return Some((0, prefix.len() + length));
            }
        }
    }
    if let Some(rest) = text.strip_prefix("AKIA") {
        let length = rest
            .bytes()
            .take_while(|byte| byte.is_ascii_uppercase() || byte.is_ascii_digit())
            .count();
        if length == 16 {
            return Some((0, 20));
        }
    }
    if text
        .get(..7)
        .is_some_and(|prefix| prefix.eq_ignore_ascii_case("Bearer "))
    {
        let length = text[7..]
            .bytes()
            .take_while(|byte| {
                byte.is_ascii_alphanumeric()
                    || matches!(byte, b'.' | b'_' | b'~' | b'+' | b'/' | b'=' | b'-')
            })
            .count();
        if length >= 16 {
            return Some((7, 7 + length));
        }
    }
    None
}

fn private_key_end(text: &str) -> Option<usize> {
    let header = text.strip_prefix("-----BEGIN ")?;
    let label = &header[..header.find("-----")?];
    if !label.ends_with("PRIVATE KEY") || label.contains(['\r', '\n']) {
        return None;
    }
    let body_start = "-----BEGIN ".len() + label.len() + 5;
    let end_marker = format!("-----END {label}-----");
    let end = text[body_start..].find(&end_marker)?;
    Some(body_start + end + end_marker.len())
}

#[cfg(test)]
mod tests {
    use super::Scrubber;

    #[test]
    fn patterns_preserve_ordinary_words_and_slugs() {
        let scrubber = Scrubber::new(Vec::new());
        for text in [
            "risk-assessment-for-the-quarterly-plan",
            "task-management-system-overview",
            "task-management-system-overview-2026",
            "sk-project-management-system-overview",
            "ghp_abcdefghijklmnopqrstuv",
            "gho_abcdefghijklmnopqrstuv",
            "ghs_abcdefghijklmnopqrstuv",
            "github_pat_abcdefghijklmnopqrstuv",
        ] {
            assert_eq!(scrubber.scrub(text), text);
        }
    }

    #[test]
    fn patterns_require_token_boundaries() {
        let scrubber = Scrubber::new(Vec::new());
        for (token, redacted) in [
            ("sk-proj-Abc123def456ghi789jkl012", "[REDACTED]"),
            ("ghp_0123456789abcdefghij", "[REDACTED]"),
            ("gho_0123456789abcdefghij", "[REDACTED]"),
            ("ghs_0123456789abcdefghij", "[REDACTED]"),
            ("github_pat_0123456789abcdefghij", "[REDACTED]"),
            ("AKIA0123456789ABCDEF", "[REDACTED]"),
            ("Bearer 0123456789abcdef", "Bearer [REDACTED]"),
            (
                "-----BEGIN PRIVATE KEY-----\nprivate-body\n-----END PRIVATE KEY-----",
                "[REDACTED]",
            ),
        ] {
            for prefix in ["", "=", "(", "é"] {
                assert_eq!(
                    scrubber.scrub(&format!("{prefix}{token}")),
                    format!("{prefix}{redacted}")
                );
            }
            for prefix in ["x", "0", "_", "-"] {
                let text = format!("{prefix}{token}");
                assert_eq!(scrubber.scrub(&text), text);
            }
        }
        assert_eq!(
            scrubber.scrub("token=sk-proj-Abc123def456ghi789jkl012"),
            "token=[REDACTED]"
        );
        assert_eq!(
            scrubber.scrub("(sk-or-v1-0123456789abcdef0123)"),
            "([REDACTED])"
        );
    }

    #[test]
    fn exact_secrets_match_inside_words_without_digits() {
        let secret = "sk-configured-secret-without-digits";
        let scrubber = Scrubber::new(vec![secret.into()]);
        assert_eq!(scrubber.scrub(&format!("xx{secret}yy")), "xx[REDACTED]yy");
    }
}
