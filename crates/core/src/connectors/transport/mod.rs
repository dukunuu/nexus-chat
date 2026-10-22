//! The two MCP transports behind one enum: Streamable HTTP and stdio. A
//! transport sends one JSON-RPC message and, for requests, hands back the
//! matching response; negotiation, timeouts and cancellation live in the
//! client above it.

pub mod http;
pub mod stdio;

use std::fmt;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use serde_json::Value;
use tokio::sync::Notify;

use super::protocol::RpcError;

/// Everything that can go wrong talking to a server. Messages are safe to
/// show: no credentials, no query strings.
#[derive(Debug, Clone, PartialEq)]
pub enum McpError {
    /// The server wants credentials (HTTP 401). C2 turns this into OAuth.
    NeedsAuth { resource_metadata: Option<String> },
    /// The server forgot our `Mcp-Session-Id` (HTTP 404 with a session).
    SessionExpired,
    /// A non-success HTTP status.
    Http { status: u16, note: String },
    /// The server answered with a JSON-RPC error.
    Rpc(RpcError),
    /// No answer in time.
    Timeout { secs: u64 },
    /// The transport is gone (process exited, stream ended).
    Closed(String),
    /// An HTTP response stream broke before the answer arrived.
    Interrupted,
    /// Could not reach the server or write to it.
    Transport(String),
    /// A single message exceeded the size cap.
    TooLarge,
    /// The server said something that is not MCP, or a version we lack.
    Protocol(String),
    /// The server asked for something nexus does not do yet.
    Unsupported(String),
}

impl fmt::Display for McpError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NeedsAuth { .. } => write!(f, "sign-in required"),
            Self::SessionExpired => write!(f, "the server forgot the session"),
            Self::Http { status, note } if note.is_empty() => write!(f, "HTTP {status}"),
            Self::Http { status, note } => write!(f, "HTTP {status}: {note}"),
            Self::Rpc(error) => write!(f, "{error}"),
            Self::Timeout { secs } => write!(f, "timed out after {secs}s"),
            Self::Closed(why) => write!(f, "connection closed: {why}"),
            Self::Interrupted => write!(f, "the response stream broke"),
            Self::TooLarge => write!(f, "a message was larger than 8 MB"),
            Self::Transport(why) | Self::Protocol(why) | Self::Unsupported(why) => {
                write!(f, "{why}")
            }
        }
    }
}

impl std::error::Error for McpError {}

/// Events a transport raises outside any one request.
#[derive(Debug, Default)]
pub struct Signals {
    /// `notifications/tools/list_changed` arrived.
    pub list_changed: Notify,
    /// The transport died; waiters should reconnect or give up.
    pub closed: Notify,
    closed_flag: AtomicBool,
}

impl Signals {
    /// Fresh signals.
    #[must_use]
    pub fn new() -> Arc<Self> {
        Arc::default()
    }

    /// Mark the transport closed and wake waiters.
    pub fn close(&self) {
        self.closed_flag.store(true, Ordering::SeqCst);
        self.closed.notify_waiters();
        self.closed.notify_one();
    }

    /// Whether the transport has died.
    #[must_use]
    pub fn is_closed(&self) -> bool {
        self.closed_flag.load(Ordering::SeqCst)
    }

    /// Wait until the transport dies.
    pub async fn wait_closed(&self) {
        loop {
            let notified = self.closed.notified();
            if self.is_closed() {
                return;
            }
            notified.await;
        }
    }
}

/// A connected transport.
pub enum Transport {
    Http(Box<http::HttpTransport>),
    Stdio(stdio::StdioTransport),
}

impl Transport {
    /// Send a request and wait for its response `result`.
    pub async fn request(&self, id: u64, message: &Value) -> Result<Value, McpError> {
        match self {
            Self::Http(transport) => transport.request(id, message).await,
            Self::Stdio(transport) => transport.request(id, message).await,
        }
    }

    /// Send a notification.
    pub async fn notify(&self, message: &Value) -> Result<(), McpError> {
        match self {
            Self::Http(transport) => transport.notify(message).await,
            Self::Stdio(transport) => transport.notify(message).await,
        }
    }

    /// Tell the transport which protocol was agreed (HTTP headers differ).
    pub fn configure(&self, protocol: &str, stateless: bool) {
        if let Self::Http(transport) = self {
            transport.configure(protocol, stateless);
        }
    }

    /// Forget the server-side session (before a fresh `initialize`).
    pub fn clear_session(&self) {
        if let Self::Http(transport) = self {
            transport.clear_session();
        }
    }

    /// Stop waiting for a request (it timed out or its caller went away).
    pub fn forget(&self, id: u64) {
        if let Self::Stdio(transport) = self {
            transport.forget(id);
        }
    }

    /// Whether cancellation is a message (`notifications/cancelled`) rather
    /// than closing the connection.
    #[must_use]
    pub fn cancels_by_message(&self, stateless: bool) -> bool {
        match self {
            Self::Http(_) => !stateless,
            Self::Stdio(_) => true,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn closing_wakes_waiters_even_when_it_happens_first() {
        let signals = Signals::new();
        signals.close();
        tokio::time::timeout(std::time::Duration::from_secs(1), signals.wait_closed())
            .await
            .expect("an already-closed transport must not block");
        let other = Signals::new();
        let waiter = tokio::spawn({
            let other = Arc::clone(&other);
            async move { other.wait_closed().await }
        });
        tokio::task::yield_now().await;
        other.close();
        tokio::time::timeout(std::time::Duration::from_secs(1), waiter)
            .await
            .expect("woken")
            .unwrap();
    }

    #[test]
    fn errors_never_print_more_than_they_were_given() {
        let error = McpError::Http {
            status: 500,
            note: String::new(),
        };
        assert_eq!(error.to_string(), "HTTP 500");
        assert_eq!(
            McpError::NeedsAuth {
                resource_metadata: Some("https://x/.well-known".into())
            }
            .to_string(),
            "sign-in required"
        );
    }
}
