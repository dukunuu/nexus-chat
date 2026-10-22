//! Turn a `tools/call` result into the text the model reads.
//!
//! Server output is untrusted data: it is rendered as plain text, never
//! interpreted. Images are saved under the space's files directory in a
//! `connectors/` folder and referenced as `![…](…)` so the existing vision
//! injection (which reads `![](file)` references out of tool results) picks
//! them up; audio becomes a note; links and embedded resources are listed.

use std::fmt::Write as _;
use std::path::Path;

use base64::Engine as _;
use sha2::{Digest as _, Sha256};

use super::protocol::{CallResult, Content};

/// Largest image (decoded) kept from a result.
const MAX_IMAGE_BYTES: usize = 10 * 1024 * 1024;

/// The rendered result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Rendered {
    pub text: String,
    pub is_error: bool,
}

fn extension(mime: &str) -> Option<&'static str> {
    match mime.split(';').next().unwrap_or_default().trim() {
        "image/png" => Some("png"),
        "image/jpeg" | "image/jpg" => Some("jpg"),
        "image/gif" => Some("gif"),
        "image/webp" => Some("webp"),
        _ => None,
    }
}

fn kilobytes(bytes: usize) -> String {
    format!("{} KB", bytes.div_ceil(1024))
}

/// Save a base64 image and return its markdown reference (or a note why not).
fn save_image(connector: &str, data: &str, mime: &str, files_dir: &Path) -> String {
    let Some(ext) = extension(mime) else {
        return format!("[image: {mime} is not a format nexus can show]");
    };
    let cleaned: String = data.chars().filter(|c| !c.is_whitespace()).collect();
    let Ok(bytes) = base64::engine::general_purpose::STANDARD.decode(cleaned) else {
        return "[image: the data was not valid base64]".to_string();
    };
    if bytes.len() > MAX_IMAGE_BYTES {
        return format!("[image: {} is too large to keep]", kilobytes(bytes.len()));
    }
    let digest = Sha256::digest(&bytes);
    let short = digest.iter().take(6).fold(String::new(), |mut hex, byte| {
        let _ = write!(hex, "{byte:02x}");
        hex
    });
    let file = format!("{connector}-{short}.{ext}");
    let dir = files_dir.join("connectors");
    if std::fs::create_dir_all(&dir).is_err() || std::fs::write(dir.join(&file), &bytes).is_err() {
        return "[image: could not be saved]".to_string();
    }
    format!("![image from {connector}](connectors/{file})")
}

