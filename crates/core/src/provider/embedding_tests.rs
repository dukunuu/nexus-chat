use super::*;
use std::sync::Mutex;
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

pub(crate) struct MockState {
    pub digest: String,
    pub native: bool,
    pub vision: bool,
    pub image_handler: bool,
    pub installed: bool,
    pub requests: Vec<(String, Value)>,
}

pub(crate) struct MockOllama {
    pub endpoint: String,
    pub state: Arc<Mutex<MockState>>,
    task: tokio::task::JoinHandle<()>,
}

impl Drop for MockOllama {
    fn drop(&mut self) {
        self.task.abort();
    }
}

impl MockOllama {
    pub async fn start(vision: bool) -> Self {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = format!("http://{}", listener.local_addr().unwrap());
        let state = Arc::new(Mutex::new(MockState {
            digest: "sha256:one".to_string(),
            native: true,
            vision,
            image_handler: vision,
            installed: true,
            requests: Vec::new(),
        }));
        let shared = state.clone();
        let task = tokio::spawn(async move {
            loop {
                let Ok((mut stream, _)) = listener.accept().await else {
                    break;
                };
                let mut bytes = Vec::new();
                let header_end = loop {
                    let mut part = [0; 4096];
                    let n = stream.read(&mut part).await.unwrap();
                    if n == 0 {
                        return;
                    }
                    bytes.extend_from_slice(&part[..n]);
                    if let Some(i) = bytes.windows(4).position(|b| b == b"\r\n\r\n") {
                        break i + 4;
                    }
                };
                let headers = String::from_utf8_lossy(&bytes[..header_end]).to_string();
                assert!(!headers.to_lowercase().contains("authorization:"));
                let length = headers
                    .lines()
                    .find_map(|line| {
                        let (name, value) = line.split_once(':')?;
                        name.eq_ignore_ascii_case("content-length")
                            .then(|| value.trim().parse::<usize>().unwrap())
                    })
                    .unwrap_or(0);
                while bytes.len() < header_end + length {
                    let mut part = [0; 4096];
                    let n = stream.read(&mut part).await.unwrap();
                    if n == 0 {
                        return;
                    }
                    bytes.extend_from_slice(&part[..n]);
                }
                let path = headers.split_whitespace().nth(1).unwrap().to_string();
                let body = if length == 0 {
                    Value::Null
                } else {
                    serde_json::from_slice(&bytes[header_end..header_end + length]).unwrap()
                };
                let (status, response) = {
                    let mut state = shared.lock().unwrap();
                    state.requests.push((path.clone(), body.clone()));
                    match path.as_str() {
                        "/api/tags" => (
                            200,
                            json!({"models": if state.installed {
                            vec![json!({"name":"embeddinggemma-2:270m", "digest":state.digest}), json!({"name":"embeddinggemma-2:latest", "digest":state.digest})]
                        } else { Vec::new() }}),
                        ),
                        "/api/show" => (
                            200,
                            json!({"capabilities": if state.vision { vec!["embedding", "vision"] } else { vec!["embedding"] }}),
                        ),
                        "/api/embed" if !state.native => (404, json!({"error":"unsupported"})),
                        "/api/embed" | "/v1/embeddings" => {
                            let inputs = body["input"].as_array().unwrap();
                            if inputs.iter().any(|i| i.get("image").is_some())
                                && !state.image_handler
                            {
                                (400, json!({"error":"image input not supported"}))
                            } else {
                                let vectors = inputs
                                    .iter()
                                    .map(|input| {
                                        let mut v = vec![0.0_f32; 768];
                                        if input.get("image").is_some() {
                                            v[0] = 0.9;
                                            v[1] = 0.1;
                                        } else if input.as_str().unwrap().contains("financial") {
                                            v[1] = 1.0;
                                        } else {
                                            v[0] = 1.0;
                                        }
                                        v
                                    })
                                    .collect::<Vec<_>>();
                                if path == "/api/embed" {
                                    (200, json!({"embeddings":vectors}))
                                } else {
                                    (
                                        200,
                                        json!({"data": vectors.iter().enumerate().rev().map(|(index, embedding)| json!({"index":index,"embedding":embedding})).collect::<Vec<_>>()}),
                                    )
                                }
                            }
                        }
                        _ => (404, json!({"error":"unknown endpoint"})),
                    }
                };
                let body = response.to_string();
                let response = format!(
                    "HTTP/1.1 {status} OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                if stream.write_all(response.as_bytes()).await.is_err() {
                    return;
                }
            }
        });
        Self {
            endpoint,
            state,
            task,
        }
    }
}

