use super::*;
use nexus_core::deck::Deck;
use nexus_core::deck::feedback::{Kind, Request};
use nexus_core::deck::suggest::Selection;
use serde_json::{Value, json};
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use tokio::net::TcpListener;

struct Temp(PathBuf);
impl Temp {
    fn new() -> Self {
        Self(std::env::temp_dir().join(format!("studio-jobs-{}", Uuid::new_v4())))
    }
    fn backend(&self) -> Backend {
        Backend::start(None, &self.0.join("files"), &self.0.join("data")).unwrap()
    }
}
impl Drop for Temp {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
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
    let req = Request {
        kind: Kind::Chat,
        revision: deck.revision(),
        deck,
        selection: Selection::default(),
        anchor: None,
        messages: vec![],
        instruction: String::new(),
        model: None,
    };
    let error = connection.feedback(&req).await.unwrap_err().to_string();
    assert!(error.contains("identity mismatch"));
    assert!(!error.contains("private-token"));
    mock.await.unwrap();
}

fn discovery(token: &str) -> Value {
    json!({"product":"nexus-chat","host_id":nexus_core::host::host_discovery_id(token)})
}

#[tokio::test]
async fn host_status_runs_as_work_and_only_the_snapshot_request_carries_the_token() {
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
        assert!(headers.starts_with("GET /.well-known/nexus "));
        assert!(!headers.to_ascii_lowercase().contains("authorization"));
        respond(&mut stream, discovery("private-token")).await;
        let (mut stream, _) = listener.accept().await.unwrap();
        let (headers, _) = read_http(&mut stream).await;
        assert!(headers.starts_with("GET /v1/snapshot "));
        assert!(headers.contains("Bearer private-token"));
        respond(&mut stream, json!({"settings":{"model":"vendor/mock"}})).await;
    });
    let event = backend.dispatch(r#"{"command":"HostStatus"}"#);
    assert!(matches!(event, Event::HostChecking {}), "{event:?}");
    assert!(!backend.suggesting);
    let result = backend.take_work().unwrap().run().await;
    let event = backend.complete(result);
    let Event::HostStatus { status } = &event else {
        panic!("{event:?}")
    };
    assert_eq!(status.state, "connected");
    assert_eq!(status.model.as_deref(), Some("vendor/mock"));
    assert!(!event.script().contains("private-token"));
    assert!(backend.take_work().is_none());
    mock.await.unwrap();
}

#[tokio::test]
async fn feedback_jobs_are_exclusive_and_results_for_a_replaced_deck_are_dropped() {
    let temp = Temp::new();
    let mut backend = temp.backend();
    assert!(matches!(
        backend.request_host(Job::HostStatus),
        Err(e) if e.to_string().contains("not configured")
    ));
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    backend.configure_host(
        host::Connection::new(
            &format!("http://{}", listener.local_addr().unwrap()),
            "private-token".into(),
        )
        .unwrap(),
    );
    let feedback = |backend: &Backend| Job::Feedback {
        request: Box::new(Request {
            kind: Kind::Chat,
            revision: backend.deck.revision(),
            deck: backend.deck.clone(),
            selection: Selection::default(),
            anchor: None,
            messages: vec![],
            instruction: String::new(),
            model: None,
        }),
        thread: None,
    };
    let job = feedback(&backend);
    assert!(matches!(
        backend.request_host(job),
        Ok(Event::Suggesting {})
    ));
    let job = feedback(&backend);
    assert!(backend.request_host(job).is_err());
    // The user opens another deck while the host is thinking.
    let work = backend.take_work().unwrap();
    let event = backend.dispatch(r#"{"command":"NewDeck","title":"Other"}"#);
    assert!(matches!(event, Event::Deck { .. }), "{event:?}");
    let mock = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        read_http(&mut stream).await;
        respond(&mut stream, discovery("private-token")).await;
        let (mut stream, _) = listener.accept().await.unwrap();
        let (_, body) = read_http(&mut stream).await;
        let req: Request = serde_json::from_slice(&body).unwrap();
        let reply = nexus_core::deck::feedback::Response {
            text: "Looks fine.".into(),
            revision: req.revision,
            model: "mock".into(),
            ..Default::default()
        };
        respond(&mut stream, serde_json::to_value(reply).unwrap()).await;
    });
    let event = backend.complete(work.run().await);
    assert!(
        matches!(&event, Event::Error { message } if message.contains("Deck changed")),
        "{event:?}"
    );
    assert!(backend.feedback.chat.is_empty());
    // The guard is released so the next request is accepted.
    let job = feedback(&backend);
    assert!(backend.request_host(job).is_ok());
    mock.await.unwrap();
}
