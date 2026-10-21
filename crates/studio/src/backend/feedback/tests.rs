use super::*;
use crate::backend::{Command, host};
use serde_json::json;
use std::fs;
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

struct Temp(PathBuf);
impl Temp {
    fn new() -> Self {
        Self(std::env::temp_dir().join(format!("studio-feedback-{}", Uuid::new_v4())))
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
fn anchor() -> Anchor {
    Anchor {
        slide: "s1".into(),
        block: Some("b1".into()),
        x: 3,
        y: 6,
    }
}
fn request(backend: &Backend, kind: Kind) -> feedback::Request {
    feedback::Request {
        kind,
        deck: backend.deck.clone(),
        revision: backend.deck.revision(),
        selection: Selection::default(),
        anchor: Some(anchor()),
        messages: vec![],
        instruction: String::new(),
        model: None,
    }
}
fn response(req: &feedback::Request, options: bool) -> feedback::Response {
    let edit = json!({"why":"A clearer title","ops":[{"op":"update_block","slide":"s1","id":"b1","fields":{"text":"A clearer title"}}]});
    let value = if options {
        json!({"text":"Two directions","options":[edit,edit]})
    } else {
        json!({"text":"Clarify the title so the hierarchy reads better.","edit":edit})
    };
    let (response, errors) = feedback::prepare(req, &value.to_string());
    assert!(errors.is_empty(), "{errors:?}");
    response
}

#[test]
fn thread_lifecycle_persists_and_ignores_s2_sidecars() {
    let temp = Temp::new();
    let mut backend = temp.backend();
    backend
        .comment_create(anchor(), "A note to myself".into(), false)
        .unwrap();
    let id = backend.feedback.threads[0].id.clone();
    backend
        .comment_reply(&id, "One more thought".into(), false)
        .unwrap();
    backend.comment_resolve(&id, true).unwrap();
    assert!(backend.feedback.threads[0].resolved.is_some());
    backend.comment_resolve(&id, false).unwrap();
    assert!(backend.feedback.threads[0].resolved.is_none());
    fs::write(backend.sidecar(".suggestions.json"), b"not valid JSON").unwrap();
    let reopened = temp.backend();
    assert_eq!(reopened.feedback.threads[0].messages.len(), 2);
    assert_eq!(
        reopened.feedback.threads[0].messages[1].text,
        "One more thought"
    );
    assert_eq!(
        fs::read(reopened.sidecar(".suggestions.json")).unwrap(),
        b"not valid JSON"
    );
    assert!(
        fs::read_dir(backend.path.parent().unwrap())
            .unwrap()
            .all(|e| !e.unwrap().file_name().to_string_lossy().ends_with(".tmp"))
    );
}

#[test]
fn preview_is_read_only_apply_is_one_undo_and_conflicts_are_outdated() {
    let temp = Temp::new();
    let mut backend = temp.backend();
    backend
        .comment_create(anchor(), "Make the headline clearer".into(), false)
        .unwrap();
    let id = backend.feedback.threads[0].id.clone();
    let req = request(&backend, Kind::Thread);
    backend
        .receive_feedback(&req, Some(id.clone()), response(&req, false))
        .unwrap();
    let edit = backend.feedback.threads[0].messages[1].edits[0]
        .proposal
        .id
        .clone();
    let original = backend.deck.clone();
    let disk = fs::read(&backend.path).unwrap();
    assert!(
        matches!(backend.preview_edit(&id, &edit).unwrap(), Event::Preview { deck } if deck != original)
    );
    assert_eq!(backend.deck, original);
    assert_eq!(fs::read(&backend.path).unwrap(), disk);
    assert!(backend.undo.is_empty());
    backend
        .execute(Command::Apply {
            ops: vec![Op::Restyle {
                target: "b1".into(),
                style: deck::Style {
                    size: deck::TextSize::S,
                    ..deck::Style::default()
                },
            }],
        })
        .unwrap();
    assert_eq!(
        backend.feedback.threads[0].messages[1].edits[0].status,
        EditStatus::Outdated
    );
    assert!(backend.apply_edit(&id, &edit).is_err());
    backend.execute(Command::Undo {}).unwrap();
    assert_eq!(
        backend.feedback.threads[0].messages[1].edits[0].status,
        EditStatus::Pending
    );
    backend.apply_edit(&id, &edit).unwrap();
    assert_eq!(backend.undo.len(), 1);
    assert_eq!(
        backend.feedback.threads[0].messages[1].edits[0].status,
        EditStatus::Applied
    );
    backend.execute(Command::Undo {}).unwrap();
    assert_eq!(backend.deck, original);
}

#[test]
fn choose_option_applies_exactly_one_and_invalidates_its_alternative() {
    let temp = Temp::new();
    let mut backend = temp.backend();
    let req = request(&backend, Kind::Chat);
    backend
        .receive_feedback(&req, None, response(&req, true))
        .unwrap();
    let message = backend.feedback.chat[0].id.clone();
    let edits = backend.feedback.chat[0].edits.clone();
    backend
        .execute(Command::ChooseOption {
            message: message.clone(),
            edit: edits[0].proposal.id.clone(),
        })
        .unwrap();
    assert_eq!(backend.undo.len(), 1);
    assert_eq!(
        backend.feedback.chat[0].edits[0].status,
        EditStatus::Applied
    );
    assert_eq!(
        backend.feedback.chat[0].edits[1].status,
        EditStatus::Outdated
    );
    backend
        .dismiss_edit(&message, &edits[1].proposal.id)
        .unwrap();
    assert_eq!(
        backend.feedback.chat[0].edits[1].status,
        EditStatus::Dismissed
    );
}

#[test]
fn caps_and_thirty_day_pruning_survive_round_trip() {
    let temp = Temp::new();
    let mut backend = temp.backend();
    backend
        .comment_create(anchor(), "Keep".into(), false)
        .unwrap();
    let mut old = backend.feedback.threads[0].clone();
    old.id = "old".into();
    old.resolved = Some(now() - 31 * 24 * 60 * 60);
    backend.feedback.threads.push(old);
    for i in 0..205 {
        let mut thread = backend.feedback.threads[0].clone();
        thread.id = format!("t{i}");
        backend.feedback.threads.push(thread);
    }
    backend.feedback.chat = (0..105)
        .map(|i| message(Role::You, i.to_string(), vec![]))
        .collect();
    backend.persist_feedback().unwrap();
    let reopened = temp.backend();
    assert_eq!(reopened.feedback.threads.len(), 200);
    assert_eq!(reopened.feedback.chat.len(), 100);
    assert!(reopened.feedback.threads.iter().all(|t| t.id != "old"));
    assert_eq!(reopened.feedback.chat[0].text, "5");
    assert!(
        backend
            .comment_create(anchor(), "One too many".into(), false)
            .is_err()
    );
}

async fn read_request(stream: &mut tokio::net::TcpStream) -> (String, Value) {
    let mut bytes = Vec::new();
    let mut chunk = [0; 4096];
    let split = loop {
        let n = stream.read(&mut chunk).await.unwrap();
        assert!(n > 0);
        bytes.extend_from_slice(&chunk[..n]);
        if let Some(i) = bytes.windows(4).position(|w| w == b"\r\n\r\n") {
            break i;
        }
    };
    let headers = String::from_utf8(bytes[..split].to_vec()).unwrap();
    let length = headers
        .lines()
        .find_map(|l| {
            l.to_ascii_lowercase()
                .strip_prefix("content-length: ")
                .and_then(|v| v.parse::<usize>().ok())
        })
        .unwrap_or(0);
    while bytes.len() < split + 4 + length {
        let n = stream.read(&mut chunk).await.unwrap();
        assert!(n > 0);
        bytes.extend_from_slice(&chunk[..n]);
    }
    (
        headers,
        serde_json::from_slice(&bytes[split + 4..]).unwrap_or(Value::Null),
    )
}
async fn respond(stream: &mut tokio::net::TcpStream, value: Value) {
    let body = value.to_string();
    stream.write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).as_bytes()).await.unwrap();
}

