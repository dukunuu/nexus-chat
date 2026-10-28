//! Doc mode of the backend: the file is the source of truth, every `DocApply`
//! is one undo step, and comments and suggestions share the model's sidecar.

use nexus_core::deck::history::Who;
use nexus_core::deck::link::Message as Link;
use nexus_core::doc::feedback::{self as comments, Anchor, Edit, EditStatus, Range};
use nexus_core::doc::{self, Doc, Kind as BlockKind};
use serde_json::json;

use super::*;

struct Temp(PathBuf);
impl Temp {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!("nexus-studio-doc-{}", Uuid::new_v4()));
        fs::create_dir_all(&path).unwrap();
        Self(path.canonicalize().unwrap())
    }
    fn files(&self) -> PathBuf {
        self.0.join("files")
    }
    fn doc_path(&self) -> PathBuf {
        self.files().join("docs/q3.doc.json")
    }
    /// A backend with `docs/q3.doc.json` open.
    fn backend(&self) -> Backend {
        let mut doc = Doc::from_design("Q3 strategy", &nexus_core::design::Design::default());
        doc.blocks = doc::markdown::parse_document(
            "# Q3 strategy\n\nWe grew **18%** while churn fell.\n\n## Goals\n\n- Ship the docs canvas\n- Templates\n\n> Make the report and the deck look like one company.\n",
            true,
        )
        .blocks;
        let doc = Doc::check(&serde_json::to_string(&doc).unwrap()).unwrap();
        doc::storage::save(&self.files(), "docs/q3.doc.json", &doc, Who::Model).unwrap();
        Backend::start(Some(&self.doc_path()), &self.files(), &self.0.join("data")).unwrap()
    }
}
impl Drop for Temp {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn send(backend: &mut Backend, value: serde_json::Value) -> Event {
    backend.dispatch(&serde_json::to_string(&value).unwrap())
}
fn doc_event(event: Event) -> (Doc, Vec<String>, Option<String>) {
    let Event::Doc {
        doc,
        changed,
        notice,
        ..
    } = event
    else {
        panic!("{event:?}");
    };
    (doc, changed, notice)
}
fn on_disk(backend: &Backend) -> Doc {
    doc::storage::load(&backend.files_dir, "docs/q3.doc.json").unwrap()
}
fn text(doc: &Doc, id: &str) -> String {
    doc.block(id).unwrap().text().unwrap().to_string()
}
/// What the model's tool does: a checked edit written to the same file.
fn model_edit(temp: &Temp, ops: serde_json::Value) {
    let before = doc::storage::load(&temp.files(), "docs/q3.doc.json").unwrap();
    let ops: Vec<doc::Op> = serde_json::from_value(ops).unwrap();
    let next = doc::apply(&before, &ops).unwrap();
    doc::storage::save(&temp.files(), "docs/q3.doc.json", &next, Who::Model).unwrap();
}
fn png_hex() -> String {
    let mut png = std::io::Cursor::new(Vec::new());
    image::RgbaImage::from_pixel(3, 2, image::Rgba([200, 60, 20, 255]))
        .write_to(&mut png, image::ImageFormat::Png)
        .unwrap();
    png.into_inner()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

#[test]
fn opens_a_doc_by_path_and_loads_it() {
    let temp = Temp::new();
    let mut backend = temp.backend();
    assert!(backend.is_doc());
    assert_eq!(backend.title(), "Q3 strategy");
    let event = send(&mut backend, json!({"command":"Load"}));
    let Event::Doc {
        doc,
        relative,
        revision,
        design,
        can_undo,
        linked,
        ..
    } = event
    else {
        panic!("not a doc event");
    };
    assert_eq!(relative, "docs/q3.doc.json");
    assert_eq!(revision, doc.revision());
    assert_eq!(doc.blocks.len(), 5);
    assert!(!can_undo && !linked);
    assert_eq!(design.name, "nexus");
    assert!(!design.update_available);
    // A doc that does not exist yet is created, as a deck is.
    let fresh = temp.files().join("docs/new.doc.json");
    let backend = Backend::start(Some(&fresh), &temp.files(), &temp.0.join("data2")).unwrap();
    assert!(fresh.is_file() && backend.is_doc());
    assert!(read_doc_file(&fresh).blocks.is_empty());
}

fn read_doc_file(path: &Path) -> Doc {
    super::doc::read_doc(path).unwrap()
}

#[test]
fn each_apply_is_one_undo_step_and_reaches_the_file_at_once() {
    let temp = Temp::new();
    let mut backend = temp.backend();
    let start = on_disk(&backend);
    let (doc, changed, notice) = doc_event(send(
        &mut backend,
        json!({"command":"DocApply","ops":[
            {"op":"update_block","id":"b2","patch":{"text":"We grew **22%**."}},
            {"op":"insert_blocks","after":"b2","blocks":[{"id":"n1","kind":"paragraph","text":"New."}]}
        ]}),
    ));
    assert!(changed.is_empty() && notice.is_none());
    assert_eq!(text(&doc, "b2"), "We grew **22%**.");
    assert_eq!(doc.blocks[2].id, "n1");
    assert_eq!(on_disk(&backend), doc, "autosaved");
    let session = backend.session().unwrap();
    assert_eq!(session.undo.len(), 1, "two operations, one undo step");
    // A failed batch changes nothing and says where.
    let error = send(
        &mut backend,
        json!({"command":"DocApply","ops":[{"op":"delete_blocks","ids":["nope"]}]}),
    );
    let Event::Error { message } = error else {
        panic!("{error:?}")
    };
    assert!(message.contains("nope"), "{message}");
    assert_eq!(on_disk(&backend), doc);
    assert_ne!(on_disk(&backend), start);
    // History keeps the version a user write replaced.
    let kept = nexus_core::deck::history::list(&backend.path).unwrap();
    assert!(kept.iter().any(|e| e.who == "user"), "{kept:?}");
}

#[test]
fn undo_and_redo_are_written_to_the_file_so_the_model_sees_them() {
    let temp = Temp::new();
    let mut backend = temp.backend();
    let start = on_disk(&backend);
    send(
        &mut backend,
        json!({"command":"DocApply","ops":[{"op":"set_title","title":"Renamed"}]}),
    );
    send(
        &mut backend,
        json!({"command":"DocApply","ops":[{"op":"update_block","id":"b2","patch":{"text":"Two"}}]}),
    );
    let (doc, ..) = doc_event(send(&mut backend, json!({"command":"Undo"})));
    assert_eq!(text(&doc, "b2"), "We grew **18%** while churn fell.");
    assert_eq!(on_disk(&backend), doc);
    let (doc, ..) = doc_event(send(&mut backend, json!({"command":"Redo"})));
    assert_eq!(text(&doc, "b2"), "Two");
    assert_eq!(on_disk(&backend), doc);
    send(&mut backend, json!({"command":"Undo"}));
    let (doc, ..) = doc_event(send(&mut backend, json!({"command":"Undo"})));
    assert_eq!(doc, start);
    assert_eq!(on_disk(&backend), start);
    // Nothing left to undo: not an error, not a change.
    let (again, ..) = doc_event(send(&mut backend, json!({"command":"Undo"})));
    assert_eq!(again, start);
}

#[test]
fn an_external_edit_reloads_as_one_undo_step_listing_the_changed_blocks() {
    let temp = Temp::new();
    let mut backend = temp.backend();
    assert!(backend.poll().unwrap().is_none(), "nothing changed yet");
    let original = backend.session().unwrap().doc.clone();
    model_edit(
        &temp,
        json!([
            {"op":"update_block","id":"b2","patch":{"text":"Written by the model."}},
            {"op":"replace_text","id":"b5","find":"one company","replace":"one voice"},
            {"op":"insert_blocks","after":"b5","blocks":[{"id":"x1","kind":"divider"}]}
        ]),
    );
    let (doc, changed, notice) = doc_event(backend.poll().unwrap().expect("reloaded"));
    assert_eq!(changed, ["b2", "b5", "x1"], "unchanged blocks do not flash");
    assert!(notice.unwrap().contains("reloaded"));
    assert_eq!(text(&doc, "b2"), "Written by the model.");
    assert!(backend.poll().unwrap().is_none(), "reported once");
    assert_eq!(backend.session().unwrap().undo.len(), 1);
    // Cmd+Z takes the model's whole edit back, and the file says so.
    let (undone, ..) = doc_event(send(&mut backend, json!({"command":"Undo"})));
    assert_eq!(undone, original);
    assert_eq!(on_disk(&backend), original);
    assert!(
        backend.poll().unwrap().is_none(),
        "our own write is not news"
    );
    let (redone, ..) = doc_event(send(&mut backend, json!({"command":"Redo"})));
    assert_eq!(text(&redone, "b2"), "Written by the model.");
}

#[test]
fn the_file_wins_when_a_local_action_races_an_external_change() {
    let temp = Temp::new();
    let mut backend = temp.backend();
    model_edit(
        &temp,
        json!([{"op":"update_block","id":"b2","patch":{"text":"Model first."}}]),
    );
    // No poll yet: the window's action arrives against the old file.
    let (doc, changed, notice) = doc_event(send(
        &mut backend,
        json!({"command":"DocApply","ops":[{"op":"update_block","id":"b2","patch":{"text":"User second."}}]}),
    ));
    assert_eq!(
        text(&doc, "b2"),
        "Model first.",
        "the user's action was not applied"
    );
    assert_eq!(changed, ["b2"]);
    assert!(notice.unwrap().contains("changed on disk first"));
    assert_eq!(text(&on_disk(&backend), "b2"), "Model first.");
    // A focus report does not trip over a changed file; it is answered as usual.
    model_edit(
        &temp,
        json!([{"op":"update_block","id":"b2","patch":{"text":"Model again."}}]),
    );
    let focus = send(
        &mut backend,
        json!({"command":"DocFocus","block":"b2","selection":null}),
    );
    assert!(matches!(focus, Event::DocFeedback { .. }), "{focus:?}");
    assert!(
        backend.poll().unwrap().is_some(),
        "the poll still delivers the reload"
    );
}

#[test]
fn comments_are_anchored_to_text_ranges_and_share_the_sidecar() {
    let temp = Temp::new();
    let mut backend = temp.backend();
    // Not linked: Send is refused, a note is not.
    let anchor = json!({"block":"b2","range":{"start":8,"end":11},"quote":"18%"});
    let refused = send(
        &mut backend,
        json!({"command":"DocCommentCreate","anchor":anchor,"text":"Cite this","send":true}),
    );
    assert!(matches!(refused, Event::Error { .. }), "{refused:?}");
    let created = send(
        &mut backend,
        json!({"command":"DocCommentCreate","anchor":anchor,"text":"Cite this","send":false}),
    );
    let Event::DocFeedback { threads, .. } = created else {
        panic!("{created:?}")
    };
    assert_eq!(threads.len(), 1);
    assert_eq!(threads[0].anchor.quote, "18%");
    assert_eq!(threads[0].anchor.range, Some(Range { start: 8, end: 11 }));
    let stored = comments::load(&backend.path).unwrap();
    assert_eq!(stored[0].messages[0].text, "Cite this");
    // Bad anchors fail where the anchor is wrong.
    for (bad, why) in [
        (
            json!({"block":"b2","range":{"start":8,"end":11},"quote":"19%"}),
            "quote",
        ),
        (json!({"block":"nope","quote":""}), "block"),
        (json!({"block":null,"quote":""}), "block"),
    ] {
        let error = send(
            &mut backend,
            json!({"command":"DocCommentCreate","anchor":bad,"text":"x","send":false}),
        );
        let Event::Error { message } = error else {
            panic!("{error:?}")
        };
        assert!(message.to_lowercase().contains(why), "{message}");
    }
    // Linked: Send goes out as a doc comment, then a reply.
    backend.linked = true;
    backend.outgoing.clear();
    send(
        &mut backend,
        json!({"command":"DocCommentCreate","anchor":{"block":"b3","quote":""},"text":"Why these goals?","send":true}),
    );
    let Link::DocComment {
        deck_path,
        block,
        range,
        quote,
        text,
        thread_id,
    } = backend.outgoing[0].clone()
    else {
        panic!("{:?}", backend.outgoing)
    };
    assert_eq!(deck_path, backend.path);
    assert_eq!((block.as_str(), range, quote.as_str()), ("b3", None, ""));
    assert_eq!(text, "Why these goals?");
    send(
        &mut backend,
        json!({"command":"CommentReply","thread":thread_id,"text":"Because Q3.","send":true}),
    );
    assert!(
        matches!(&backend.outgoing[1], Link::Reply { thread_id: t, text, .. } if *t == thread_id && text == "Because Q3.")
    );
    // Resolve and reopen.
    send(
        &mut backend,
        json!({"command":"CommentResolve","thread":thread_id}),
    );
    assert!(comments::load(&backend.path).unwrap()[1].resolved.is_some());
    send(
        &mut backend,
        json!({"command":"CommentReopen","thread":thread_id}),
    );
    assert!(comments::load(&backend.path).unwrap()[1].resolved.is_none());
    assert!(matches!(
        send(
            &mut backend,
            json!({"command":"CommentResolve","thread":"ghost"})
        ),
        Event::Error { .. }
    ));
    // The model's reply in the sidecar arrives through the poll.
    let mut threads = comments::load(&backend.path).unwrap();
    threads[0].messages.push(comments::Message::new_for(
        comments::Role::Model,
        "Done.".into(),
        vec![],
    ));
    comments::save(&backend.path, &threads).unwrap();
    let Event::Doc { threads, .. } = backend.poll().unwrap().expect("comments changed") else {
        panic!("a doc event")
    };
    assert_eq!(threads[0].messages.len(), 2);
}

#[test]
fn a_local_edit_keeps_comment_anchors_on_their_text() {
    let temp = Temp::new();
    let mut backend = temp.backend();
    send(
        &mut backend,
        json!({"command":"DocCommentCreate","anchor":{"block":"b2","range":{"start":8,"end":11},"quote":"18%"},"text":"Cite","send":false}),
    );
    // Typing before the quote shifts it; the anchor follows its text.
    send(
        &mut backend,
        json!({"command":"DocApply","ops":[{"op":"update_block","id":"b2","patch":{"text":"This year we grew **18%** while churn fell."}}]}),
    );
    let stored = comments::load(&backend.path).unwrap();
    assert_eq!(stored[0].anchor.range, Some(Range { start: 18, end: 21 }));
    // Rewriting the quote degrades to the block; undo brings the range back.
    send(
        &mut backend,
        json!({"command":"DocApply","ops":[{"op":"update_block","id":"b2","patch":{"text":"Growth was strong."}}]}),
    );
    assert_eq!(comments::load(&backend.path).unwrap()[0].anchor.range, None);
    send(&mut backend, json!({"command":"Undo"}));
    assert_eq!(
        comments::load(&backend.path).unwrap()[0].anchor.range,
        Some(Range { start: 18, end: 21 })
    );
}

/// A model comment on the block its first op edits, carrying that suggestion.
fn suggestion_thread(backend: &Backend, id: &str, ops: serde_json::Value) -> comments::Thread {
    let block = ops[0]["id"].as_str().unwrap().to_string();
    let doc = backend.session().unwrap().doc.clone();
    let (mut valid, errors) = doc::suggest::prepare(
        &doc,
        vec![doc::suggest::RawSuggestion {
            why: "Tighter".into(),
            ops: ops.as_array().unwrap().clone(),
        }],
    );
    assert!(errors.is_empty(), "{errors:?}");
    let anchor = if block == "b2" {
        Anchor {
            block: Some(block),
            range: Some(Range { start: 8, end: 11 }),
            quote: "18%".into(),
        }
    } else {
        Anchor {
            block: Some(block),
            range: None,
            quote: String::new(),
        }
    };
    let edit = Edit::new(&doc, valid.pop().unwrap(), &anchor);
    comments::Thread {
        id: id.into(),
        anchor,
        severity: comments::Severity::Should,
        created: comments::now(),
        resolved: None,
        messages: vec![comments::Message::new_for(
            comments::Role::Model,
            "Be exact.".into(),
            vec![edit],
        )],
    }
}
fn edit_id(thread: &comments::Thread) -> String {
    thread.messages[0].edits[0].proposal.id.clone()
}

#[test]
fn a_suggestion_previews_applies_as_one_undo_step_and_can_be_dismissed() {
    let temp = Temp::new();
    let mut backend = temp.backend();
    let one = suggestion_thread(
        &backend,
        "t1",
        json!([{"op":"replace_text","id":"b2","find":"18%","replace":"18.4%"}]),
    );
    let two = suggestion_thread(
        &backend,
        "t2",
        json!([{"op":"replace_text","id":"b5","find":"one company","replace":"one voice"}]),
    );
    let (one_id, two_id) = (edit_id(&one), edit_id(&two));
    comments::save(&backend.path, &[one, two]).unwrap();
    backend.poll().unwrap().expect("threads arrived");
    // Preview names what would change and writes nothing.
    let before = on_disk(&backend);
    let preview = send(
        &mut backend,
        json!({"command":"PreviewEdit","owner":"t1","edit":one_id}),
    );
    let Event::DocPreview {
        changed, removed, ..
    } = preview
    else {
        panic!("{preview:?}")
    };
    assert_eq!(changed.len(), 1);
    assert!(matches!(&changed[0].kind, BlockKind::Paragraph { text } if text.contains("18.4%")));
    assert!(removed.is_empty());
    assert_eq!(on_disk(&backend), before);
    // Apply is one undo step and settles the edit.
    let (doc, ..) = doc_event(send(
        &mut backend,
        json!({"command":"ApplyEdit","owner":"t1","edit":one_id}),
    ));
    assert!(text(&doc, "b2").contains("18.4%"));
    assert_eq!(backend.session().unwrap().undo.len(), 1);
    let stored = comments::load(&backend.path).unwrap();
    assert_eq!(stored[0].messages[0].edits[0].status, EditStatus::Applied);
    assert_eq!(
        stored[1].messages[0].edits[0].status,
        EditStatus::Pending,
        "an edit to another block stays valid"
    );
    // The same edit cannot be applied twice.
    assert!(matches!(
        send(
            &mut backend,
            json!({"command":"ApplyEdit","owner":"t1","edit":one_id})
        ),
        Event::Error { .. }
    ));
    // Dismiss keeps the text and settles the edit.
    let dismissed = send(
        &mut backend,
        json!({"command":"DismissEdit","owner":"t2","edit":two_id}),
    );
    assert!(matches!(dismissed, Event::DocFeedback { .. }));
    assert!(text(&backend.session().unwrap().doc, "b5").contains("one company"));
    assert_eq!(
        comments::load(&backend.path).unwrap()[1].messages[0].edits[0].status,
        EditStatus::Dismissed
    );
    assert!(matches!(
        send(
            &mut backend,
            json!({"command":"PreviewEdit","owner":"t2","edit":two_id})
        ),
        Event::Error { .. }
    ));
}

#[test]
fn an_edit_made_since_outdates_a_suggestion() {
    let temp = Temp::new();
    let mut backend = temp.backend();
    let thread = suggestion_thread(
        &backend,
        "t1",
        json!([{"op":"replace_text","id":"b2","find":"18%","replace":"18.4%"}]),
    );
    let id = edit_id(&thread);
    comments::save(&backend.path, &[thread]).unwrap();
    backend.poll().unwrap();
    send(
        &mut backend,
        json!({"command":"DocApply","ops":[{"op":"update_block","id":"b2","patch":{"text":"Rewritten without the number."}}]}),
    );
    let stored = comments::load(&backend.path).unwrap();
    assert_eq!(stored[0].messages[0].edits[0].status, EditStatus::Outdated);
    let error = send(
        &mut backend,
        json!({"command":"ApplyEdit","owner":"t1","edit":id}),
    );
    let Event::Error { message } = error else {
        panic!("{error:?}")
    };
    assert!(message.contains("Outdated"));
}

#[test]
fn accept_all_is_one_undo_step_and_reject_all_changes_no_text() {
    let temp = Temp::new();
    let mut backend = temp.backend();
    let one = suggestion_thread(
        &backend,
        "t1",
        json!([{"op":"replace_text","id":"b2","find":"18%","replace":"18.4%"}]),
    );
    let two = suggestion_thread(
        &backend,
        "t2",
        json!([{"op":"replace_text","id":"b5","find":"one company","replace":"one voice"}]),
    );
    comments::save(&backend.path, &[one, two]).unwrap();
    backend.poll().unwrap();
    let start = backend.session().unwrap().doc.clone();
    let (doc, ..) = doc_event(send(
        &mut backend,
        json!({"command":"DocResolveEdits","accept":true}),
    ));
    assert!(text(&doc, "b2").contains("18.4%") && text(&doc, "b5").contains("one voice"));
    assert_eq!(backend.session().unwrap().undo.len(), 1);
    let stored = comments::load(&backend.path).unwrap();
    assert!(
        stored
            .iter()
            .all(|t| t.messages[0].edits[0].status == EditStatus::Applied)
    );
    let (undone, ..) = doc_event(send(&mut backend, json!({"command":"Undo"})));
    assert_eq!(undone, start);
    // Reject all on fresh suggestions.
    let again = suggestion_thread(
        &backend,
        "t3",
        json!([{"op":"replace_text","id":"b2","find":"18%","replace":"20%"}]),
    );
    comments::save(&backend.path, &[again]).unwrap();
    backend.poll().unwrap();
    let (doc, ..) = doc_event(send(
        &mut backend,
        json!({"command":"DocResolveEdits","accept":false}),
    ));
    assert_eq!(doc, start);
    assert_eq!(
        comments::load(&backend.path).unwrap()[0].messages[0].edits[0].status,
        EditStatus::Dismissed
    );
}

#[test]
fn every_format_exports_under_exports_without_overwriting_and_reveal_is_confined() {
    let temp = Temp::new();
    let mut backend = temp.backend();
    // An image block, so Markdown has something to bring along.
    let bytes = (0..png_hex().len() / 2)
        .map(|i| u8::from_str_radix(&png_hex()[i * 2..i * 2 + 2], 16).unwrap())
        .collect::<Vec<_>>();
    let src = nexus_core::deck::assets::store_image_bytes(
        &bytes,
        "pic",
        "pic",
        &asset_root(&backend.path).unwrap(),
    )
    .unwrap();
    send(
        &mut backend,
        json!({"command":"DocApply","ops":[{"op":"insert_blocks","blocks":[{"kind":"image","src":src,"alt":"Pic","caption":"Figure","width":"full"}]}]}),
    );
    let exports = backend.files_dir.canonicalize().unwrap().join("exports");
    let mut seen = Vec::new();
    for (format, magic) in [
        ("docx", &b"PK"[..]),
        ("pdf", b"%PDF"),
        ("md", b"# Q3 strategy"),
        ("html", b"<!doctype html>"),
    ] {
        let event = send(&mut backend, json!({"command":"DocExport","format":format}));
        let Event::Exported { path, .. } = event else {
            panic!("{format}: {event:?}")
        };
        assert_eq!(path.parent().unwrap(), exports);
        assert!(
            fs::read(&path).unwrap().starts_with(magic),
            "{format}: {:?}",
            &fs::read(&path).unwrap()[..8]
        );
        seen.push(path);
    }
    // Markdown carries its image into exports/assets.
    let md = fs::read_to_string(&seen[2]).unwrap();
    let image = md
        .split("](")
        .nth(1)
        .unwrap()
        .split(['"', ')'])
        .next()
        .unwrap()
        .trim();
    assert!(exports.join(image).is_file(), "{image}");
    // Never overwrites.
    let Event::Exported { path, .. } =
        send(&mut backend, json!({"command":"DocExport","format":"md"}))
    else {
        panic!("second export")
    };
    assert!(path.to_string_lossy().ends_with(" (2).md"), "{path:?}");
    // Errors: unknown format, relative destination, doc unchanged.
    for bad in [
        json!({"command":"DocExport","format":"rtf"}),
        json!({"command":"DocExport","format":"md","path":"relative.md"}),
    ] {
        assert!(matches!(send(&mut backend, bad), Event::Error { .. }));
    }
    assert!(
        backend.session().unwrap().undo.len() == 1,
        "exporting is not an edit"
    );
    // Reveal only reaches what this window exported.
    assert!(matches!(
        send(
            &mut backend,
            json!({"command":"Reveal","path":"/etc/hosts"})
        ),
        Event::Error { .. }
    ));
}

#[test]
fn page_preview_serves_core_rendered_pngs_keyed_by_revision() {
    let temp = Temp::new();
    let mut backend = temp.backend();
    let event = send(&mut backend, json!({"command":"DocPages"}));
    let Event::Pages { revision, pages } = event else {
        panic!("{event:?}")
    };
    assert!(!pages.is_empty());
    assert_eq!(revision, backend.session().unwrap().doc.revision());
    assert!(
        pages[0]
            .src
            .starts_with(&format!("/pages/{}-1", &revision[..8]))
    );
    assert!(pages[0].width > 500.0 && pages[0].height > pages[0].width);
    let (mime, bytes) = backend
        .serve(&format!("nexus://localhost{}", pages[0].src))
        .unwrap();
    assert_eq!(mime, "image/png");
    assert!(bytes.starts_with(b"\x89PNG"));
    assert_eq!(
        cache_control(&format!("nexus://localhost{}", pages[0].src), &bytes),
        "no-store"
    );
    // Unknown pages and other revisions are refused.
    for bad in [
        "/pages/00000000-1.png",
        &format!("/pages/{}-99.png", &revision[..8]),
        &format!("/pages/{}-0.png", &revision[..8]),
        "/pages/..%2Fx-1.png",
        "/pages/nonsense.png",
    ] {
        assert!(
            backend.serve(&format!("nexus://localhost{bad}")).is_err(),
            "{bad}"
        );
    }
    // After an edit the old pages are stale until the preview asks again.
    send(
        &mut backend,
        json!({"command":"DocApply","ops":[{"op":"set_title","title":"Changed"}]}),
    );
    assert!(
        backend
            .serve(&format!("nexus://localhost{}", pages[0].src))
            .is_err()
    );
    let Event::Pages { pages: fresh, .. } = send(&mut backend, json!({"command":"DocPages"}))
    else {
        panic!("pages")
    };
    assert_ne!(fresh[0].src, pages[0].src);
    assert!(
        backend
            .serve(&format!("nexus://localhost{}", fresh[0].src))
            .is_ok()
    );
}

#[test]
fn imported_images_are_stored_by_content_beside_the_doc_and_stay_confined() {
    let temp = Temp::new();
    let mut backend = temp.backend();
    let event = send(
        &mut backend,
        json!({"command":"DocImportAsset","name":"../../pasted shot.png","data":png_hex()}),
    );
    let Event::Asset { src } = event else {
        panic!("{event:?}")
    };
    assert!(src.starts_with("assets/") && src.ends_with(".png"), "{src}");
    let file = asset_root(&backend.path)
        .unwrap()
        .join(src.strip_prefix("assets/").unwrap());
    assert!(file.is_file());
    assert!(file.starts_with(temp.files().join("docs")));
    // The same bytes land on the same file, and the page can fetch it.
    let Event::Asset { src: again } = send(
        &mut backend,
        json!({"command":"DocImportAsset","name":"other.png","data":png_hex()}),
    ) else {
        panic!("asset")
    };
    assert_eq!(
        fs::read(&file).unwrap(),
        fs::read(
            asset_root(&backend.path)
                .unwrap()
                .join(again.strip_prefix("assets/").unwrap())
        )
        .unwrap()
    );
    assert_eq!(
        backend
            .serve(&format!("nexus://localhost/{src}"))
            .unwrap()
            .0,
        "image/png"
    );
    // A block can then use it, and the doc event lists its hash.
    let (doc, ..) = doc_event(send(
        &mut backend,
        json!({"command":"DocApply","ops":[{"op":"insert_blocks","blocks":[{"kind":"image","src":src,"alt":"Shot"}]}]}),
    ));
    assert!(
        doc.blocks
            .iter()
            .any(|b| matches!(&b.kind, BlockKind::Image { .. }))
    );
    let Event::Doc { assets, .. } = send(&mut backend, json!({"command":"Load"})) else {
        panic!("doc")
    };
    assert_eq!(assets.get(&src).map(String::len), Some(64));
    // Not an image, not hex, too big, and a path outside the doc's folder.
    for bad in [
        json!({"command":"DocImportAsset","name":"x.png","data":"68656c6c6f"}),
        json!({"command":"DocImportAsset","name":"x.png","data":"zz"}),
        json!({"command":"DocImportAsset","name":"x.png","data":"abc"}),
        json!({"command":"ImportAsset","path":"relative.png"}),
        json!({"command":"ImportAsset","path":"/etc/hosts"}),
    ] {
        assert!(
            matches!(send(&mut backend, bad.clone()), Event::Error { .. }),
            "{bad}"
        );
    }
    assert!(
        backend
            .serve("nexus://localhost/assets/../q3.doc.json")
            .is_err()
    );
    assert!(
        backend
            .serve("nexus://localhost/assets/%2e%2e/q3.doc.json")
            .is_err()
    );
    // An image dropped by path comes in as a stored copy, not a reference to the path.
    let outside = temp.0.join("photo.png");
    fs::write(
        &outside,
        (0..png_hex().len() / 2)
            .map(|i| u8::from_str_radix(&png_hex()[i * 2..i * 2 + 2], 16).unwrap())
            .collect::<Vec<_>>(),
    )
    .unwrap();
    let Event::Asset { src: copied } = send(
        &mut backend,
        json!({"command":"ImportAsset","path":outside}),
    ) else {
        panic!("asset")
    };
    assert!(
        copied.starts_with("assets/") && copied.matches('/').count() == 1,
        "{copied}"
    );
}

#[test]
fn designs_are_listed_and_applied_and_an_update_is_announced() {
    let temp = Temp::new();
    let mut backend = temp.backend();
    // A stored design the doc can switch to.
    let mut warm = nexus_core::design::Design::default();
    warm.name = "Warm".into();
    warm.colors.insert("accent".into(), "#c0392b".into());
    nexus_core::design::storage::save(&temp.files(), "warm", &warm).unwrap();
    let Event::Designs { available } = send(&mut backend, json!({"command":"DocDesigns"})) else {
        panic!("designs")
    };
    let slugs: Vec<_> = available.iter().map(|d| d.slug.as_str()).collect();
    // The stored design sorts in with nexus and the curated presets.
    assert!(
        slugs.contains(&"warm") && slugs.contains(&"editorial") && slugs.is_sorted(),
        "{slugs:?}"
    );
    let current: Vec<_> = available
        .iter()
        .filter(|d| d.current)
        .map(|d| d.slug.as_str())
        .collect();
    assert_eq!(current, ["nexus"]);
    let (doc, ..) = doc_event(send(
        &mut backend,
        json!({"command":"DocApplyDesign","design":"warm"}),
    ));
    assert_eq!(doc.design.as_deref(), Some("warm"));
    assert_eq!(doc.theme.colors["accent"], "#c0392b");
    assert_eq!(backend.session().unwrap().undo.len(), 1);
    let Event::Doc { design, .. } = send(&mut backend, json!({"command":"Load"})) else {
        panic!("doc")
    };
    assert_eq!(
        (design.name.as_str(), design.update_available),
        ("Warm", false)
    );
    // The design moves on: the doc says an update is available, and applying it follows.
    warm.colors.insert("accent".into(), "#8e44ad".into());
    nexus_core::design::storage::save(&temp.files(), "warm", &warm).unwrap();
    let Event::Doc { design, .. } = send(&mut backend, json!({"command":"Load"})) else {
        panic!("doc")
    };
    assert!(design.update_available);
    let (doc, ..) = doc_event(send(
        &mut backend,
        json!({"command":"DocApplyDesign","design":null}),
    ));
    assert_eq!(doc.theme.colors["accent"], "#8e44ad");
    let Event::Doc { design, .. } = send(&mut backend, json!({"command":"Load"})) else {
        panic!("doc")
    };
    assert!(!design.update_available);
    assert!(matches!(
        send(
            &mut backend,
            json!({"command":"DocApplyDesign","design":"../nope"})
        ),
        Event::Error { .. }
    ));
}

#[test]
fn focus_goes_out_on_the_link_only_when_linked_and_the_model_can_move_it() {
    let temp = Temp::new();
    let mut backend = temp.backend();
    send(
        &mut backend,
        json!({"command":"DocFocus","block":"b3","selection":"Goals"}),
    );
    assert!(backend.outgoing.is_empty());
    backend.linked = true;
    send(
        &mut backend,
        json!({"command":"DocFocus","block":"b3","selection":"Goals"}),
    );
    assert_eq!(
        backend.outgoing,
        [Link::DocFocus {
            deck_path: backend.path.clone(),
            block: Some("b3".into()),
            selection: Some("Goals".into()),
        }]
    );
    // Nexus moves the window to a block.
    let moved = backend
        .receive_link(Link::DocFocus {
            deck_path: backend.path.clone(),
            block: Some("b5".into()),
            selection: None,
        })
        .unwrap();
    assert!(matches!(moved, Event::DocFocus { block: Some(b) } if b == "b5"));
    // And to another file entirely.
    let other = temp.files().join("docs/other.doc.json");
    let hello = backend.outgoing.len();
    let opened = backend
        .receive_link(Link::Open {
            deck_path: other.clone(),
        })
        .unwrap();
    assert!(matches!(opened, Event::Doc { .. }));
    assert_eq!(backend.path, other);
    assert!(
        matches!(&backend.outgoing[hello..], [Link::Hello { deck_path }] if *deck_path == other)
    );
    // Opening the file that is already open keeps its undo history.
    send(
        &mut backend,
        json!({"command":"DocApply","ops":[{"op":"set_title","title":"Kept"}]}),
    );
    backend
        .receive_link(Link::Open {
            deck_path: other.clone(),
        })
        .unwrap();
    assert_eq!(backend.session().unwrap().undo.len(), 1);
}

#[test]
fn doc_and_deck_commands_stay_on_their_own_side_and_files_switch_modes() {
    let temp = Temp::new();
    let mut backend = temp.backend();
    for deck_only in [
        json!({"command":"Apply","ops":[]}),
        json!({"command":"Export","format":"pptx"}),
        json!({"command":"ListFonts"}),
        json!({"command":"CommentCreate","anchor":{"slide":"s1","block":null,"x":1,"y":1,"px":true},"text":"x","send":false}),
    ] {
        assert!(
            matches!(send(&mut backend, deck_only.clone()), Event::Error { .. }),
            "{deck_only}"
        );
    }
    assert!(backend.is_doc());
    // Opening a deck leaves doc mode with the deck's own state.
    let deck_path = temp.files().join("decks/talk.deck.json");
    nexus_core::deck::write_atomic(
        &deck_path,
        &serde_json::to_vec(&Deck::new_from_layouts("Talk")).unwrap(),
    )
    .unwrap();
    let event = send(&mut backend, json!({"command":"Open","path":deck_path}));
    assert!(matches!(event, Event::Deck { .. }), "{event:?}");
    assert!(!backend.is_doc());
    assert_eq!(backend.title(), "Talk");
    for doc_only in [
        json!({"command":"DocApply","ops":[]}),
        json!({"command":"DocPages"}),
        json!({"command":"DocDesigns"}),
        json!({"command":"DocFocus","block":null,"selection":null}),
    ] {
        assert!(
            matches!(send(&mut backend, doc_only.clone()), Event::Error { .. }),
            "{doc_only}"
        );
    }
    // A failed switch leaves the open document alone.
    let event = send(
        &mut backend,
        json!({"command":"Open","path":temp.files().join("docs/broken.doc.json")}),
    );
    assert!(
        matches!(event, Event::Doc { .. }),
        "a missing doc is created: {event:?}"
    );
    fs::write(temp.files().join("docs/bad.doc.json"), "{ not json").unwrap();
    let bad = send(
        &mut backend,
        json!({"command":"Open","path":temp.files().join("docs/bad.doc.json")}),
    );
    assert!(matches!(bad, Event::Error { .. }));
    assert!(backend.is_doc());
    assert!(backend.path.ends_with("broken.doc.json"));
    // A new doc, then a new deck.
    let event = send(&mut backend, json!({"command":"NewDoc","title":"Fresh"}));
    let (doc, ..) = doc_event(event);
    assert_eq!(doc.title, "Fresh");
    assert!(backend.path.starts_with(temp.files().join("docs")));
    let event = send(&mut backend, json!({"command":"NewDeck","title":"Slides"}));
    assert!(matches!(event, Event::Deck { .. }));
    assert!(!backend.is_doc());
    let event = send(&mut backend, json!({"command":"NewDoc","title":"Again"}));
    assert!(matches!(event, Event::Doc { .. }));
}

#[test]
fn a_remembered_doc_reopens_and_the_window_title_follows_it() {
    let temp = Temp::new();
    let backend = temp.backend();
    assert_eq!(backend.title(), "Q3 strategy");
    drop(backend);
    let backend = Backend::start(None, &temp.files(), &temp.0.join("data")).unwrap();
    assert!(backend.is_doc());
    assert!(backend.path.ends_with("q3.doc.json"));
    let embedded_ok = ["/doc/doc.js", "/doc/doc.css"]
        .iter()
        .all(|p| backend.serve(&format!("nexus://localhost{p}")).is_ok());
    assert!(embedded_ok, "the doc UI is served from the embedded files");
}

#[test]
fn history_previews_and_restores_kept_docs() {
    let temp = Temp::new();
    let mut backend = temp.backend();
    let start = on_disk(&backend);
    doc_event(send(
        &mut backend,
        json!({"command":"DocApply","ops":[{"op":"update_block","id":"b2","patch":{"text":"We grew **22%**."}}]}),
    ));
    let event = send(&mut backend, json!({"command":"History"}));
    let Event::History { versions } = event else {
        panic!("{event:?}")
    };
    let mine: Vec<_> = versions.iter().filter(|v| v.who == "user").collect();
    assert_eq!(mine.len(), 1, "{versions:?}");
    assert_eq!(mine[0].summary, "Edited 1 block");
    let id = mine[0].id.clone();

    let event = send(&mut backend, json!({"command":"HistoryPreview","id":id}));
    let Event::HistoryPreview {
        doc: Some(kept),
        deck: None,
        ..
    } = event
    else {
        panic!("{event:?}")
    };
    assert_eq!(text(&kept, "b2"), text(&start, "b2"));
    assert_eq!(
        text(&on_disk(&backend), "b2"),
        "We grew **22%**.",
        "a preview writes nothing"
    );

    let (doc, _, _) = doc_event(send(
        &mut backend,
        json!({"command":"HistoryRestore","id":id}),
    ));
    assert_eq!(text(&doc, "b2"), text(&start, "b2"));
    assert_eq!(on_disk(&backend), doc, "restored to the file at once");
    let (doc, _, _) = doc_event(send(&mut backend, json!({"command":"Undo"})));
    assert_eq!(
        text(&doc, "b2"),
        "We grew **22%**.",
        "a restore is one undo step"
    );
}
