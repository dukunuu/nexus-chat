use super::super::{HostConfig, HostServer, SocketAddr, TcpListener, read_body, read_request_head};
use super::*;
use crate::deck::{Deck, suggest::Selection};
use crate::provider::BackendTag;
use serde_json::{Value, json};
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

async fn request(
    addr: SocketAddr,
    method: &str,
    path: &str,
    body: &[u8],
    auth: bool,
) -> (String, Value) {
    let mut stream = TcpStream::connect(addr).await.unwrap();
    let authorization = if auth {
        "Authorization: Bearer studio-test\r\n"
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

async fn exercise(replies: Vec<String>, status: &str, valid: bool, with_usage: bool) -> Value {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}/v1", listener.local_addr().unwrap());
    let rounds = replies.len();
    let mock = tokio::spawn(async move {
        for (round, reply) in replies.into_iter().enumerate() {
            let (mut stream, _) = listener.accept().await.unwrap();
            let head = read_request_head(&mut stream).await.ok().unwrap();
            let body = read_body(&mut stream, &head.prefix, head.content_length)
                .await
                .ok()
                .unwrap();
            let body: Value = serde_json::from_slice(&body).unwrap();
            assert_eq!(body["model"], "mock-deck");
            assert!(head.headers.get("authorization").is_none());
            assert!(
                body["messages"][0]["content"]
                    .as_str()
                    .unwrap()
                    .contains("32×18")
            );
            if round == 1 {
                assert_eq!(body["messages"][2]["role"], "assistant");
                assert!(
                    body["messages"][3]["content"]
                        .as_str()
                        .unwrap()
                        .contains("Correct these errors")
                );
            }
            let mut response = json!({"choices":[{"message":{"content":reply}}]});
            if with_usage {
                response["usage"] = json!({"prompt_tokens":10,"completion_tokens":4,"cost":0.01});
            }
            let response = response.to_string();
            stream.write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{response}", response.len()).as_bytes()).await.unwrap();
        }
    });
    let root = std::env::temp_dir().join(format!("nexus-studio-host-{}", uuid::Uuid::new_v4()));
    let mut app = App::new(
        crate::db::Db::open_in_memory().unwrap(),
        None,
        crate::space::Space { root: root.clone() },
    );
    app.backends.set(
        BackendTag::Local,
        openrouter::OpenRouter::local(&base).unwrap(),
    );
    app.current_model = Some("local:mock-deck".into());
    let mut server = HostServer::bind(app, HostConfig::new(0, "studio-test"))
        .await
        .unwrap();
    let deck = Deck::new_from_layouts("Story");
    let request_body = serde_json::to_vec(&SuggestRequest {
        revision: deck.revision(),
        deck,
        selection: Selection::default(),
        instruction: "Tighten".into(),
        model: None,
    })
    .unwrap();
    let (unauthorized, _) = request(
        server.local_addr(),
        "POST",
        "/v1/studio/suggest",
        &request_body,
        false,
    )
    .await;
    assert!(unauthorized.starts_with("HTTP/1.1 401"));
    let (headers, body) = request(
        server.local_addr(),
        "POST",
        "/v1/studio/suggest",
        &request_body,
        true,
    )
    .await;
    assert!(headers.starts_with(status), "{headers} {body}");
    assert_eq!(body["model"], "local:mock-deck");
    if valid {
        assert_eq!(
            body["suggestions"][0]["ops"][0]["fields"]["text"],
            "Sharper story"
        );
        assert_eq!(
            body["suggestions"][0]["ops"][0]["base_hash"]
                .as_str()
                .unwrap()
                .len(),
            64
        );
    } else {
        assert_eq!(body["notes"][0], "dropped 1 suggestion that didn't apply");
    }
    let (_, usage) = request(server.local_addr(), "GET", "/v1/usage", &[], true).await;
    assert_eq!(usage["totals"]["requests"], rounds);
    assert_eq!(
        usage["totals"]["prompt_tokens"],
        if with_usage { rounds * 10 } else { 0 }
    );
    mock.await.unwrap();
    server.shutdown().await;
    let _ = std::fs::remove_dir_all(root);
    body
}

fn good() -> String {
    json!({"suggestions":[{"why":"Tighten the headline","ops":[{"op":"update_block","slide":"s1","id":"b1","fields":{"text":"Sharper story"}}]}]}).to_string()
}
fn bad() -> String {
    json!({"suggestions":[{"why":"Unknown block","ops":[{"op":"update_block","slide":"s1","id":"b9","fields":{"text":"No"}}]}]}).to_string()
}

#[tokio::test]
async fn authorized_suggestions_fences_and_usage() {
    exercise(
        vec![format!("```json\n{}\n```\nDone.", good())],
        "HTTP/1.1 200",
        true,
        true,
    )
    .await;
}
#[tokio::test]
async fn repairs_once_with_located_errors() {
    exercise(vec![bad(), good()], "HTTP/1.1 200", true, true).await;
}
#[tokio::test]
async fn still_bad_is_422_with_notes() {
    exercise(vec![bad(), bad()], "HTTP/1.1 422", false, true).await;
}

#[tokio::test]
async fn malformed_json_is_repaired_and_missing_usage_still_counts_the_calls() {
    exercise(vec!["not JSON".into(), good()], "HTTP/1.1 200", true, false).await;
}

#[tokio::test]
async fn partial_repair_keeps_valid_independent_suggestions() {
    let mut mixed: Value = serde_json::from_str(&good()).unwrap();
    let bad: Value = serde_json::from_str(&bad()).unwrap();
    mixed["suggestions"]
        .as_array_mut()
        .unwrap()
        .push(bad["suggestions"][0].clone());
    let reply = exercise(
        vec![mixed.to_string(), mixed.to_string()],
        "HTTP/1.1 200",
        true,
        true,
    )
    .await;
    assert_eq!(reply["suggestions"].as_array().unwrap().len(), 1);
    assert_eq!(reply["notes"][0], "dropped 1 suggestion that didn't apply");
}