#[tokio::test]
async fn sending_and_replying_run_as_async_work_with_whole_thread_context() {
    let temp = Temp::new();
    let mut backend = temp.backend();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    backend.configure_host(
        host::Connection::new(
            &format!("http://{}", listener.local_addr().unwrap()),
            "test-token".into(),
        )
        .unwrap(),
    );
    let mock = tokio::spawn(async move {
        for expected in [1, 3] {
            let (mut stream, _) = listener.accept().await.unwrap();
            let (headers, _) = read_request(&mut stream).await;
            assert!(!headers.to_lowercase().contains("authorization"));
            respond(&mut stream, json!({"product":"nexus-chat","host_id":nexus_core::host::host_discovery_id("test-token")})).await;
            let (mut stream, _) = listener.accept().await.unwrap();
            let (headers, body) = read_request(&mut stream).await;
            assert!(headers.starts_with("POST /v1/studio/suggest "));
            let req: feedback::Request = serde_json::from_value(body).unwrap();
            assert_eq!(req.kind, Kind::Thread);
            assert_eq!(req.messages.len(), expected);
            assert_eq!(req.anchor.as_ref().unwrap().block.as_deref(), Some("b1"));
            respond(
                &mut stream,
                serde_json::to_value(response(&req, false)).unwrap(),
            )
            .await;
        }
    });
    assert!(matches!(
        backend
            .comment_create(anchor(), "Clarify this".into(), true)
            .unwrap(),
        Event::Suggesting {}
    ));
    let completed = backend.take_work().unwrap().run().await;
    assert!(matches!(
        backend.complete(completed),
        Event::Feedback { .. }
    ));
    let id = backend.feedback.threads[0].id.clone();
    backend
        .comment_reply(&id, "Keep my tone".into(), true)
        .unwrap();
    let completed = backend.take_work().unwrap().run().await;
    assert!(matches!(
        backend.complete(completed),
        Event::Feedback { .. }
    ));
    assert_eq!(backend.feedback.threads[0].messages.len(), 4);
    assert!(backend.undo.is_empty());
    mock.await.unwrap();
}
