use super::super::{HostConfig, HostServer, SocketAddr, TcpListener, read_body, read_request_head};
use super::*;
use crate::deck::Deck;
use crate::deck::feedback::{Kind, Request as FeedbackRequest};
use crate::deck::suggest::Selection;
use crate::provider::BackendTag;
use serde_json::{Value, json};
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

async fn send(
    addr: SocketAddr,
    method: &str,
    path: &str,
    body: &[u8],
    auth: bool,
) -> (String, Value) {
    let mut stream = TcpStream::connect(addr).await.unwrap();
    let authorization = if auth {
        "Authorization: Bearer feedback-test\r\n"
    } else {
        ""
    };
    stream.write_all(format!("{method} {path} HTTP/1.1\r\nHost: localhost\r\n{authorization}Content-Length: {}\r\nConnection: close\r\n\r\n", body.len()).as_bytes()).await.unwrap();
    stream.write_all(body).await.unwrap();
    let mut bytes = Vec::new();
    stream.read_to_end(&mut bytes).await.unwrap();
    let split = bytes.windows(4).position(|w| w == b"\r\n\r\n").unwrap();
    (
        String::from_utf8_lossy(&bytes[..split]).into_owned(),
        serde_json::from_slice(&bytes[split + 4..]).unwrap_or(Value::Null),
    )
}
async fn post(addr: SocketAddr, body: &[u8]) -> (String, Value) {
    send(addr, "POST", "/v1/studio/suggest", body, true).await
}
async fn provider(listener: TcpListener, replies: Vec<String>) {
    for (i, reply) in replies.into_iter().enumerate() {
        let (mut stream, _) = listener.accept().await.unwrap();
        let head = read_request_head(&mut stream).await.unwrap();
        let body = read_body(&mut stream, &head.prefix, head.content_length)
            .await
            .unwrap();
        let body: Value = serde_json::from_slice(&body).unwrap();
        assert!(
            body["messages"][0]["content"]
                .as_str()
                .unwrap()
                .contains("## Commenting principles")
        );
        if i == 1 {
            assert!(
                body["messages"][3]["content"]
                    .as_str()
                    .unwrap()
                    .contains("Correct these errors")
            );
        }
        let response = json!({"choices":[{"message":{"content":reply}}],"usage":{"prompt_tokens":10,"completion_tokens":4,"cost":0.01}}).to_string();
        stream.write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{response}", response.len()).as_bytes()).await.unwrap();
    }
}
async fn exercise(kind: Kind, replies: Vec<String>) -> Value {
    let rounds = replies.len();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}/v1", listener.local_addr().unwrap());
    let mock = tokio::spawn(provider(listener, replies));
    let root = std::env::temp_dir().join(format!("feedback-host-{}", uuid::Uuid::new_v4()));
    let mut app = App::new(
        crate::db::Db::open_in_memory().unwrap(),
        None,
        crate::space::Space { root: root.clone() },
    );
    app.backends.set(
        BackendTag::Local,
        openrouter::OpenRouter::local(&base).unwrap(),
    );
    app.current_model = Some("local:mock-feedback".into());
    let mut server = HostServer::bind(app, HostConfig::new(0, "feedback-test"))
        .await
        .unwrap();
    let deck = Deck::new_from_layouts("Story");
    let req = FeedbackRequest {
        kind,
        revision: deck.revision(),
        deck,
        selection: Selection::default(),
        anchor: None,
        messages: vec![],
        instruction: "Review".into(),
        model: None,
    };
    let request_body = serde_json::to_vec(&req).unwrap();
    let (unauthorized, _) = send(
        server.local_addr(),
        "POST",
        "/v1/studio/suggest",
        &request_body,
        false,
    )
    .await;
    assert!(unauthorized.starts_with("HTTP/1.1 401"));
    let (headers, body) = post(server.local_addr(), &request_body).await;
    assert!(headers.starts_with("HTTP/1.1 200"), "{headers} {body}");
    assert_eq!(body["model"], "local:mock-feedback");
    let (_, usage) = send(server.local_addr(), "GET", "/v1/usage", &[], true).await;
    assert_eq!(usage["totals"]["requests"], rounds);
    assert_eq!(usage["totals"]["prompt_tokens"], rounds * 10);
    mock.await.unwrap();
    server.shutdown().await;
    let _ = std::fs::remove_dir_all(root);
    body
}
fn good() -> Value {
    json!({"anchor":{"slide":"s1","block":"b1","x":3,"y":6},"severity":"should","text":"Shorten the title so it is easier to scan.","edit":{"why":"Clarify hierarchy","ops":[{"op":"update_block","slide":"s1","id":"b1","fields":{"text":"A sharper story"}}]}})
}

