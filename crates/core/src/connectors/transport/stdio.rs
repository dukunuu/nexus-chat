//! stdio: a child process speaking newline-delimited JSON-RPC on stdin and
//! stdout. The command is an argv array (never a shell string); provider
//! keys are scrubbed from its environment; stderr is appended to a
//! per-connector log, rotated at 1 MB, with known secrets masked. The child
//! (and its process group) is killed when the transport is dropped.

use std::collections::HashMap;
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::{Arc, Mutex};

use serde_json::Value;
use tokio::io::{AsyncBufReadExt as _, AsyncReadExt as _, AsyncWriteExt as _, BufReader};
use tokio::process::{Child, ChildStdin, Command};
use tokio::sync::oneshot;

use super::{McpError, Signals};
use crate::connectors::protocol::{self, MAX_MESSAGE_BYTES, METHOD_NOT_FOUND, Message};
use crate::connectors::secrets::redact_value;

/// How a child is started.
#[derive(Clone)]
pub struct StdioSpec {
    pub argv: Vec<String>,
    /// Extra environment, already resolved from the keychain.
    pub env: Vec<(String, String)>,
    /// Where stderr goes; `None` discards it.
    pub log_path: Option<PathBuf>,
    /// Secret values to mask in the log.
    pub redact: Vec<String>,
}

type Reply = Result<Value, McpError>;

struct Inner {
    pending: Mutex<HashMap<u64, oneshot::Sender<Reply>>>,
    stdin: tokio::sync::Mutex<ChildStdin>,
    signals: Arc<Signals>,
    redact: Vec<String>,
    #[cfg(unix)]
    group: Option<libc::pid_t>,
    child: Mutex<Option<Child>>,
}

impl Drop for Inner {
    fn drop(&mut self) {
        #[cfg(unix)]
        if let Some(group) = self.group {
            // SAFETY: killing the child's own process group; the pid came
            // from the child we spawned.
            unsafe { libc::kill(-group, libc::SIGKILL) };
        }
        if let Some(child) = self.child.lock().expect("child").as_mut() {
            let _ = child.start_kill();
        }
    }
}

/// A running child process.
pub struct StdioTransport {
    inner: Arc<Inner>,
}

/// The child's command line and environment: the argv as given, the
/// parent's environment minus provider credentials, then the configured
/// variables on top.
fn build_command(spec: &StdioSpec) -> Result<Command, McpError> {
    let (program, args) = spec
        .argv
        .split_first()
        .ok_or_else(|| McpError::Protocol("the command is empty".into()))?;
    let mut command = Command::new(program);
    command
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    crate::config::scrub_secret_env(command.as_std_mut());
    for (key, value) in &spec.env {
        command.env(key, value);
    }
    #[cfg(unix)]
    command.process_group(0);
    Ok(command)
}

impl StdioTransport {
    /// Start the process and its reader tasks (needs a tokio runtime).
    ///
    /// # Errors
    /// The command is empty or could not be started.
    pub fn spawn(spec: &StdioSpec, signals: Arc<Signals>) -> Result<Self, McpError> {
        let mut command = build_command(spec)?;
        let program = &spec.argv[0];
        let mut child = command.spawn().map_err(|error| {
            McpError::Transport(format!("could not start {program}: {}", error.kind()))
        })?;
        let stdin = child.stdin.take().expect("piped stdin");
        let stdout = child.stdout.take().expect("piped stdout");
        let stderr = child.stderr.take().expect("piped stderr");
        #[cfg(unix)]
        let group = child.id().and_then(|pid| libc::pid_t::try_from(pid).ok());
        let inner = Arc::new(Inner {
            pending: Mutex::default(),
            stdin: tokio::sync::Mutex::new(stdin),
            signals,
            redact: spec.redact.clone(),
            #[cfg(unix)]
            group,
            child: Mutex::new(Some(child)),
        });
        tokio::spawn(read_stdout(Arc::downgrade(&inner), stdout));
        tokio::spawn(log_stderr(
            stderr,
            spec.log_path.clone(),
            spec.redact.clone(),
        ));
        Ok(Self { inner })
    }

    async fn write(&self, message: &Value) -> Result<(), McpError> {
        let mut line =
            serde_json::to_vec(message).map_err(|error| McpError::Protocol(error.to_string()))?;
        if line.len() > MAX_MESSAGE_BYTES {
            return Err(McpError::TooLarge);
        }
        line.push(b'\n');
        let mut stdin = self.inner.stdin.lock().await;
        stdin
            .write_all(&line)
            .await
            .and(stdin.flush().await)
            .map_err(|_| McpError::Closed("the process is not accepting input".into()))
    }

    /// Send a request and wait for the response with the same id.
    pub async fn request(&self, id: u64, message: &Value) -> Result<Value, McpError> {
        let (sender, receiver) = oneshot::channel();
        self.inner
            .pending
            .lock()
            .expect("pending")
            .insert(id, sender);
        let _guard = Forget {
            inner: &self.inner,
            id,
        };
        if self.inner.signals.is_closed() {
            return Err(McpError::Closed("the process has exited".into()));
        }
        self.write(message).await?;
        receiver
            .await
            .unwrap_or_else(|_| Err(McpError::Closed("the process exited".into())))
    }

