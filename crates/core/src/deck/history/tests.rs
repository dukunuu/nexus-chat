use std::path::PathBuf;

use serde_json::json;

use super::*;

struct Folder(PathBuf);

impl Folder {
    fn new() -> Self {
        let dir = std::env::temp_dir().join(format!("nexus-history-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        Self(dir)
    }
    fn deck(&self) -> PathBuf {
        self.0.join("talk.deck.json")
    }
}

impl Drop for Folder {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn deck(texts: &[&str]) -> Deck {
    let slides: Vec<_> = texts
        .iter()
        .enumerate()
        .map(|(i, t)| json!({"id": format!("s{i}"), "children": [{"id": format!("n{i}"), "kind": "text", "text": t}]}))
        .collect();
    Deck::check(&json!({"version": 2, "title": "Synthetic", "slides": slides}).to_string()).unwrap()
}

/// What the tool and studio do: keep the old version, then write the new one.
fn write(folder: &Folder, next: &Deck, who: Who) {
    record_before_write(&folder.deck(), &next.revision(), who).unwrap();
    write_atomic(&folder.deck(), &serde_json::to_vec_pretty(next).unwrap()).unwrap();
}

#[test]
fn the_first_write_keeps_nothing_and_later_ones_keep_the_replaced_version() {
    let f = Folder::new();
    let (a, b, c) = (deck(&["a"]), deck(&["b"]), deck(&["c"]));
    write(&f, &a, Who::Model);
    assert!(list(&f.deck()).unwrap().is_empty());
    write(&f, &b, Who::Model);
    write(&f, &c, Who::User);
    let kept = list(&f.deck()).unwrap();
    assert_eq!(kept.len(), 2);
    assert_eq!(kept[0].rev8, a.revision()[..8]);
    assert_eq!(kept[0].who, "model");
    assert_eq!(kept[1].rev8, b.revision()[..8]);
    assert_eq!(kept[1].who, "user");
    assert_eq!(kept[0].load().unwrap(), a);
    assert!(kept[0].path.starts_with(f.0.join(".history/talk")));
    assert!(kept[0].time().ends_with("UTC"));
}

#[test]
fn identical_revisions_are_skipped() {
    let f = Folder::new();
    let (a, b) = (deck(&["a"]), deck(&["b"]));
    write(&f, &a, Who::Model);
    write(&f, &b, Who::Model);
    write(&f, &b, Who::Model); // same content again: nothing replaced
    assert_eq!(list(&f.deck()).unwrap().len(), 1);
    // a -> b -> a -> b: the second `a` replaces b, then b replaces a again;
    // the newest kept version is never repeated back to back.
    write(&f, &a, Who::User);
    write(&f, &a, Who::User);
    let kept = list(&f.deck()).unwrap();
    assert_eq!(kept.len(), 2);
    assert_ne!(kept[0].rev8, kept[1].rev8);
}

#[test]
fn only_the_last_fifty_versions_are_kept() {
    let f = Folder::new();
    for i in 0..(KEEP + 8) {
        write(&f, &deck(&[&format!("version {i}")]), Who::Model);
    }
    let kept = list(&f.deck()).unwrap();
    assert_eq!(kept.len(), KEEP);
    // The oldest were pruned; the newest replaced version is still there.
    let newest = deck(&[&format!("version {}", KEEP + 6)]);
    assert_eq!(kept.last().unwrap().rev8, newest.revision()[..8]);
    let first_ever = deck(&["version 0"]);
    assert!(kept.iter().all(|e| e.rev8 != first_ever.revision()[..8]));
}

#[test]
fn a_version_is_found_by_revision_prefix() {
    let f = Folder::new();
    let (a, b) = (deck(&["a"]), deck(&["b"]));
    write(&f, &a, Who::Model);
    write(&f, &b, Who::Model);
    let full = a.revision();
    assert_eq!(find(&f.deck(), &full[..12]).unwrap().rev8, full[..8]);
    assert_eq!(find(&f.deck(), &full[..4]).unwrap().rev8, full[..8]);
    assert!(find(&f.deck(), "ffffffff").is_err());
    assert!(find(&f.deck(), "ab").is_err());
}

#[test]
fn changes_are_summarized_on_one_line() {
    let before = deck(&["a", "b"]);
    assert_eq!(describe_change(&before, &before), "no change");
    let edited = deck(&["a", "B"]);
    assert_eq!(describe_change(&before, &edited), "slide 2 changed");
    let fewer = deck(&["a"]);
    assert_eq!(describe_change(&before, &fewer), "1 slide removed");
    let with_image = Deck::check(
        &json!({"version": 2, "title": "Synthetic", "slides": [
            {"id": "s0", "children": [{"id": "n0", "kind": "text", "text": "a"}, {"id": "i", "kind": "image", "src": "assets/x.png"}]},
            {"id": "s1", "children": [{"id": "n1", "kind": "text", "text": "b"}]}
        ]})
        .to_string(),
    )
    .unwrap();
    assert_eq!(
        describe_change(&before, &with_image),
        "slide 1 changed · images 0 → 1"
    );
}
