//! Local EmbeddingGemma 2 flows against an in-process mock Ollama.

use std::time::Duration;

use serde_json::Value;

use super::*;
use crate::app::tests::app_with_key;
use crate::provider::embedding::tests::MockOllama;
use crate::provider::local::LocalConfig;

fn point_at(app: &mut App, endpoint: &str) {
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
}

/// An endpoint nothing listens on.
async fn dead_endpoint() -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("http://{}", listener.local_addr().unwrap());
    drop(listener);
    endpoint
}

/// Apply embed-channel messages until the pipeline is idle, returning the
/// status line seen after each step.
async fn drain(app: &mut App) -> Vec<String> {
    let mut seen = Vec::new();
    for _ in 0..50 {
        let Some(rx) = app.embed_rx.as_mut() else {
            return seen;
        };
        let message = tokio::time::timeout(Duration::from_secs(20), rx.recv())
            .await
            .expect("embedding job stalled");
        app.on_embed_done(message);
        seen.push(app.embedding_status.clone());
    }
    panic!("embedding pipeline did not settle");
}

fn space_with_files(app: &App) -> std::path::PathBuf {
    let dir = app.space.files_dir(&app.active_space.name);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("notes.txt"), "quarterly financial report").unwrap();
    image::RgbImage::from_fn(64, 64, |x, y| image::Rgb([x as u8, y as u8, 7]))
        .save(dir.join("beach.png"))
        .unwrap();
    dir
}

fn stored(app: &App) -> Vec<(String, String, String)> {
    let mut stmt = app
        .db
        .raw()
        .prepare(
            "SELECT files.name, e.location, e.identity FROM cache.chunk_embeddings e
             JOIN files ON files.id = e.file_id ORDER BY files.name, e.seq",
        )
        .unwrap();
    stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
        .unwrap()
        .map(Result::unwrap)
        .collect()
}

fn documents(mock: &MockOllama) -> Vec<String> {
    mock.state
        .lock()
        .unwrap()
        .requests
        .iter()
        .filter(|(path, _)| path == "/api/embed")
        .flat_map(|(_, body)| body["input"].as_array().cloned().unwrap_or_default())
        .filter_map(|input| input.as_str().map(str::to_owned))
        .filter(|text| text.starts_with("title:"))
        .collect()
}

fn image_inputs(mock: &MockOllama) -> usize {
    mock.state
        .lock()
        .unwrap()
        .requests
        .iter()
        .filter(|(path, _)| path == "/api/embed")
        .flat_map(|(_, body)| body["input"].as_array().cloned().unwrap_or_default())
        .filter(|input| input.get("image").is_some())
        .count()
}

#[tokio::test]
async fn local_model_embeds_text_and_images_directly_and_queues_no_ocr() {
    let mock = MockOllama::start(true).await;
    let mut app = app_with_key();
    point_at(&mut app, &mock.endpoint);
    space_with_files(&app);
    app.embedding_model = "local:embeddinggemma-2".to_string();

    app.rescan_files();
    let seen = drain(&mut app).await;
    assert!(seen.iter().any(|s| s.contains("indexing")), "{seen:?}");

    assert!(app.ocr_rx.is_none(), "no OCR or caption job for the image");
    let beach = app
        .files_cache
        .iter()
        .find(|f| f.name == "beach.png")
        .unwrap();
    assert!(
        beach.status.contains("image (direct embeddings)"),
        "{}",
        beach.status
    );
    assert_eq!(
        documents(&mock),
        vec!["title: notes.txt | text: quarterly financial report"]
    );
    assert_eq!(
        image_inputs(&mock),
        2,
        "one capability probe plus the real image"
    );
    let rows = stored(&app);
    assert_eq!(rows.len(), 2, "{rows:?}");
    assert_eq!(rows[0].0, "beach.png");
    assert_eq!(rows[0].1, "image");
    assert_eq!(rows[1].1, "lines 1-1");
    let identity: Value = serde_json::from_str(&rows[0].2).unwrap();
    assert_eq!(identity["model"], "embeddinggemma-2");
    assert_eq!(identity["dimension"], 768);
    assert_eq!(identity["revision"], "sha256:one");
    assert_eq!(rows[0].2, rows[1].2);
    assert_eq!(
        app.embedding_status,
        "embeddings: local · embeddinggemma-2 · 768d · images"
    );
}

#[tokio::test]
async fn same_identity_is_not_reembedded_but_a_new_digest_is() {
    let mock = MockOllama::start(false).await;
    let mut app = app_with_key();
    point_at(&mut app, &mock.endpoint);
    space_with_files(&app);
    app.embedding_model = "local:embeddinggemma-2:270m".to_string();

    app.rescan_files();
    drain(&mut app).await;
    assert_eq!(documents(&mock).len(), 1);
    let first = stored(&app)[0].2.clone();

    // A rescan re-probes the runtime, finds the same identity, embeds nothing.
    app.rescan_files();
    drain(&mut app).await;
    assert_eq!(documents(&mock).len(), 1);
    assert!(stored(&app).iter().all(|row| row.2 == first));

    // `ollama pull` replaced the weights: same name, different digest.
    mock.state.lock().unwrap().digest = "sha256:two".to_string();
    app.rescan_files();
    drain(&mut app).await;
    assert_eq!(documents(&mock).len(), 2);
    let rows = stored(&app);
    assert!(!rows.is_empty());
    assert!(
        rows.iter()
            .all(|row| row.2 != first && row.2.contains("sha256:two")),
        "{rows:?}"
    );
}

