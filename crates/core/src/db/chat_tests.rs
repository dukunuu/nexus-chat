use super::*;
use crate::db::Db;

fn fixture() -> (Db, String, String) {
    let db = Db::open_in_memory().unwrap();
    let space = db.list_spaces().unwrap()[0].id.clone();
    let session = db
        .create_session("Past conversation", "test", &space, "chat")
        .unwrap()
        .id;
    (db, space, session)
}

#[test]
fn indexes_only_user_and_assistant_text_with_bounded_overlapping_chunks() {
    let (db, space, session) = fixture();
    let long = "漢字 and a long message ".repeat(500);
    let user = db.add_user_message(&session, &long).unwrap();
    let assistant = db
        .add_assistant_message(
            &session,
            "visible answer",
            None,
            Some("secret reasoning"),
            None,
            None,
            None,
            None,
        )
        .unwrap();
    for role in ["system", "tool", "tool_call", "reasoning", "compaction"] {
        db.insert_message(
            &session,
            role,
            "never indexed",
            None,
            None,
            None,
            None,
            None,
            None,
        )
        .unwrap();
    }
    assert_eq!(index_pending(db.raw()).unwrap(), 2);
    assert_eq!(index_pending(db.raw()).unwrap(), 0);
    let mut stmt = db
        .raw()
        .prepare("SELECT message_id, text FROM cache.message_chunks ORDER BY rowid")
        .unwrap();
    let chunks: Vec<(String, String)> = stmt
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
        .unwrap()
        .map(Result::unwrap)
        .collect();
    assert!(chunks.len() > 3);
    assert!(
        chunks
            .iter()
            .all(|(id, text)| (id == &user || id == &assistant) && text.len() < 1000)
    );
    assert!(
        !chunks
            .iter()
            .any(|(_, text)| text.contains("secret") || text.contains("never"))
    );
    let user_chunks: Vec<_> = chunks.iter().filter(|(id, _)| id == &user).collect();
    assert!(long.ends_with(&user_chunks.last().unwrap().1));
    assert_eq!(progress(db.raw(), &space, None).unwrap(), (2, 2));
}

#[test]
fn identity_changes_requeue_vectors_and_deletes_or_edits_remove_cache_rows() {
    let (db, space, session) = fixture();
    let id = db
        .add_user_message(&session, "a searchable message")
        .unwrap();
    index_pending(db.raw()).unwrap();
    let chunks = missing_vectors(db.raw(), "identity-one").unwrap();
    store_vectors(db.raw(), "identity-one", &chunks, &[(0, vec![1.0, 0.0])]).unwrap();
    assert!(
        missing_vectors(db.raw(), "identity-one")
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        progress(db.raw(), &space, Some("identity-one")).unwrap(),
        (1, 1)
    );
    assert_eq!(missing_vectors(db.raw(), "identity-two").unwrap().len(), 1);
    assert_eq!(
        progress(db.raw(), &space, Some("identity-two")).unwrap(),
        (0, 1)
    );
    db.raw()
        .execute(
            "UPDATE messages SET content = 'replacement' WHERE id = ?1",
            [&id],
        )
        .unwrap();
    assert_eq!(progress(db.raw(), &space, None).unwrap(), (0, 1));
    index_pending(db.raw()).unwrap();
    // An obsolete job cannot restore the old text's vector after an edit.
    store_vectors(db.raw(), "identity-one", &chunks, &[(0, vec![1.0, 0.0])]).unwrap();
    assert_eq!(
        missing_vectors(db.raw(), "identity-one").unwrap()[0].2,
        "replacement"
    );
    db.delete_message(&id).unwrap();
    assert_empty(db.raw());
    db.add_user_message(&session, "session delete").unwrap();
    index_pending(db.raw()).unwrap();
    let chunks = missing_vectors(db.raw(), "identity-one").unwrap();
    store_vectors(db.raw(), "identity-one", &chunks, &[(0, vec![1.0, 0.0])]).unwrap();
    db.delete_session(&session).unwrap();
    assert_empty(db.raw());
}

fn assert_empty(conn: &Connection) {
    for table in ["message_index", "message_chunks", "message_embeddings"] {
        assert_eq!(
            conn.query_row(&format!("SELECT COUNT(*) FROM cache.{table}"), [], |r| r
                .get::<_, i64>(0))
                .unwrap(),
            0,
            "{table}"
        );
    }
}