/// Render a result for the model.
#[must_use]
pub fn render(connector: &str, tool: &str, result: &CallResult, files_dir: &Path) -> Rendered {
    let mut parts: Vec<String> = Vec::new();
    for block in &result.content {
        parts.push(match block {
            Content::Text(text) => text.clone(),
            Content::Image { data, mime } => save_image(connector, data, mime, files_dir),
            Content::Audio { mime, bytes } => {
                format!("[audio: {mime}, {} · not playable here]", kilobytes(*bytes))
            }
            Content::ResourceLink {
                uri,
                name,
                description,
            } => {
                let label = if name.is_empty() { uri } else { name };
                if description.is_empty() {
                    format!("resource: {label} <{uri}>")
                } else {
                    format!("resource: {label} <{uri}> · {description}")
                }
            }
            Content::Resource {
                uri,
                mime,
                text: Some(text),
                ..
            } => {
                let header = if mime.is_empty() {
                    format!("[resource {uri}]")
                } else {
                    format!("[resource {uri} · {mime}]")
                };
                format!("{header}\n{text}")
            }
            Content::Resource {
                uri,
                mime,
                blob: Some(blob),
                ..
            } => {
                if extension(mime).is_some() {
                    save_image(connector, blob, mime, files_dir)
                } else {
                    format!(
                        "[resource {uri} · {} · {} binary, not shown]",
                        if mime.is_empty() {
                            "unknown type"
                        } else {
                            mime
                        },
                        kilobytes(blob.len() / 4 * 3)
                    )
                }
            }
            Content::Resource { uri, .. } => format!("[resource {uri}]"),
            Content::Unknown(kind) => format!("[{kind} content not shown]"),
        });
    }
    if let Some(structured) = &result.structured {
        parts.retain(|part| !part.trim().is_empty());
        parts.push(structured.to_string());
    }
    let body = parts.join("\n");
    if result.is_error {
        let detail = if body.trim().is_empty() {
            "the tool reported an error without details".to_string()
        } else {
            body
        };
        return Rendered {
            text: format!(
                "{connector} {tool} error: {detail}\nthe server reported that the tool failed · check the arguments against its schema, or tell the user what failed instead of repeating the same call"
            ),
            is_error: true,
        };
    }
    Rendered {
        text: if body.trim().is_empty() {
            "(the tool returned no content)".to_string()
        } else {
            body
        },
        is_error: false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn dir() -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("nexus-conn-results-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn every_content_type_renders_and_images_land_in_files() {
        let dir = dir();
        let png = base64::engine::general_purpose::STANDARD.encode(b"\x89PNG fake");
        let result = CallResult::parse(&json!({
            "content": [
                {"type": "text", "text": "hello"},
                {"type": "image", "data": png, "mimeType": "image/png"},
                {"type": "audio", "data": "aGVsbG8gd29ybGQ=", "mimeType": "audio/mpeg"},
                {"type": "resource_link", "uri": "file:///a", "name": "a.txt", "description": "notes"},
                {"type": "resource", "resource": {"uri": "file:///b", "mimeType": "text/plain", "text": "body"}},
                {"type": "resource", "resource": {"uri": "file:///c", "mimeType": "application/zip", "blob": "AAAA"}},
                {"type": "image", "data": "!!!", "mimeType": "image/png"},
                {"type": "image", "data": "AAAA", "mimeType": "image/tiff"}
            ]
        }));
        let rendered = render("demo", "make", &result, &dir);
        assert!(!rendered.is_error);
        let text = &rendered.text;
        assert!(
            text.starts_with("hello\n![image from demo](connectors/demo-"),
            "{text}"
        );
        assert!(text.contains("[audio: audio/mpeg"), "{text}");
        assert!(
            text.contains("resource: a.txt <file:///a> · notes"),
            "{text}"
        );
        assert!(
            text.contains("[resource file:///b · text/plain]\nbody"),
            "{text}"
        );
        assert!(text.contains("application/zip"), "{text}");
        assert!(text.contains("not valid base64"), "{text}");
        assert!(text.contains("image/tiff"), "{text}");
        let saved: Vec<_> = std::fs::read_dir(dir.join("connectors")).unwrap().collect();
        assert_eq!(saved.len(), 1);
        // The saved reference is exactly what the vision injection resolves.
        let reference = text
            .lines()
            .find_map(|line| line.strip_prefix("![image from demo](")?.strip_suffix(')'))
            .unwrap();
        assert!(dir.join(reference).is_file());
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn structured_content_fills_in_when_there_is_no_text() {
        let dir = dir();
        let result = CallResult::parse(&json!({"content": [], "structuredContent": {"n": 1}}));
        assert_eq!(render("d", "t", &result, &dir).text, "{\"n\":1}");
        let empty = CallResult::parse(&json!({}));
        assert_eq!(
            render("d", "t", &empty, &dir).text,
            "(the tool returned no content)"
        );
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn structured_content_is_kept_alongside_text() {
        let dir = dir();
        let result = CallResult::parse(&json!({
            "content": [{"type": "text", "text": "three rows"}],
            "structuredContent": {"rows": [1, 2, 3]}
        }));
        assert_eq!(
            render("d", "t", &result, &dir).text,
            "three rows\n{\"rows\":[1,2,3]}"
        );
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn tool_errors_are_recovery_oriented_and_first_line_marks_failure() {
        let dir = dir();
        let result = CallResult::parse(&json!({
            "content": [{"type": "text", "text": "no such issue"}], "isError": true
        }));
        let rendered = render("linear", "get_issue", &result, &dir);
        assert!(rendered.is_error);
        let first = rendered.text.lines().next().unwrap();
        assert_eq!(first, "linear get_issue error: no such issue");
        assert!(rendered.text.contains("instead of repeating the same call"));
        // The tool card classifies this as a failure from the first line alone.
        assert!(
            crate::app::tool_call_view("linear__get_issue", "{}", &rendered.text).outcome
                == crate::app::ToolOutcome::Failed
        );
        std::fs::remove_dir_all(dir).unwrap();
    }
}