#[tokio::test]
async fn switching_to_the_local_model_replaces_old_vectors_lazily() {
    let mock = MockOllama::start(false).await;
    let mut app = app_with_key();
    point_at(&mut app, &mock.endpoint);
    space_with_files(&app);
    app.embedding_model.clear();
    app.rescan_files();
    let id = app
        .files_cache
        .iter()
        .find(|f| f.name == "notes.txt")
        .unwrap()
        .id
        .clone();
    app.db
        .set_file_embeddings(
            &id,
            "old-identity",
            &[("lines 1-1".into(), "x".into())],
            &[(0, vec![1.0, 0.0])],
        )
        .unwrap();

    app.set_setting("embedding_model", "local:embeddinggemma-2:270m")
        .unwrap();
    // Nothing blocks: the switch only started a background probe.
    assert!(app.embed_rx.is_some());
    drain(&mut app).await;
    let rows = stored(&app);
    assert!(!rows.is_empty());
    assert!(rows.iter().all(|row| row.2 != "old-identity"), "{rows:?}");
}

#[tokio::test]
async fn image_ocr_is_automatic_only_when_images_are_not_embedded() {
    let mut app = app_with_key();
    assert!(
        app.automatic_image_ocr(),
        "cloud default keeps OCR for images"
    );

    // Local choice, runtime not probed yet: hold OCR rather than race a paid call.
    app.embedding_model = "local:embeddinggemma-2".to_string();
    assert!(!app.automatic_image_ocr());

    let mock = MockOllama::start(false).await;
    point_at(&mut app, &mock.endpoint);
    app.rescan_files();
    drain(&mut app).await;
    assert!(
        app.automatic_image_ocr(),
        "text-only runtime falls back to OCR"
    );

    let mock = MockOllama::start(true).await;
    point_at(&mut app, &mock.endpoint);
    app.reset_embedding_backend();
    app.rescan_files();
    drain(&mut app).await;
    assert!(!app.automatic_image_ocr());

    app.set_setting("ocr_images", "true").unwrap();
    assert!(app.automatic_image_ocr(), "explicit setting wins");
    app.set_setting("ocr_images", "false").unwrap();
    assert!(!app.automatic_image_ocr());
}

#[tokio::test]
async fn default_is_local_only_when_reachable_and_not_explicitly_configured() {
    let mock = MockOllama::start(false).await;
    let mut app = app_with_key();
    point_at(&mut app, &mock.endpoint);
    assert_eq!(app.embedding_model, "openai/text-embedding-3-small");
    app.discover_embeddings();
    drain(&mut app).await;
    assert_eq!(app.embedding_model, "local:embeddinggemma-2:270m");
    assert!(
        app.db
            .load_settings()
            .unwrap()
            .iter()
            .all(|(k, _)| k != "embedding_model")
    );

    // An explicit choice, even the cloud one, is never overridden.
    let mut explicit = app_with_key();
    point_at(&mut explicit, &mock.endpoint);
    explicit
        .db
        .set_setting("embedding_model", "openai/text-embedding-3-small")
        .unwrap();
    explicit.discover_embeddings();
    assert!(explicit.embed_rx.is_none());
    assert_eq!(explicit.embedding_model, "openai/text-embedding-3-small");

    // Nothing reachable: behave as before.
    let mut unreachable = app_with_key();
    let dead = dead_endpoint().await;
    point_at(&mut unreachable, &dead);
    unreachable.discover_embeddings();
    drain(&mut unreachable).await;
    assert_eq!(unreachable.embedding_model, "openai/text-embedding-3-small");

    // Reachable server without the model installed: behave as before.
    mock.state.lock().unwrap().installed = false;
    let mut missing = app_with_key();
    point_at(&mut missing, &mock.endpoint);
    missing.discover_embeddings();
    drain(&mut missing).await;
    assert_eq!(missing.embedding_model, "openai/text-embedding-3-small");
}

#[tokio::test]
async fn choosing_local_never_falls_back_to_a_cloud_embedder() {
    let mut app = app_with_key();
    let dead = dead_endpoint().await;
    point_at(&mut app, &dead);
    space_with_files(&app);
    app.embedding_model = "local:embeddinggemma-2:270m".to_string();

    let embedder = app.file_embedder().unwrap();
    assert!(embedder.is_ollama());
    assert!(
        embedder.same_route(
            &crate::provider::embedding::Embedder::ollama(&dead, "embeddinggemma-2:270m".into())
                .unwrap()
        )
    );

    app.rescan_files();
    drain(&mut app).await;
    assert_eq!(
        app.embedding_status,
        "embeddings: not reachable — is `ollama serve` running?"
    );
    assert!(stored(&app).is_empty());

    // An unusable endpoint yields no embedder at all, not a cloud one.
    point_at(&mut app, "http://user:secret@127.0.0.1:1");
    assert!(app.file_embedder().is_none());
}

#[tokio::test]
async fn a_corrupt_image_stays_findable_by_name_and_does_not_stall_the_queue() {
    let mock = MockOllama::start(true).await;
    let mut app = app_with_key();
    point_at(&mut app, &mock.endpoint);
    let dir = space_with_files(&app);
    std::fs::write(dir.join("broken.png"), b"\x89PNG\r\n\x1a\ntruncated").unwrap();
    app.embedding_model = "local:embeddinggemma-2".to_string();
    app.rescan_files();
    drain(&mut app).await;
    let rows = stored(&app);
    assert_eq!(
        rows.iter().map(|r| r.0.as_str()).collect::<Vec<_>>(),
        ["beach.png", "broken.png", "notes.txt"]
    );
    assert_eq!(
        rows[1].1, "image",
        "falls back to a vector of its name, as an image hit"
    );
    assert_eq!(
        image_inputs(&mock),
        2,
        "the corrupt file never reaches the runtime as an image"
    );
}
