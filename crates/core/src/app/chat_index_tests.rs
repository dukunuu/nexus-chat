use std::time::Duration;

use super::*;
use crate::app::tests::app_with_key;
use crate::provider::embedding::tests::MockOllama;
use crate::provider::local::{LocalConfig, LocalRuntime};

fn local_app(endpoint: &str) -> App {
    let mut app = app_with_key();
    app.saved.local = Some(LocalConfig {
        provider: LocalRuntime::Ollama,
        endpoint: Some(endpoint.to_string()),
        list_command: None,
        memory_budget_mb: None,
        context_length: None,
        kv_cache_q8: false,
        parallel_tool_calls: None,
        tier: None,
    });
    app.embedding_model = "local:embeddinggemma-2:270m".to_string();
    app
}

/// The background timer firing: chat backfill only runs when it is due.
fn due(app: &mut App) {
    app.next_chat_index = std::time::Instant::now();
}

async fn drain(app: &mut App) {
    for _ in 0..20 {
        let Some(rx) = app.embed_rx.as_mut() else {
            return;
        };
        let message = tokio::time::timeout(Duration::from_secs(20), rx.recv())
            .await
            .unwrap();
        app.on_embed_done(message);
    }
    panic!("embedding job did not settle");
}

fn chat_documents(mock: &MockOllama) -> usize {
    mock.state
        .lock()
        .unwrap()
        .requests
        .iter()
        .filter(|(path, _)| path == "/api/embed")
        .flat_map(|(_, body)| body["input"].as_array().cloned().unwrap_or_default())
        .filter(|input| {
            input
                .as_str()
                .is_some_and(|text| text.starts_with("title: chat | text: "))
        })
        .count()
}

#[tokio::test]
async fn chats_share_local_file_embedder_and_identity_without_cloud_fallback() {
    let mock = MockOllama::start(false).await;
    // app_with_key has a cloud backend configured; local selection must win
    // for both files and chats, not merely for the file half of the queue.
    let mut app = local_app(&mock.endpoint);
    let session = app
        .db
        .create_session("Past chat", "test", &app.active_space.id, "chat")
        .unwrap()
        .id;
    app.db
        .add_user_message(&session, "financial plan discussed earlier")
        .unwrap();
    let dir = app.space.files_dir(&app.active_space.name);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("notes.txt"), "financial notes").unwrap();
    due(&mut app);
    app.rescan_files();
    drain(&mut app).await;
    // Files go first; the chat backfill takes the next background tick.
    due(&mut app);
    app.start_embedding();
    drain(&mut app).await;
    assert_eq!(chat_documents(&mock), 1);
    let key = app
        .embedding_backend
        .as_ref()
        .unwrap()
        .identity()
        .unwrap()
        .key();
    assert!(key.contains("ollama:"));
    let file_key: String = app
        .db
        .raw()
        .query_row(
            "SELECT identity FROM cache.chunk_embeddings LIMIT 1",
            [],
            |r| r.get(0),
        )
        .unwrap();
    let chat_key: String = app
        .db
        .raw()
        .query_row(
            "SELECT identity FROM cache.message_embeddings LIMIT 1",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(file_key, key);
    assert_eq!(chat_key, key);
    let requests = &mock.state.lock().unwrap().requests;
    let documents: Vec<_> = requests
        .iter()
        .filter(|(path, _)| path == "/api/embed")
        .flat_map(|(_, body)| body["input"].as_array().cloned().unwrap_or_default())
        .filter_map(|input| input.as_str().map(str::to_owned))
        .filter(|text| text.starts_with("title:"))
        .collect();
    assert!(
        documents[0].starts_with("title: notes.txt"),
        "files have priority: {documents:?}"
    );
    assert!(documents[1].starts_with("title: chat"));
    assert_eq!(app.chat_index_status, "chats: 1 of 1 indexed");
}

#[tokio::test]
async fn history_is_reembedded_only_after_identity_changes() {
    let mock = MockOllama::start(false).await;
    let mut app = local_app(&mock.endpoint);
    let session = app
        .db
        .create_session("Past", "test", &app.active_space.id, "chat")
        .unwrap()
        .id;
    app.db
        .add_user_message(&session, "the previous financial discussion")
        .unwrap();
    due(&mut app);
    app.start_embedding();
    drain(&mut app).await;
    assert_eq!(chat_documents(&mock), 1);
    due(&mut app);
    app.start_embedding();
    drain(&mut app).await;
    assert_eq!(chat_documents(&mock), 1);
    mock.state.lock().unwrap().digest = "sha256:two".to_string();
    app.reset_embedding_backend();
    due(&mut app);
    app.start_embedding();
    drain(&mut app).await;
    assert_eq!(chat_documents(&mock), 2);
    let key: String = app
        .db
        .raw()
        .query_row("SELECT identity FROM cache.message_embeddings", [], |r| {
            r.get(0)
        })
        .unwrap();
    assert!(key.contains("sha256:two"));
}

#[tokio::test]
async fn chat_backfill_yields_after_one_bounded_batch_and_keyword_index_works_without_embedder() {
    let mock = MockOllama::start(false).await;
    let mut app = local_app(&mock.endpoint);
    let session = app
        .db
        .create_session("Past", "test", &app.active_space.id, "chat")
        .unwrap()
        .id;
    for n in 0..40 {
        app.db
            .add_user_message(&session, &format!("financial message {n}"))
            .unwrap();
    }
    // Preparing the shared backend doesn't consume a chat backfill tick.
    let embedder = app.file_embedder().unwrap();
    embedder.prepare().await.unwrap();
    app.embedding_backend = Some(embedder);
    due(&mut app);
    app.start_embedding();
    drain(&mut app).await;
    assert_eq!(chat_documents(&mock), chats::INDEX_BATCH);
    assert!(app.embed_rx.is_none());
    assert_eq!(app.chat_index_status, "chats: 16 of 40 indexed");
    due(&mut app);
    app.start_embedding();
    drain(&mut app).await;
    assert_eq!(chat_documents(&mock), chats::INDEX_BATCH * 2);
    app.embedding_model.clear();
    app.reset_embedding_backend();
    due(&mut app);
    app.start_embedding();
    assert!(app.embed_rx.is_none());
    assert_eq!(app.chat_index_status, "chats: 40 of 40 indexed");
}

#[test]
fn incognito_text_never_reaches_the_index() {
    let mut app = app_with_key();
    app.embedding_model.clear();
    app.run_command("incognito").unwrap();
    assert!(app.incognito);
    app.messages.push(crate::db::Message {
        role: "user".to_string(),
        content: "private text".to_string(),
        model: None,
        reasoning: None,
        tokens: None,
        secs: None,
        cost: None,
        phrase: None,
        persona: None,
        created_at: None,
    });
    due(&mut app);
    app.start_embedding();
    assert_eq!(
        app.db
            .raw()
            .query_row("SELECT COUNT(*) FROM cache.message_chunks", [], |r| r
                .get::<_, i64>(0))
            .unwrap(),
        0
    );
}
