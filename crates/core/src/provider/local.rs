//! Opt-in local runtime configuration and installed-model discovery.

use std::process::Stdio;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use tokio::io::AsyncReadExt as _;

use super::{BackendTag, Model};

/// Local runtime used for model discovery and inference.
#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum LocalRuntime {
    Ollama,
    Mlx,
    MlxServe,
    Lmstudio,
    Edge0,
}

impl LocalRuntime {
    /// Every runtime, in the order `/local` lists them.
    pub const ALL: [Self; 5] = [
        Self::Ollama,
        Self::Mlx,
        Self::MlxServe,
        Self::Lmstudio,
        Self::Edge0,
    ];

    /// Human-readable runtime name, shown in model names and the picker.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Ollama => "Ollama",
            Self::Mlx => "MLX (mlx-lm)",
            Self::MlxServe => "mlx-serve",
            Self::Lmstudio => "LM Studio",
            Self::Edge0 => "edge0",
        }
    }

    /// The canonical `/local <token>` spelling, for echoing a runtime back
    /// in a command the user can retype.
    #[must_use]
    pub const fn token(self) -> &'static str {
        match self {
            Self::Ollama => "ollama",
            Self::Mlx => "mlx",
            Self::MlxServe => "mlx-serve",
            Self::Lmstudio => "lmstudio",
            Self::Edge0 => "edge0",
        }
    }

    /// Parse a `/local <runtime>` token, accepting the config spellings plus
    /// the ways people write them by hand (`lm-studio`, `mlx_lm`).
    #[must_use]
    pub fn parse(token: &str) -> Option<Self> {
        match token
            .to_ascii_lowercase()
            .replace(['-', '_', ' '], "")
            .as_str()
        {
            "ollama" => Some(Self::Ollama),
            "mlx" | "mlxlm" => Some(Self::Mlx),
            "mlxserve" => Some(Self::MlxServe),
            "lmstudio" | "lms" => Some(Self::Lmstudio),
            "edge0" | "edge" => Some(Self::Edge0),
            _ => None,
        }
    }
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
    /// Warn once a local server's process tree passes this many mebibytes.
    /// Unset means a fraction of physical memory. Advisory only: Nexus warns,
    /// it never stops a server on its own.
    pub memory_budget_mb: Option<u64>,
}

impl LocalConfig {
    /// API base used for inference, independent of cloud credentials.
    #[must_use]
    pub fn endpoint(&self) -> &str {
        self.endpoint.as_deref().unwrap_or(match self.provider {
            LocalRuntime::Ollama => "http://localhost:11434/v1",
            LocalRuntime::Mlx => "http://localhost:8080/v1",
            LocalRuntime::MlxServe => "http://localhost:11234/v1",
            LocalRuntime::Lmstudio => "http://localhost:1234/v1",
            LocalRuntime::Edge0 => "http://localhost:8000/v1",
        })
    }

