use super::*;
use crate::backend::Command;
use nexus_core::deck::suggest::{RawSuggestion, prepare};
use nexus_core::deck::{Deck, Op};
use serde_json::{Value, json};
use std::fs;
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use tokio::net::TcpListener;

struct Temp(PathBuf);
impl Temp {
    fn new() -> Self {
        Self(std::env::temp_dir().join(format!("studio-suggest-{}", Uuid::new_v4())))
    }
    fn backend(&self) -> Backend {
        Backend::start(None, &self.0.join("files"), &self.0.join("data")).unwrap()
    }
}
impl Drop for Temp {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn add_history(backend: &mut Backend, operations: Vec<Value>) -> Vec<String> {
    let raw = operations
        .into_iter()
        .map(|ops| RawSuggestion {
            why: "Tighter".into(),
            ops: ops.as_array().unwrap().clone(),
        })
        .collect();
    let (suggestions, errors) = prepare(&backend.deck, raw);
    assert!(errors.is_empty(), "{errors:?}");
    let ids = suggestions.iter().map(|s| s.id.clone()).collect();
    let response = SuggestResponse {
        suggestions,
        model: "mock".into(),
        revision: backend.deck.revision(),
        notes: vec![],
    };
    assert!(matches!(
        backend.complete(Completed {
            document: backend.document,
            suggestion: true,
            result: Ok(Outcome::Suggestions {
                response,
                instruction: "Tighten".into()
            })
        }),
        Event::Suggestions { .. }
    ));
    ids
}
fn edit(text: &str) -> Value {
    json!([{"op":"update_block","slide":"s1","id":"b1","fields":{"text":text}}])
}

#[test]
fn preview_is_read_only_accept_is_one_undo_and_conflicts_are_outdated() {
    let temp = Temp::new();
    let mut backend = temp.backend();
    let original = backend.deck.clone();
    let disk = fs::read(&backend.path).unwrap();
    let ids = add_history(&mut backend, vec![edit("Better"), edit("Alternative")]);
    assert!(
        matches!(backend.preview(&ids[0]).unwrap(), Event::Preview { deck } if deck != original)
    );
    assert_eq!(backend.deck, original);
    assert_eq!(fs::read(&backend.path).unwrap(), disk);
    assert!(backend.undo.is_empty());
    backend.accept(&ids[0]).unwrap();
    assert_eq!(backend.undo.len(), 1);
    assert_eq!(backend.entry(&ids[0]).unwrap().status, Status::Accepted);
    assert_eq!(backend.entry(&ids[1]).unwrap().status, Status::Outdated);
    assert_eq!(backend.accept(&ids[1]).unwrap_err().to_string(), "outdated");
    backend.execute(Command::Undo {}).unwrap();
    assert_eq!(backend.deck, original);
    let ids = add_history(&mut backend, vec![edit("New")]);
    let op = Op::UpdateBlock {
        slide: "s1".into(),
        id: "b1".into(),
        fields: json!({"text":"Manual"}).as_object().unwrap().clone(),
        base_hash: backend.deck.slides[0].blocks[0].hash(),
    };
    backend.execute(Command::Apply { ops: vec![op] }).unwrap();
    assert_eq!(backend.entry(&ids[0]).unwrap().status, Status::Outdated);
}

#[test]
fn accept_all_is_ordered_and_history_round_trips_and_caps() {
    let temp = Temp::new();
    let mut backend = temp.backend();
    let ids = add_history(
        &mut backend,
        vec![
            edit("First"),
            edit("Second"),
            json!([{"op":"set_notes","slide":"s2","notes":"Independent"}]),
        ],
    );
    backend.accept_all().unwrap();
    assert_eq!(backend.entry(&ids[0]).unwrap().status, Status::Accepted);
    assert_eq!(backend.entry(&ids[1]).unwrap().status, Status::Outdated);
    assert_eq!(backend.entry(&ids[2]).unwrap().status, Status::Accepted);
    assert_eq!(backend.deck.slides[1].notes, "Independent");
    for i in 0..52 {
        add_history(
            &mut backend,
            vec![json!([{"op":"set_notes","slide":"s1","notes":i.to_string()}])],
        );
    }
    assert_eq!(backend.history.len(), 50);
    let history = serde_json::to_value(&backend.history).unwrap();
    let reopened = temp.backend();
    assert_eq!(serde_json::to_value(&reopened.history).unwrap(), history);
    assert_eq!(reopened.history.last().unwrap().instruction, "Tighten");
    assert!(
        fs::read_dir(backend.path.parent().unwrap())
            .unwrap()
            .all(|e| !e.unwrap().file_name().to_string_lossy().ends_with(".tmp"))
    );
}

async fn read_http(stream: &mut tokio::net::TcpStream) -> (String, Vec<u8>) {
    let mut bytes = Vec::new();
    let split = loop {
        if let Some(split) = bytes.windows(4).position(|w| w == b"\r\n\r\n") {
            break split;
        }
        let mut chunk = [0; 1024];
        let n = stream.read(&mut chunk).await.unwrap();
        assert!(n > 0);
        bytes.extend_from_slice(&chunk[..n]);
    };
    let headers = String::from_utf8(bytes[..split].to_vec()).unwrap();
    let size = headers
        .lines()
        .find_map(|line| {
            line.to_ascii_lowercase()
                .strip_prefix("content-length: ")
                .and_then(|s| s.parse::<usize>().ok())
        })
        .unwrap_or(0);
    while bytes.len() < split + 4 + size {
        let mut chunk = [0; 1024];
        let n = stream.read(&mut chunk).await.unwrap();
        assert!(n > 0);
        bytes.extend_from_slice(&chunk[..n]);
    }
    (headers, bytes[split + 4..].to_vec())
}
async fn respond(stream: &mut tokio::net::TcpStream, body: Value) {
    let body = body.to_string();
    stream.write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).as_bytes()).await.unwrap();
}