#[tokio::test]
async fn review_repairs_unknown_anchors_once_and_fills_hashes() {
    let mut bad = good();
    bad["anchor"]["block"] = json!("missing");
    let body = exercise(
        Kind::Review,
        vec![
            json!({"text":"Review","comments":[bad]}).to_string(),
            format!(
                "```json\n{}\n```\nDone",
                json!({"text":"Review","comments":[good()]})
            ),
        ],
    )
    .await;
    assert_eq!(body["comments"].as_array().unwrap().len(), 1);
    assert_eq!(
        body["comments"][0]["edit"]["ops"][0]["base_hash"]
            .as_str()
            .unwrap()
            .len(),
        64
    );
    assert!(body["notes"].as_array().unwrap().is_empty());
}
#[tokio::test]
async fn second_round_drops_only_invalid_siblings_with_located_note() {
    let mut bad = good();
    bad["severity"] = json!("critical");
    let reply = json!({"text":"Review","comments":[good(),bad]}).to_string();
    let body = exercise(Kind::Review, vec![reply.clone(), reply]).await;
    assert_eq!(body["comments"].as_array().unwrap().len(), 1);
    assert!(body["notes"][0].as_str().unwrap().contains("comments[1]"));
}
#[tokio::test]
async fn malformed_chat_is_repaired_into_two_options() {
    let edit = good()["edit"].clone();
    let body = exercise(
        Kind::Chat,
        vec![
            "not JSON".into(),
            json!({"text":"Two directions","options":[edit,good()["edit"]]}).to_string(),
        ],
    )
    .await;
    assert_eq!(body["options"].as_array().unwrap().len(), 2);
    assert!(body["comments"].as_array().unwrap().is_empty());
}

#[tokio::test]
async fn a_request_without_a_kind_or_with_a_stale_revision_is_rejected_before_inference() {
    // No provider is configured: both must be refused before any model call.
    let root = std::env::temp_dir().join(format!("feedback-host-{}", uuid::Uuid::new_v4()));
    let app = App::new(
        crate::db::Db::open_in_memory().unwrap(),
        None,
        crate::space::Space { root: root.clone() },
    );
    let mut server = HostServer::bind(app, HostConfig::new(0, "feedback-test"))
        .await
        .unwrap();
    let deck = Deck::new_from_layouts("Story");
    let mut kindless = json!({
        "deck": deck, "revision": deck.revision(), "selection": {},
        "instruction": "Tighten", "model": null
    });
    let (headers, _) = post(server.local_addr(), kindless.to_string().as_bytes()).await;
    assert!(headers.starts_with("HTTP/1.1 400"), "{headers}");
    kindless["kind"] = json!("chat");
    kindless["anchor"] = Value::Null;
    kindless["revision"] = json!("stale");
    let (headers, body) = post(server.local_addr(), kindless.to_string().as_bytes()).await;
    assert!(headers.starts_with("HTTP/1.1 400"), "{headers}");
    assert_eq!(body["error"], "invalid feedback request");
    server.shutdown().await;
    let _ = std::fs::remove_dir_all(root);
}