#[test]
fn synced_messages_are_indexed_and_synced_tombstones_clean_up() {
    let (source, _, session) = fixture();
    let target = Db::open_in_memory().unwrap();
    let root = std::env::temp_dir().join(format!("nexus-chat-sync-{}", uuid::Uuid::new_v4()));
    let layout = crate::space::Space { root: root.clone() };
    let id = source
        .add_user_message(&session, "synced knowledge")
        .unwrap();
    let changes = crate::sync::build_changeset(&source, None, "source").unwrap();
    crate::sync::apply_changeset(&target, &layout, &changes, None).unwrap();
    assert_eq!(index_pending(target.raw()).unwrap(), 1);
    let chunks = missing_vectors(target.raw(), "local-identity").unwrap();
    assert_eq!(chunks[0].0, id);
    store_vectors(target.raw(), "local-identity", &chunks, &[(0, vec![1.0])]).unwrap();
    source.delete_message(&id).unwrap();
    let changes = crate::sync::build_changeset(&source, None, "source").unwrap();
    crate::sync::apply_changeset(&target, &layout, &changes, None).unwrap();
    assert_empty(target.raw());
    source
        .add_user_message(&session, "second synced message")
        .unwrap();
    let changes = crate::sync::build_changeset(&source, None, "source").unwrap();
    crate::sync::apply_changeset(&target, &layout, &changes, None).unwrap();
    index_pending(target.raw()).unwrap();
    source.delete_session(&session).unwrap();
    let changes = crate::sync::build_changeset(&source, None, "source").unwrap();
    crate::sync::apply_changeset(&target, &layout, &changes, None).unwrap();
    assert_empty(target.raw());
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn hybrid_fusion_promotes_semantics_and_scopes_sessions_and_spaces() {
    let (db, space, session) = fixture();
    let semantic = db
        .add_user_message(&session, "financial forecast budget")
        .unwrap();
    let keyword = db.add_user_message(&session, "budget vacation").unwrap();
    let current = db
        .create_session("Current", "test", &space, "chat")
        .unwrap()
        .id;
    db.add_user_message(&current, "budget current secret")
        .unwrap();
    let other_space = db.create_space("other").unwrap().id;
    let other = db
        .create_session("Other", "test", &other_space, "chat")
        .unwrap()
        .id;
    let other_id = db.add_user_message(&other, "budget elsewhere").unwrap();
    index_pending(db.raw()).unwrap();
    let chunks = missing_vectors(db.raw(), "model").unwrap();
    let vectors: Vec<_> = chunks
        .iter()
        .enumerate()
        .map(|(i, (id, _, _))| {
            (
                i64::try_from(i).unwrap(),
                if id == &semantic {
                    vec![1.0, 0.0]
                } else {
                    vec![0.0, 1.0]
                },
            )
        })
        .collect();
    store_vectors(db.raw(), "model", &chunks, &vectors).unwrap();
    let mut request = Search {
        space: &space,
        current: &current,
        all_spaces: false,
        query: "budget",
        vector: Some(("model", &[1.0, 0.0])),
        limit: 8,
    };
    let hits = search(db.raw(), &request).unwrap();
    assert_eq!(hits[0].message, semantic);
    assert!(hits.iter().any(|h| h.message == keyword));
    assert!(hits.iter().all(|h| h.session == session));
    assert!(hits[0].score > hits[1].score);
    request.all_spaces = true;
    assert!(
        search(db.raw(), &request)
            .unwrap()
            .iter()
            .any(|h| h.message == other_id)
    );
    request.vector = None;
    request.query = "vacation";
    assert_eq!(search(db.raw(), &request).unwrap()[0].message, keyword);
    request.vector = Some(("different-identity", &[1.0, 0.0]));
    request.query = "unmentioned";
    assert!(search(db.raw(), &request).unwrap().is_empty());
}

#[test]
fn backfill_and_embedding_batches_are_bounded_and_newest_first() {
    let (db, _, session) = fixture();
    let mut ids = Vec::new();
    for n in 0..40 {
        ids.push(
            db.add_user_message(&session, &format!("message {n}"))
                .unwrap(),
        );
    }
    assert_eq!(index_pending(db.raw()).unwrap(), INDEX_BATCH);
    let chunks = missing_vectors(db.raw(), "model").unwrap();
    assert_eq!(chunks[0].0, *ids.last().unwrap());
    assert!(!chunks.iter().any(|(id, _, _)| id == &ids[0]));
    index_pending(db.raw()).unwrap();
    index_pending(db.raw()).unwrap();
    assert_eq!(
        missing_vectors(db.raw(), "model").unwrap().len(),
        EMBED_BATCH
    );
    db.add_user_message(&session, &"long line ".repeat(10_000))
        .unwrap();
    index_pending(db.raw()).unwrap();
    assert_eq!(
        missing_vectors(db.raw(), "model").unwrap().len(),
        EMBED_BATCH
    );
}