#[tokio::test]
async fn native_probe_prefixes_images_and_digest() {
    let mock = MockOllama::start(true).await;
    let embedder =
        Embedder::ollama(&format!("{}/v1", mock.endpoint), "embeddinggemma-2".into()).unwrap();
    embedder.prepare().await.unwrap();
    assert!(embedder.supports_images());
    assert_eq!(embedder.identity().unwrap().dimension, 768);
    assert_eq!(
        embedder.identity().unwrap().revision.as_deref(),
        Some("sha256:one")
    );
    let vectors = embedder
        .embed(vec![
            embedder.document("notes.txt", "financial report"),
            probe_image().unwrap(),
        ])
        .await
        .unwrap();
    assert_eq!(vectors.len(), 2);
    assert!((vectors[1].iter().map(|x| x * x).sum::<f32>() - 1.0).abs() < 1e-5);
    embedder.query("beach").await.unwrap();
    let state = mock.state.lock().unwrap();
    let requests = state
        .requests
        .iter()
        .filter(|(p, _)| p == "/api/embed")
        .map(|(_, b)| b)
        .collect::<Vec<_>>();
    assert_eq!(
        requests[2]["input"][0],
        "title: notes.txt | text: financial report"
    );
    assert_eq!(
        requests[3]["input"][0],
        "task: search result | query: beach"
    );
    assert_eq!(requests[2]["truncate"], false);
    assert_eq!(requests[2]["dimensions"], 768);
    let image = &requests[2]["input"][1];
    assert_eq!(image.as_object().unwrap().len(), 1);
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(image["image"].as_str().unwrap())
        .unwrap();
    assert_eq!(crate::extract::vision_mime(&bytes), Some("image/png"));
}

#[tokio::test]
async fn text_fallback_and_capability_probe_not_catalog_label() {
    let mock = MockOllama::start(true).await;
    mock.state.lock().unwrap().image_handler = false;
    let embedder = Embedder::ollama(&mock.endpoint, "embeddinggemma-2".into()).unwrap();
    embedder.prepare().await.unwrap();
    assert!(!embedder.supports_images());
    assert!(
        embedder
            .embed(vec![probe_image().unwrap()])
            .await
            .unwrap_err()
            .to_string()
            .contains("unsupported")
    );
    mock.state.lock().unwrap().native = false;
    let text = Embedder::ollama(&mock.endpoint, "embeddinggemma-2:270m".into()).unwrap();
    let vectors = text
        .embed(vec![
            text.document("a", "beach"),
            text.document("b", "financial"),
        ])
        .await
        .unwrap();
    assert_eq!(vectors[0][0], 1.0);
    assert_eq!(vectors[1][1], 1.0);
    assert!(!text.supports_images());
    assert!(
        mock.state
            .lock()
            .unwrap()
            .requests
            .iter()
            .any(|(p, _)| p == "/v1/embeddings")
    );
}

#[test]
fn strict_validation_honors_indices_and_rejects_invalid_values() {
    let body = json!({"data":[{"index":1,"embedding":[0,2]},{"index":0,"embedding":[3,0]}]});
    assert_eq!(
        indexed_vectors(&body, 2, Some(2)).unwrap(),
        vec![vec![1.0, 0.0], vec![0.0, 1.0]]
    );
    for invalid in [
        json!({"data":[]}),
        json!({"data":[{"index":0,"embedding":[1,0]}]}),
        json!({"data":[{"index":0,"embedding":[1,0]},{"index":0,"embedding":[1,0]}]}),
        json!({"data":[{"index":2,"embedding":[1,0]},{"index":0,"embedding":[1,0]}]}),
        json!({"data":[{"embedding":[1,0]},{"index":0,"embedding":[1,0]}]}),
        json!({"data":[{"index":0,"embedding":[]},{"index":1,"embedding":[]}]}),
        json!({"data":[{"index":0,"embedding":[1]},{"index":1,"embedding":[1]}]}),
        json!({"data":[{"index":0,"embedding":["NaN",1]},{"index":1,"embedding":[0,1]}]}),
        json!({"data":[{"index":0,"embedding":[null,1]},{"index":1,"embedding":[0,1]}]}),
    ] {
        assert!(indexed_vectors(&invalid, 2, Some(2)).is_err(), "{invalid}");
    }
    assert!(validate_vectors(vec![vec![f32::NAN, 1.0]], 1, Some(2)).is_err());
    assert!(validate_vectors(vec![vec![f32::INFINITY, 1.0]], 1, Some(2)).is_err());
    assert!(validate_vectors(vec![vec![0.0, 0.0]], 1, Some(2)).is_err());
    assert!(parse_vector(&json!([1e300])).is_err());
    assert!(serde_json::from_str::<Value>("{\"embeddings\":[[NaN]]}").is_err());
}

#[tokio::test]
async fn discovery_requires_installed_reachable_model_and_valid_probe() {
    let mock = MockOllama::start(false).await;
    let (model, _) = discover(&mock.endpoint).await.unwrap();
    assert_eq!(model, "local:embeddinggemma-2:270m");
    mock.state.lock().unwrap().installed = false;
    assert!(discover(&mock.endpoint).await.is_none());
    let endpoint = mock.endpoint.clone();
    drop(mock);
    assert!(discover(&endpoint).await.is_none());
}