    /// The TCP port the runtime's server listens on, from the endpoint.
    #[must_use]
    pub fn port(&self) -> Option<u16> {
        reqwest::Url::parse(self.endpoint())
            .ok()
            .and_then(|url| url.port_or_known_default())
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

    /// Switch to `provider`, carrying over the settings that still apply: a
    /// custom endpoint and discovery command are written for one runtime, so
    /// they survive re-selecting it and are dropped when the runtime changes.
    /// The memory budget describes the machine rather than the runtime, so it
    /// always carries.
    #[must_use]
    pub fn for_runtime(provider: LocalRuntime, current: Option<&Self>) -> Self {
        let carried = current.filter(|config| config.provider == provider);
        Self {
            provider,
            endpoint: carried.and_then(|config| config.endpoint.clone()),
            list_command: carried.and_then(|config| config.list_command.clone()),
            memory_budget_mb: current.and_then(|config| config.memory_budget_mb),
        }
    }

    /// Parse a `/local` argument: `off` disables local inference,
    /// `<runtime> [endpoint]` selects one. An explicit endpoint replaces the
    /// carried-over one; omitting it keeps whatever the runtime already had.
    ///
    /// # Errors
    /// Rejects unknown runtimes, extra arguments and invalid endpoints.
    pub fn from_spec(spec: &str, current: Option<&Self>) -> Result<Option<Self>> {
        let mut words = spec.split_whitespace();
        let runtime = words.next().context("expected a runtime, or `off`")?;
        if matches!(
            runtime.to_ascii_lowercase().as_str(),
            "off" | "none" | "disable"
        ) {
            anyhow::ensure!(words.next().is_none(), "`/local off` takes no arguments");
            return Ok(None);
        }
        let provider = LocalRuntime::parse(runtime).with_context(|| {
            format!(
                "unknown local runtime {runtime:?} — expected ollama, mlx, mlx-serve, lmstudio, edge0 or off"
            )
        })?;
        let mut config = Self::for_runtime(provider, current);
        if let Some(endpoint) = words.next() {
            config.endpoint = Some(endpoint.to_string());
        }
        anyhow::ensure!(
            words.next().is_none(),
            "expected `/local <runtime> [endpoint]`"
        );
        config.validate()?;
        Ok(Some(config))
    }

    fn command(&self) -> Vec<String> {
        if let Some(command) = &self.list_command {
            return command.clone();
        }
        match self.provider {
            LocalRuntime::Ollama => vec!["ollama".into(), "list".into()],
            LocalRuntime::Lmstudio => vec!["lms".into(), "ls".into(), "--json".into()],
            LocalRuntime::Edge0 => vec!["edge0".into(), "models".into()],
            LocalRuntime::MlxServe => vec!["mlx-serve".into(), "list".into()],
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
        } else {
            match self.provider {
                LocalRuntime::Ollama => parse_ollama(&output)?,
                LocalRuntime::Lmstudio => parse_lmstudio(&output)?,
                LocalRuntime::Edge0 => parse_edge0(&output)?,
                LocalRuntime::MlxServe => parse_mlx_serve(&output)?,
                LocalRuntime::Mlx => unreachable!("mlx takes the line-per-id path"),
            }
        };
        let mut ids: Vec<String> = ids;
        ids.sort();
        ids.dedup();
        anyhow::ensure!(ids.len() <= 10_000, "local model catalog is too large");
        Ok(ids
            .into_iter()
            .map(|id| Model {
                name: format!("{id} ({})", self.provider.label()),
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

/// `mlx-serve list` prints NAME TYPE SIZE; incomplete downloads are marked
/// unsupported and must not be offered as inference-ready models.
fn parse_mlx_serve(output: &str) -> Result<Vec<String>> {
    let mut lines = output.lines().filter(|line| !line.trim().is_empty());
    let header = lines.next().context("mlx-serve list returned no header")?;
    anyhow::ensure!(
        header.split_whitespace().take(2).collect::<Vec<_>>() == ["NAME", "TYPE"],
        "unexpected mlx-serve list output"
    );
    Ok(lines
        .filter_map(|line| {
            let mut fields = line.split_whitespace();
            let name = fields.next()?;
            let kind = fields.next()?;
            (kind != "unsupported").then(|| name.to_string())
        })
        .collect())
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

/// `edge0 models` prints one flush-left `<tier>  (port …)` line per
/// registered tier, each followed by indented detail lines. Only the
/// flush-left lines name a model, and the first one must be flush left —
/// anything else means we are not reading `edge0 models` output.
fn parse_edge0(output: &str) -> Result<Vec<String>> {
    let mut lines = output.lines().filter(|line| !line.trim().is_empty());
    anyhow::ensure!(
        !lines
            .next()
            .is_some_and(|first| first.starts_with(char::is_whitespace)),
        "unexpected edge0 models output"
    );
    Ok(output
        .lines()
        .filter(|line| !line.trim().is_empty())
        .filter(|line| !line.starts_with(char::is_whitespace))
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
            parse_mlx_serve(
                "NAME TYPE SIZE\norg/model text 5 GB\norg/partial unsupported 900 MB\n"
            )
            .unwrap(),
            ["org/model"]
        );
        assert!(parse_mlx_serve("unexpected output").is_err());
        assert!(parse_mlx_serve("NAME TYPE SIZE\n").unwrap().is_empty());
        assert_eq!(
            parse_lmstudio(
                r#"[{"modelKey":"org/model","type":"llm"},{"modelKey":"embed","type":"embedding"}]"#
            )
            .unwrap(),
            ["org/model"]
        );
        assert!(parse_lmstudio("{}").is_err());
        assert_eq!(
            parse_edge0(concat!(
                "edge0-35b  (port 8000, target 20 tok/s, peak \u{2248} 3000 MB)\n",
                "  experts=128 top_k=4 quant=4bit/g64\n",
                "  prerouter: start=4 hidden=512\n",
                "edge0-8b  (port 8000, target 25 tok/s, peak \u{2248} 900 MB)\n",
                "  lora: recover (r=16 alpha=32)\n",
            ))
            .unwrap(),
            ["edge0-35b", "edge0-8b"]
        );
        assert!(parse_edge0("").unwrap().is_empty());
        assert!(parse_edge0("  experts=128\n").is_err());
    }

    #[test]
    fn config_selects_runtime_without_cloud_keys() {
        let config: LocalConfig = toml::from_str("provider = 'ollama'").unwrap();
        assert_eq!(config.endpoint(), "http://localhost:11434/v1");
        assert_eq!(config.command(), ["ollama", "list"]);
        assert_eq!(config.port(), Some(11434));
        assert_eq!(config.memory_budget_mb, None);
        config.validate().unwrap();
        let serve: LocalConfig = toml::from_str("provider = 'mlx_serve'").unwrap();
        assert_eq!(serve.endpoint(), "http://localhost:11234/v1");
        assert_eq!(serve.command(), ["mlx-serve", "list"]);
        assert_eq!(serve.port(), Some(11234));
        assert_eq!(
            LocalRuntime::parse("mlx-serve"),
            Some(LocalRuntime::MlxServe)
        );
        let invalid: LocalConfig = toml::from_str("provider = 'mlx'\nlist_command = []").unwrap();
        assert!(invalid.validate().is_err());
    }

    #[test]
    fn spec_parsing_selects_disables_and_carries_runtime_settings() {
        let current = LocalConfig {
            provider: LocalRuntime::Ollama,
            endpoint: Some("http://127.0.0.1:9999/v1".into()),
            list_command: Some(vec!["my-list".into()]),
            memory_budget_mb: Some(4096),
        };
        // Re-selecting a runtime keeps the settings written for it; an
        // explicit endpoint wins; switching runtimes drops both.
        let same = LocalConfig::from_spec("ollama", Some(&current))
            .unwrap()
            .unwrap();
        assert_eq!(same.endpoint(), "http://127.0.0.1:9999/v1");
        assert_eq!(same.list_command, current.list_command);
        let edge0 = LocalConfig::from_spec("edge0", Some(&current))
            .unwrap()
            .unwrap();
        assert_eq!(edge0.provider, LocalRuntime::Edge0);
        assert_eq!(edge0.endpoint(), "http://localhost:8000/v1");
        assert_eq!(edge0.command(), ["edge0", "models"]);
        assert_eq!(edge0.port(), Some(8000));
        let moved = LocalConfig::from_spec("lm-studio", Some(&current))
            .unwrap()
            .unwrap();
        assert_eq!(moved.provider, LocalRuntime::Lmstudio);
        assert_eq!(moved.endpoint(), "http://localhost:1234/v1");
        assert_eq!(moved.list_command, None);
        // The budget is about the machine, so it survives the switch that
        // drops the runtime-specific endpoint and discovery command.
        assert_eq!(moved.memory_budget_mb, Some(4096));
        let retargeted = LocalConfig::from_spec("ollama http://localhost:1234/v1", Some(&current))
            .unwrap()
            .unwrap();
        assert_eq!(retargeted.endpoint(), "http://localhost:1234/v1");
        assert!(
            LocalConfig::from_spec("off", Some(&current))
                .unwrap()
                .is_none()
        );

        assert!(LocalConfig::from_spec("", None).is_err());
        assert!(LocalConfig::from_spec("olama", None).is_err());
        assert!(LocalConfig::from_spec("off now", None).is_err());
        assert!(LocalConfig::from_spec("ollama http://a/v1 extra", None).is_err());
        assert!(LocalConfig::from_spec("ollama file:///models", None).is_err());
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
            memory_budget_mb: None,
        };
        let models = config.list_models().await.unwrap();
        assert_eq!(models.len(), 2);
        assert_eq!(models[0].id, "org/model");
        assert_eq!(models[0].backend, BackendTag::Local);
    }
}
