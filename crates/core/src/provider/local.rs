//! Opt-in local runtime configuration and installed-model discovery.

use std::process::Stdio;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use tokio::io::AsyncReadExt as _;

use super::{BackendTag, Model};

/// Local runtime used for model discovery and inference.
#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum LocalRuntime {
    Ollama,
    Mlx,
    Lmstudio,
}

/// Trusted, machine-local settings. Commands are argv arrays, never shell strings.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct LocalConfig {
    pub provider: LocalRuntime,
    /// Override the runtime's OpenAI-compatible API base.
    pub endpoint: Option<String>,
    /// Optional discovery command; stdout must contain one model ID per line.
    pub list_command: Option<Vec<String>>,
}

impl LocalConfig {
    /// API base used for inference, independent of cloud credentials.
    #[must_use]
    pub fn endpoint(&self) -> &str {
        self.endpoint.as_deref().unwrap_or(match self.provider {
            LocalRuntime::Ollama => "http://localhost:11434/v1",
            LocalRuntime::Mlx => "http://localhost:8080/v1",
            LocalRuntime::Lmstudio => "http://localhost:1234/v1",
        })
    }

    /// Validate settings without executing commands or contacting a server.
    ///
    /// # Errors
    /// Rejects invalid URLs and empty discovery commands.
    pub fn validate(&self) -> Result<()> {
        super::openrouter::OpenRouter::local(self.endpoint())?;
        if let Some(command) = &self.list_command {
            anyhow::ensure!(
                command
                    .first()
                    .is_some_and(|program| !program.trim().is_empty()),
                "local.list_command must contain an executable"
            );
        }
        Ok(())
    }

    fn command(&self) -> Vec<String> {
        if let Some(command) = &self.list_command {
            return command.clone();
        }
        match self.provider {
            LocalRuntime::Ollama => vec!["ollama".into(), "list".into()],
            LocalRuntime::Lmstudio => vec!["lms".into(), "ls".into(), "--json".into()],
            LocalRuntime::Mlx => vec![
                "python3".into(),
                "-c".into(),
                include_str!("../../assets/list-mlx-models.py").into(),
            ],
        }
    }

    /// List installed models through the selected runtime's command.
    ///
    /// # Errors
    /// Reports missing executables, timeouts, oversized output and invalid catalogs.
    pub async fn list_models(&self) -> Result<Vec<Model>> {
        self.validate()?;
        let command = self.command();
        let output = run_command(&command).await?;
        let ids = if self.list_command.is_some() || self.provider == LocalRuntime::Mlx {
            output
                .lines()
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_owned)
                .collect()
        } else if self.provider == LocalRuntime::Ollama {
            parse_ollama(&output)?
        } else {
            parse_lmstudio(&output)?
        };
        let mut ids: Vec<String> = ids;
        ids.sort();
        ids.dedup();
        anyhow::ensure!(ids.len() <= 10_000, "local model catalog is too large");
        Ok(ids
            .into_iter()
            .map(|id| Model {
                name: format!(
                    "{id} ({})",
                    match self.provider {
                        LocalRuntime::Ollama => "Ollama",
                        LocalRuntime::Mlx => "MLX",
                        LocalRuntime::Lmstudio => "LM Studio",
                    }
                ),
                id,
                backend: BackendTag::Local,
                context_length: None,
                reasoning_efforts: Vec::new(),
                supports_images: false,
                supports_image_generation: false,
                supports_video_generation: false,
                pricing: None,
            })
            .collect())
    }
}