    /// Send a notification.
    pub async fn notify(&self, message: &Value) -> Result<(), McpError> {
        self.write(message).await
    }

    /// Stop waiting for `id`.
    pub fn forget(&self, id: u64) {
        self.inner.pending.lock().expect("pending").remove(&id);
    }
}

struct Forget<'a> {
    inner: &'a Inner,
    id: u64,
}

impl Drop for Forget<'_> {
    fn drop(&mut self) {
        self.inner.pending.lock().expect("pending").remove(&self.id);
    }
}

/// Read one line of at most `MAX_MESSAGE_BYTES`. `Ok(None)` is end of file;
/// an over-long line is skipped to its end and reported as `Err(TooLarge)`.
async fn read_line<R: tokio::io::AsyncBufRead + Unpin>(
    reader: &mut R,
    line: &mut Vec<u8>,
) -> Result<Option<()>, McpError> {
    line.clear();
    let mut too_large = false;
    loop {
        let available = reader
            .fill_buf()
            .await
            .map_err(|error| McpError::Closed(error.kind().to_string()))?;
        if available.is_empty() {
            if too_large {
                return Err(McpError::TooLarge);
            }
            return Ok((!line.is_empty()).then_some(()));
        }
        let (used, done) = match available.iter().position(|b| *b == b'\n') {
            Some(end) => (end + 1, true),
            None => (available.len(), false),
        };
        if !too_large {
            if line.len() + used > MAX_MESSAGE_BYTES {
                too_large = true;
                line.clear();
            } else {
                line.extend_from_slice(&available[..used]);
            }
        }
        reader.consume(used);
        if done {
            return if too_large {
                Err(McpError::TooLarge)
            } else {
                Ok(Some(()))
            };
        }
    }
}

async fn read_stdout(inner: std::sync::Weak<Inner>, stdout: tokio::process::ChildStdout) {
    let mut reader = BufReader::new(stdout);
    let mut line = Vec::new();
    loop {
        match read_line(&mut reader, &mut line).await {
            Ok(Some(())) => {}
            Err(McpError::TooLarge) => continue,
            Ok(None) | Err(_) => break,
        }
        let Some(inner) = inner.upgrade() else {
            return;
        };
        // Servers and launchers print banners; anything that is not JSON-RPC
        // is not ours.
        let Ok(value) = serde_json::from_slice::<Value>(&line) else {
            continue;
        };
        let value = redact_value(value, &inner.redact);
        for message in Message::parse_all(&value) {
            dispatch(&inner, message).await;
        }
    }
    if let Some(inner) = inner.upgrade() {
        inner.signals.close();
        let senders: Vec<_> = inner.pending.lock().expect("pending").drain().collect();
        for (_, sender) in senders {
            let _ = sender.send(Err(McpError::Closed("the process exited".into())));
        }
    }
}

async fn dispatch(inner: &Arc<Inner>, message: Message) {
    match message {
        Message::Response { id, outcome } => {
            let sender = id
                .as_u64()
                .and_then(|id| inner.pending.lock().expect("pending").remove(&id));
            if let Some(sender) = sender {
                let _ = sender.send(outcome.map_err(McpError::Rpc));
            }
        }
        Message::Notification { method } => {
            if method == "notifications/tools/list_changed" {
                inner.signals.list_changed.notify_one();
            }
        }
        Message::Request { id, method } => {
            // We advertise no capabilities, so the only request a polite
            // server sends is `ping`.
            let reply = if method == "ping" {
                protocol::response_ok(&id, serde_json::json!({}))
            } else {
                protocol::response_error(&id, METHOD_NOT_FOUND, "method not supported")
            };
            if let Ok(mut line) = serde_json::to_vec(&reply) {
                line.push(b'\n');
                let mut stdin = inner.stdin.lock().await;
                let _ = stdin.write_all(&line).await;
                let _ = stdin.flush().await;
            }
        }
    }
}

const LOG_LIMIT: u64 = 1024 * 1024;
const LOG_LINE_LIMIT: usize = 4096;

/// Append stderr lines to the log, rotating to `<file>.1` past 1 MB.
async fn log_stderr(
    mut stderr: tokio::process::ChildStderr,
    path: Option<PathBuf>,
    redact: Vec<String>,
) {
    let mut sink = path.map(LogSink::new);
    let mut pending = Vec::new();
    let mut skipping = false;
    let mut buf = [0_u8; 4096];
    loop {
        let read = match stderr.read(&mut buf).await {
            Ok(0) | Err(_) => break,
            Ok(read) => read,
        };
        pending.extend_from_slice(&buf[..read]);
        while let Some(end) = pending.iter().position(|b| *b == b'\n') {
            let line: Vec<u8> = pending.drain(..=end).collect();
            if !skipping && let Some(sink) = &mut sink {
                sink.write(&String::from_utf8_lossy(&line), &redact);
            }
            skipping = false;
        }
        if pending.len() > LOG_LINE_LIMIT * 4 {
            // Do not split a credential across separately redacted fragments.
            if !skipping && let Some(sink) = &mut sink {
                sink.write("[stderr line exceeded the log limit]", &[]);
            }
            pending.clear();
            skipping = true;
        }
    }
    if !skipping
        && !pending.is_empty()
        && let Some(sink) = &mut sink
    {
        sink.write(&String::from_utf8_lossy(&pending), &redact);
    }
}

