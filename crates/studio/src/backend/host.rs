//! Credential-bearing HTTP stays in Rust and follows unauthenticated discovery.

use std::path::Path;
use std::time::Duration;

use anyhow::{Context as _, Result, bail, ensure};
use nexus_core::deck::suggest::{SuggestRequest, SuggestResponse};
use nexus_core::host::host_discovery_id;
use reqwest::{Client, Url};
use serde::Serialize;
use serde_json::Value;

#[derive(Clone)]
pub struct Connection {
    origin: Url,
    token: String,
    client: Client,
}

#[derive(Clone, Debug, Serialize)]
pub struct Status {
    pub state: &'static str,
    pub model: Option<String>,
    pub can_start: bool,
    pub message: &'static str,
}

impl Connection {
    pub fn new(origin: &str, token: String) -> Result<Self> {
        let origin = Url::parse(origin).context("invalid host URL")?;
        ensure!(
            matches!(origin.scheme(), "http" | "https")
                && origin.host_str().is_some()
                && origin.username().is_empty()
                && origin.password().is_none()
                && matches!(origin.path(), "" | "/")
                && origin.query().is_none()
                && origin.fragment().is_none(),
            "expected host origin without credentials or path"
        );
        let client = Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .no_proxy()
            .timeout(Duration::from_secs(125))
            .build()?;
        Ok(Self {
            origin,
            token,
            client,
        })
    }

    fn loopback(&self) -> bool {
        self.origin.host_str().is_some_and(|host| {
            host == "localhost"
                || host
                    .trim_matches(['[', ']'])
                    .parse::<std::net::IpAddr>()
                    .is_ok_and(|ip| ip.is_loopback())
        })
    }

    fn url(&self, path: &str) -> Url {
        self.origin.join(path).expect("static host path")
    }

    async fn discover(&self) -> Status {
        let response = self
            .client
            .get(self.url("/.well-known/nexus"))
            .timeout(Duration::from_secs(2))
            .send()
            .await;
        let Ok(response) = response else {
            return self.offline();
        };
        let value = response.json::<Value>().await.unwrap_or_default();
        if value["product"] != "nexus-chat" || value["host_id"] != host_discovery_id(&self.token) {
            return Status {
                state: "mismatch",
                model: None,
                can_start: false,
                message: "Host identity mismatch; check the host URL and local token.",
            };
        }
        Status {
            state: "connected",
            model: None,
            can_start: false,
            message: "Connected",
        }
    }

    fn offline(&self) -> Status {
        Status {
            state: "offline",
            model: None,
            can_start: self.loopback(),
            message: "Host unavailable",
        }
    }

    pub async fn status(&self) -> Status {
        let mut status = self.discover().await;
        if status.state != "connected" {
            return status;
        }
        let response = self
            .client
            .get(self.url("/v1/snapshot"))
            .bearer_auth(&self.token)
            .timeout(Duration::from_secs(5))
            .send()
            .await;
        match response {
            Ok(response) if response.status().is_success() => {
                let value = response.json::<Value>().await.unwrap_or_default();
                status.model = value["settings"]["model"].as_str().map(str::to_owned);
                status
            }
            Ok(response) if response.status() == reqwest::StatusCode::UNAUTHORIZED => Status {
                state: "mismatch",
                model: None,
                can_start: false,
                message: "Host authentication mismatch; check the local token.",
            },
            _ => self.offline(),
        }
    }

    pub async fn suggest(&self, request: &SuggestRequest) -> Result<SuggestResponse> {
        let status = self.discover().await;
        ensure!(status.state == "connected", "{}", status.message);
        let response = self
            .client
            .post(self.url("/v1/studio/suggest"))
            .bearer_auth(&self.token)
            .json(request)
            .send()
            .await
            .map_err(|_| anyhow::anyhow!("Host suggestion request failed"))?;
        let code = response.status();
        if code == reqwest::StatusCode::UNPROCESSABLE_ENTITY {
            let reply = response
                .json::<SuggestResponse>()
                .await
                .context("Invalid host response")?;
            bail!("{}", reply.notes.join("; "));
        }
        ensure!(
            code.is_success(),
            "Host suggestion failed ({})",
            code.as_u16()
        );
        let reply = response
            .json::<SuggestResponse>()
            .await
            .context("Invalid host response")?;
        ensure!(
            reply.revision == request.revision,
            "Host returned a different revision"
        );
        Ok(reply)
    }

    pub async fn start(&self, data_dir: &Path) -> Result<Status> {
        ensure!(self.loopback(), "Only a loopback host can be started");
        let status = self.discover().await;
        if status.state == "connected" {
            return Ok(self.status().await);
        }
        ensure!(status.state == "offline", "{}", status.message);
        std::fs::create_dir_all(data_dir)?;
        let log = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(data_dir.join("studio-host.log"))?;
        let name = if cfg!(windows) { "nexus.exe" } else { "nexus" };
        let beside = std::env::current_exe()?.with_file_name(name);
        let mut command = std::process::Command::new(if beside.is_file() {
            beside
        } else {
            name.into()
        });
        command
            .args([
                "host",
                "--port",
                &self
                    .origin
                    .port_or_known_default()
                    .context("missing host port")?
                    .to_string(),
            ])
            .stdin(std::process::Stdio::null())
            .stdout(log.try_clone()?)
            .stderr(log);
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt as _;
            command.process_group(0);
        }
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt as _;
            command.creation_flags(0x0000_0008 | 0x0000_0200);
        }
        let mut child = command
            .spawn()
            .context("Could not start nexus host; install nexus beside studio or on PATH")?;
        // Reap if it eventually exits; the daemon is not tied to the window.
        std::thread::spawn(move || {
            let _ = child.wait();
        });
        let deadline = tokio::time::Instant::now() + Duration::from_secs(15);
        loop {
            let status = tokio::time::timeout_at(deadline, self.discover())
                .await
                .map_err(|_| {
                    anyhow::anyhow!("Host did not start within 15 seconds; see studio-host.log")
                })?;
            if status.state == "connected" {
                return Ok(self.status().await);
            }
            ensure!(status.state != "mismatch", "{}", status.message);
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
    }
}
