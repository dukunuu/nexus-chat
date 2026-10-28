//! `nexus library [--kind deck|doc] [--json]`: the decks and docs of the active
//! space, newest first, listed the way the TUI's `/library` and studio's
//! Library list them (`nexus_core::canvas::library`).

use anyhow::Result;
use chrono::{DateTime, Utc};
use nexus_core::canvas::{self, Entry};
use nexus_core::deck::link::Kind;

/// What to list: everything, or one kind.
fn wanted(kind: Option<&str>) -> Result<Option<Kind>> {
    Ok(match kind {
        None => None,
        Some("deck") => Some(Kind::Deck),
        Some("doc") => Some(Kind::Doc),
        Some(other) => anyhow::bail!("--kind is deck or doc, not {other:?}"),
    })
}

/// The lines `nexus library` prints for `entries`: a JSON array, or one aligned
/// line per file (kind, path in the space, size, age, title).
pub fn lines(
    entries: &[Entry],
    kind: Option<&str>,
    json: bool,
    now: DateTime<Utc>,
) -> Result<Vec<String>> {
    let kind = wanted(kind)?;
    let shown: Vec<&Entry> = entries
        .iter()
        .filter(|entry| kind.is_none_or(|kind| entry.kind == kind))
        .collect();
    if json {
        return Ok(vec![serde_json::to_string_pretty(&shown)?]);
    }
    if shown.is_empty() {
        return Ok(vec!["no decks or docs yet".into()]);
    }
    let path_width = shown
        .iter()
        .map(|e| e.relative.chars().count())
        .max()
        .unwrap_or(0);
    Ok(shown
        .iter()
        .map(|entry| {
            format!(
                "{:<4}  {:<path_width$}  {:>10}  {:>14}  {}",
                match entry.kind {
                    Kind::Deck => "deck",
                    Kind::Doc => "doc",
                },
                entry.relative,
                entry.count_label(),
                canvas::relative_time(entry.modified, now),
                entry.title,
            )
        })
        .collect())
}

/// The command: list the active space's library on stdout.
pub fn run(kind: Option<&str>, json: bool, files: &std::path::Path) -> Result<()> {
    wanted(kind)?;
    for line in lines(&canvas::library(files), kind, json, Utc::now())? {
        crate::cli::out(line);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, SystemTime};

    use nexus_core::deck::Deck;
    use nexus_core::doc::Doc;

    use super::*;

    struct Space(std::path::PathBuf);
    impl Space {
        fn new() -> Self {
            let path =
                std::env::temp_dir().join(format!("nexus-cli-library-{}", uuid::Uuid::new_v4()));
            std::fs::create_dir_all(&path).unwrap();
            Self(path.canonicalize().unwrap())
        }
        fn put(&self, relative: &str, bytes: &[u8], age: u64) {
            let path = self.0.join(relative);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(&path, bytes).unwrap();
            std::fs::File::options()
                .write(true)
                .open(&path)
                .unwrap()
                .set_modified(SystemTime::now() - Duration::from_secs(age))
                .unwrap();
        }
    }
    impl Drop for Space {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn filled() -> Space {
        let space = Space::new();
        space.put(
            "decks/launch.deck.json",
            &serde_json::to_vec(&Deck::titled("Launch review")).unwrap(),
            3 * 3600 + 120,
        );
        let mut doc = Doc::from_design("Q3 memo", &nexus_core::design::Design::default());
        doc.blocks = nexus_core::doc::markdown::parse_document("# T\n\nBody.\n", true).blocks;
        let doc = Doc::check(&serde_json::to_string(&doc).unwrap()).unwrap();
        space.put("docs/memo.doc.json", &serde_json::to_vec(&doc).unwrap(), 90);
        space.put(
            "decks/talks/old.deck.json",
            &serde_json::to_vec(&Deck::titled("Old talk")).unwrap(),
            9 * 86_400,
        );
        space
    }

    #[test]
    fn text_lists_one_aligned_line_per_file_newest_first() {
        let space = filled();
        let entries = canvas::library(&space.0);
        let text = lines(&entries, None, false, Utc::now()).unwrap();
        assert_eq!(
            text,
            [
                "doc   docs/memo.doc.json           2 blocks    1 minute ago  Q3 memo",
                "deck  decks/launch.deck.json        1 slide     3 hours ago  Launch review",
                "deck  decks/talks/old.deck.json     1 slide      9 days ago  Old talk",
            ]
        );
    }

    #[test]
    fn kind_filters_and_rejects_anything_else() {
        let space = filled();
        let entries = canvas::library(&space.0);
        let decks = lines(&entries, Some("deck"), false, Utc::now()).unwrap();
        assert_eq!(decks.len(), 2);
        assert!(decks.iter().all(|line| line.starts_with("deck")));
        let docs = lines(&entries, Some("doc"), false, Utc::now()).unwrap();
        assert_eq!(docs.len(), 1);
        assert!(lines(&entries, Some("slides"), false, Utc::now()).is_err());
        assert!(run(Some("slides"), false, &space.0).is_err());
    }

    #[test]
    fn json_is_an_array_of_the_same_entries() {
        let space = filled();
        let entries = canvas::library(&space.0);
        let json = lines(&entries, None, true, Utc::now()).unwrap();
        assert_eq!(json.len(), 1);
        let value: serde_json::Value = serde_json::from_str(&json[0]).unwrap();
        let items = value.as_array().unwrap();
        assert_eq!(items.len(), 3);
        assert_eq!(items[0]["kind"], "doc");
        assert_eq!(items[0]["relative"], "docs/memo.doc.json");
        assert_eq!(items[0]["title"], "Q3 memo");
        assert_eq!(items[0]["count"], 2);
        assert!(items[0]["modified"].as_str().unwrap().contains('T'));
        assert!(
            items[0]["path"]
                .as_str()
                .unwrap()
                .ends_with("docs/memo.doc.json")
        );
        assert!(items[0]["revision"].as_str().unwrap().len() == 64);
        assert_eq!(items[2]["relative"], "decks/talks/old.deck.json");
        let decks = lines(&entries, Some("deck"), true, Utc::now()).unwrap();
        let decks: serde_json::Value = serde_json::from_str(&decks[0]).unwrap();
        assert_eq!(decks.as_array().unwrap().len(), 2);
    }

    #[test]
    fn an_empty_space_prints_a_sentence_or_an_empty_array() {
        let space = Space::new();
        let entries = canvas::library(&space.0);
        assert_eq!(
            lines(&entries, None, false, Utc::now()).unwrap(),
            ["no decks or docs yet"]
        );
        assert_eq!(lines(&entries, None, true, Utc::now()).unwrap(), ["[]"]);
    }
}