async fn run_command(argv: &[String]) -> Result<String> {
    const MAX_OUTPUT: u64 = 1024 * 1024;
    let program = argv.first().context("empty local discovery command")?;
    let mut child = tokio::process::Command::new(program)
        .args(&argv[1..])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .with_context(|| {
            format!("starting local model discovery ({program}); is the runtime installed?")
        })?;
    let stdout = child.stdout.take().context("missing discovery stdout")?;
    let result = tokio::time::timeout(Duration::from_secs(15), async {
        let mut bytes = Vec::new();
        stdout.take(MAX_OUTPUT + 1).read_to_end(&mut bytes).await?;
        anyhow::ensure!(
            bytes.len() as u64 <= MAX_OUTPUT,
            "local discovery output exceeds 1 MiB"
        );
        let status = child.wait().await?;
        anyhow::ensure!(
            status.success(),
            "local discovery command {program} failed ({status})"
        );
        String::from_utf8(bytes).context("local discovery output is not UTF-8")
    })
    .await;
    match result {
        Ok(Ok(output)) => Ok(output),
        other => {
            let _ = child.kill().await;
            match other {
                Ok(Err(error)) => Err(error),
                Err(_) => bail!("local model discovery timed out after 15 seconds"),
                Ok(Ok(_)) => unreachable!(),
            }
        }
    }
}

fn parse_ollama(output: &str) -> Result<Vec<String>> {
    let mut lines = output.lines().filter(|line| !line.trim().is_empty());
    let header = lines.next().context("ollama list returned no header")?;
    anyhow::ensure!(
        header.split_whitespace().next() == Some("NAME"),
        "unexpected ollama list output"
    );
    Ok(lines
        .filter_map(|line| line.split_whitespace().next().map(str::to_owned))
        .collect())
}

fn parse_lmstudio(output: &str) -> Result<Vec<String>> {
    let value: serde_json::Value =
        serde_json::from_str(output).context("invalid lms ls --json output")?;
    let entries = value
        .as_array()
        .context("expected a JSON array from lms ls --json")?;
    entries
        .iter()
        .filter(|entry| entry.get("type").and_then(serde_json::Value::as_str) != Some("embedding"))
        .map(|entry| {
            entry
                .get("modelKey")
                .or_else(|| entry.get("path"))
                .and_then(serde_json::Value::as_str)
                .filter(|id| !id.is_empty())
                .map(str::to_owned)
                .context("LM Studio model missing modelKey/path")
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_runtime_catalogs() {
        assert_eq!(
            parse_ollama("NAME ID SIZE MODIFIED\nqwen3:8b abc 5GB now\n").unwrap(),
            ["qwen3:8b"]
        );
        assert!(parse_ollama("server unavailable").is_err());
        assert!(parse_ollama("NAME ID SIZE MODIFIED\n").unwrap().is_empty());
        assert_eq!(
            parse_lmstudio(
                r#"[{"modelKey":"org/model","type":"llm"},{"modelKey":"embed","type":"embedding"}]"#
            )
            .unwrap(),
            ["org/model"]
        );
        assert!(parse_lmstudio("{}").is_err());
    }

    #[test]
    fn config_selects_runtime_without_cloud_keys() {
        let config: LocalConfig = toml::from_str("provider = 'ollama'").unwrap();
        assert_eq!(config.endpoint(), "http://localhost:11434/v1");
        assert_eq!(config.command(), ["ollama", "list"]);
        config.validate().unwrap();
        let invalid: LocalConfig = toml::from_str("provider = 'mlx'\nlist_command = []").unwrap();
        assert!(invalid.validate().is_err());
    }

    #[tokio::test]
    async fn missing_runtime_is_actionable() {
        let error = run_command(&[format!(
            "/nonexistent/nexus-runtime-{}",
            uuid::Uuid::new_v4()
        )])
        .await
        .unwrap_err();
        assert!(error.to_string().contains("is the runtime installed?"));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn failed_and_oversized_commands_are_rejected() {
        assert!(run_command(&["/usr/bin/false".into()]).await.is_err());
        let error = run_command(&["/usr/bin/printf".into(), "%1048577s".into(), "x".into()])
            .await
            .unwrap_err();
        assert!(error.to_string().contains("exceeds 1 MiB"));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn custom_discovery_is_keyless_and_preserves_ids() {
        let config = LocalConfig {
            provider: LocalRuntime::Mlx,
            endpoint: None,
            list_command: Some(vec![
                "/usr/bin/printf".into(),
                "org/model\norg/model\nother/model\n".into(),
            ]),
        };
        let models = config.list_models().await.unwrap();
        assert_eq!(models.len(), 2);
        assert_eq!(models[0].id, "org/model");
        assert_eq!(models[0].backend, BackendTag::Local);
    }
}