#[tokio::test]
async fn image_preprocessing_is_bounded_and_mime_checked() {
    let root = std::env::temp_dir().join(format!("nexus-embed-image-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&root).unwrap();
    let path = root.join("photo.png");
    image::RgbImage::new(1600, 1000).save(&path).unwrap();
    let input = image_input(&path).unwrap();
    let EmbedInput::Image { bytes, mime } = &input else {
        panic!()
    };
    assert_eq!(mime, "image/png");
    let decoded = image::load_from_memory(bytes).unwrap();
    assert_eq!(decoded.width(), 768);
    assert!(decoded.height() <= 768);
    let mock = MockOllama::start(true).await;
    let embedder = Embedder::ollama(&mock.endpoint, "embeddinggemma-2".into()).unwrap();
    assert!(
        embedder
            .embed(vec![EmbedInput::Image {
                bytes: bytes.clone(),
                mime: "image/jpeg".into()
            }])
            .await
            .unwrap_err()
            .to_string()
            .contains("MIME")
    );
    std::fs::File::create(&path)
        .unwrap()
        .set_len(21 * 1024 * 1024)
        .unwrap();
    assert!(
        image_input(&path)
            .unwrap_err()
            .to_string()
            .contains("20 MiB")
    );
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn over_budget_text_is_rejected_without_a_request() {
    let mock = MockOllama::start(false).await;
    let embedder = Embedder::ollama(&mock.endpoint, "embeddinggemma-2:270m".into()).unwrap();
    embedder.prepare().await.unwrap();
    let before = mock.state.lock().unwrap().requests.len();
    let huge = EmbedInput::Text("x".repeat(8192));
    let error = embedder.embed(vec![huge]).await.unwrap_err().to_string();
    assert!(error.contains("8192-token budget"), "{error}");
    assert_eq!(mock.state.lock().unwrap().requests.len(), before);
}

#[tokio::test]
async fn a_missing_model_says_how_to_install_it() {
    let mock = MockOllama::start(false).await;
    mock.state.lock().unwrap().installed = false;
    let embedder = Embedder::ollama(&mock.endpoint, "embeddinggemma-2:270m".into()).unwrap();
    let error = embedder.prepare().await.unwrap_err().to_string();
    assert!(
        error.contains("ollama pull embeddinggemma-2:270m"),
        "{error}"
    );

    let endpoint = {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        format!("http://{}", listener.local_addr().unwrap())
    };
    let down = Embedder::ollama(&endpoint, "embeddinggemma-2:270m".into()).unwrap();
    assert_eq!(
        down.prepare().await.unwrap_err().to_string(),
        "not reachable — is `ollama serve` running?"
    );
    assert!(down.identity().is_none());
}

#[test]
fn every_identity_field_changes_the_cache_key() {
    let base = EmbedIdentity {
        backend: "ollama:http://127.0.0.1:11434".into(),
        model: "embeddinggemma-2:270m".into(),
        revision: Some("sha256:one".into()),
        dimension: 768,
        prefix_scheme: "gemma-search-v1".into(),
        preprocessing_version: "bytes-1000-overlap-100;png-768-v1;images=false".into(),
    };
    let variants = [
        EmbedIdentity {
            backend: "ollama:http://127.0.0.1:1".into(),
            ..base.clone()
        },
        EmbedIdentity {
            model: "embeddinggemma-2".into(),
            ..base.clone()
        },
        EmbedIdentity {
            revision: Some("sha256:two".into()),
            ..base.clone()
        },
        EmbedIdentity {
            revision: None,
            ..base.clone()
        },
        EmbedIdentity {
            dimension: 512,
            ..base.clone()
        },
        EmbedIdentity {
            prefix_scheme: "file-v1".into(),
            ..base.clone()
        },
        EmbedIdentity {
            preprocessing_version: "bytes-1000-overlap-100;png-768-v1;images=true".into(),
            ..base.clone()
        },
    ];
    for variant in variants {
        assert_ne!(variant.key(), base.key());
    }
    assert_eq!(base.clone().key(), base.key());
}

#[test]
fn prefixes_belong_to_the_gemma_family_only() {
    let provider = OpenRouter::openai("sk-test".into());
    let other = Embedder::openai(provider, "openai/text-embedding-3-small".into());
    let EmbedInput::Text(doc) = other.document("a.txt", "body") else {
        panic!()
    };
    assert_eq!(doc, "file: a.txt\nbody");
    assert_eq!(other.query_text("q"), "q");
    assert_eq!(other.identity().unwrap().dimension, 1536);
    let gemma = Embedder::ollama("http://127.0.0.1:11434", "embeddinggemma-2:270m".into()).unwrap();
    let EmbedInput::Text(doc) = gemma.document("a.txt", "body") else {
        panic!()
    };
    assert_eq!(doc, "title: a.txt | text: body");
    assert_eq!(gemma.query_text("q"), "task: search result | query: q");
}

#[test]
fn positional_responses_are_accepted_but_mixed_indices_are_not() {
    let positional = json!({"data":[{"embedding":[3,0]},{"embedding":[0,2]}]});
    assert_eq!(
        indexed_vectors(&positional, 2, Some(2)).unwrap(),
        vec![vec![1.0, 0.0], vec![0.0, 1.0]]
    );
    let mixed = json!({"data":[{"embedding":[3,0]},{"index":0,"embedding":[0,2]}]});
    assert!(indexed_vectors(&mixed, 2, Some(2)).is_err());
}