#[tokio::test]
async fn discovery_mismatch_never_sends_the_token_or_posts() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let connection = host::Connection::new(
        &format!("http://{}", listener.local_addr().unwrap()),
        "private-token".into(),
    )
    .unwrap();
    let mock = tokio::spawn(async move {
        for _ in 0..2 {
            let (mut stream, _) = listener.accept().await.unwrap();
            let (headers, _) = read_http(&mut stream).await;
            assert!(headers.starts_with("GET /.well-known/nexus "));
            assert!(!headers.to_ascii_lowercase().contains("authorization"));
            assert!(!headers.contains("private-token"));
            respond(
                &mut stream,
                json!({"product":"nexus-chat","host_id":"wrong"}),
            )
            .await;
        }
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(100), listener.accept())
                .await
                .is_err()
        );
    });
    assert_eq!(connection.status().await.state, "mismatch");
    let deck = Deck::new_from_layouts("Story");
    let req = SuggestRequest {
        revision: deck.revision(),
        deck,
        selection: Selection::default(),
        instruction: String::new(),
        model: None,
    };
    let error = connection.suggest(&req).await.unwrap_err().to_string();
    assert!(error.contains("identity mismatch"));
    assert!(!error.contains("private-token"));
    mock.await.unwrap();
}

#[tokio::test]
async fn suggest_runs_as_work_and_delivers_events_and_history() {
    let temp = Temp::new();
    let mut backend = temp.backend();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    backend.configure_host(
        host::Connection::new(
            &format!("http://{}", listener.local_addr().unwrap()),
            "private-token".into(),
        )
        .unwrap(),
    );
    let mock = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        let (headers, _) = read_http(&mut stream).await;
        assert!(!headers.to_ascii_lowercase().contains("authorization"));
        respond(&mut stream, json!({"product":"nexus-chat","host_id":nexus_core::host::host_discovery_id("private-token")})).await;
        let (mut stream, _) = listener.accept().await.unwrap();
        let (headers, bytes) = read_http(&mut stream).await;
        assert!(headers.starts_with("POST /v1/studio/suggest "));
        assert!(headers.contains("Bearer private-token"));
        let req: SuggestRequest = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(req.instruction, "Tighten");
        let (suggestions, _) = prepare(
            &req.deck,
            vec![RawSuggestion {
                why: "Tighter".into(),
                ops: edit("Better").as_array().unwrap().clone(),
            }],
        );
        respond(
            &mut stream,
            json!(SuggestResponse {
                suggestions,
                revision: req.revision,
                model: "mock".into(),
                notes: vec![]
            }),
        )
        .await;
    });
    let event = backend.dispatch(r#"{"command":"Suggest","instruction":"Tighten","selection":{}}"#);
    assert!(matches!(event, Event::Suggesting {}));
    assert!(backend.undo.is_empty());
    let result = backend.take_work().unwrap().run().await;
    let event = backend.complete(result);
    assert!(matches!(event, Event::Suggestions { .. }), "{event:?}");
    assert!(!event.script().contains("private-token"));
    assert_eq!(backend.history.len(), 1);
    assert_eq!(backend.history[0].suggestions[0].status, Status::Pending);
    mock.await.unwrap();
}