/// An append-only log file with one rotation.
pub struct LogSink {
    path: PathBuf,
}

impl LogSink {
    #[must_use]
    pub const fn new(path: PathBuf) -> Self {
        Self { path }
    }

    /// Append one line (a timestamp is added; secrets are masked).
    pub fn write(&mut self, line: &str, redact: &[String]) {
        use std::io::Write as _;
        let mut text =
            crate::connectors::secrets::redact(line.trim_end_matches(['\n', '\r']), redact);
        if text.chars().count() > LOG_LINE_LIMIT {
            text = text.chars().take(LOG_LINE_LIMIT).collect::<String>() + " …";
        }
        if let Some(dir) = self.path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let record = format!("{} {text}\n", chrono::Utc::now().to_rfc3339());
        if std::fs::metadata(&self.path)
            .is_ok_and(|meta| meta.len() + record.len() as u64 > LOG_LIMIT)
        {
            let mut rotated = self.path.clone().into_os_string();
            rotated.push(".1");
            let _ = std::fs::rename(&self.path, rotated);
        }
        if let Ok(mut file) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)
        {
            let _ = file.write_all(record.as_bytes());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn provider_keys_are_scrubbed_from_the_child_and_configured_env_is_added() {
        let spec = StdioSpec {
            argv: vec!["srv".into(), "--flag".into()],
            env: vec![("MY_TOKEN".into(), "resolved".into())],
            log_path: None,
            redact: Vec::new(),
        };
        let command = build_command(&spec).unwrap();
        let std_command = command.as_std();
        assert_eq!(std_command.get_program(), "srv");
        assert_eq!(std_command.get_args().collect::<Vec<_>>(), ["--flag"]);
        let envs: std::collections::HashMap<_, _> = std_command
            .get_envs()
            .map(|(key, value)| {
                (
                    key.to_string_lossy().into_owned(),
                    value.map(|v| v.to_string_lossy().into_owned()),
                )
            })
            .collect();
        for key in crate::config::SECRET_ENV_VARS {
            assert_eq!(envs.get(*key), Some(&None), "{key} must be removed");
        }
        assert_eq!(envs["MY_TOKEN"].as_deref(), Some("resolved"));
        assert!(
            build_command(&StdioSpec {
                argv: Vec::new(),
                ..spec
            })
            .is_err()
        );
    }

    #[tokio::test]
    async fn lines_are_read_and_oversize_lines_are_skipped() {
        let mut data = b"{\"a\":1}\n".to_vec();
        data.extend(std::iter::repeat_n(b'x', MAX_MESSAGE_BYTES + 5));
        data.extend_from_slice(b"\n{\"b\":2}\nlast");
        let mut reader = BufReader::new(std::io::Cursor::new(data));
        let mut line = Vec::new();
        assert_eq!(read_line(&mut reader, &mut line).await, Ok(Some(())));
        assert_eq!(line, b"{\"a\":1}\n");
        assert_eq!(
            read_line(&mut reader, &mut line).await,
            Err(McpError::TooLarge)
        );
        assert_eq!(read_line(&mut reader, &mut line).await, Ok(Some(())));
        assert_eq!(line, b"{\"b\":2}\n");
        assert_eq!(read_line(&mut reader, &mut line).await, Ok(Some(())));
        assert_eq!(line, b"last");
        assert_eq!(read_line(&mut reader, &mut line).await, Ok(None));
    }

    #[test]
    fn the_log_rotates_at_one_megabyte_and_masks_secrets() {
        let dir = std::env::temp_dir().join(format!("nexus-conn-log-{}", uuid::Uuid::new_v4()));
        let path = dir.join("logs").join("demo.log");
        let mut sink = LogSink::new(path.clone());
        sink.write("token=hunter2hunter2 ok", &["hunter2hunter2".to_string()]);
        let first = std::fs::read_to_string(&path).unwrap();
        assert!(first.contains("token=… ok"), "{first}");
        assert!(!first.contains("hunter2"));
        let filler = "y".repeat(2000);
        for _ in 0..700 {
            sink.write(&filler, &[]);
        }
        let mut rotated = path.clone().into_os_string();
        rotated.push(".1");
        assert!(std::path::Path::new(&rotated).exists(), "rotated file");
        assert!(std::fs::metadata(&path).unwrap().len() < LOG_LIMIT);
        std::fs::remove_dir_all(dir).unwrap();
    }
}
