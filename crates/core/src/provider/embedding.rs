//! File-search embeddings: formatting, runtime probing and a versioned cache identity.

use std::io::Cursor;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context as _, Result, ensure};
use base64::Engine as _;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::sync::OnceCell;

use super::openrouter::OpenRouter;

/// Raw inputs; text formatting belongs exclusively to the embedder.
#[derive(Clone, Debug)]
pub enum EmbedInput {
    Text(String),
    Image { bytes: Vec<u8>, mime: String },
}

/// Every property that changes the meaning of a cached vector.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct EmbedIdentity {
    pub backend: String,
    pub model: String,
    pub revision: Option<String>,
    pub dimension: usize,
    pub prefix_scheme: String,
    pub preprocessing_version: String,
}

impl EmbedIdentity {
    /// Stable serialized cache key (contains no credentials).
    #[must_use]
    pub fn key(&self) -> String {
        serde_json::to_string(self).expect("embedding identity is serializable")
    }
}

#[derive(Clone)]
enum Backend {
    OpenAi(OpenRouter),
    Ollama {
        client: reqwest::Client,
        base: String,
    },
}

#[derive(Clone)]
struct Profile {
    identity: EmbedIdentity,
    native: bool,
    images: bool,
}

/// A cloneable embedding backend. Clones share the probed identity and capabilities.
#[derive(Clone)]
pub struct Embedder {
    backend: Backend,
    model: String,
    profile: Arc<OnceCell<Profile>>,
}

impl Embedder {
    #[must_use]
    pub fn openai(provider: OpenRouter, model: String) -> Self {
        let dimension = match model.rsplit('/').next().unwrap_or(&model) {
            "text-embedding-3-small" | "text-embedding-ada-002" => Some(1536),
            "text-embedding-3-large" => Some(3072),
            _ => None,
        };
        let embedder = Self {
            backend: Backend::OpenAi(provider),
            model,
            profile: Arc::default(),
        };
        if let Some(dimension) = dimension {
            let _ = embedder.profile.set(Profile {
                identity: embedder.make_identity(None, dimension, false),
                native: false,
                images: false,
            });
        }
        embedder
    }

    /// Use the configured local endpoint; a trailing `/v1` is removed for native APIs.
    ///
    /// # Errors
    /// Rejects unsafe endpoints and HTTP client construction failures.
    pub fn ollama(endpoint: &str, model: String) -> Result<Self> {
        OpenRouter::local(endpoint)?;
        Ok(Self {
            backend: Backend::Ollama {
                client: reqwest::Client::builder()
                    .connect_timeout(Duration::from_secs(2))
                    .timeout(Duration::from_secs(120))
                    .redirect(reqwest::redirect::Policy::none())
                    .build()?,
                base: endpoint
                    .trim_end_matches('/')
                    .trim_end_matches("/v1")
                    .to_string(),
            },
            model,
            profile: Arc::default(),
        })
    }

    pub(crate) fn logged(mut self, log: Option<crate::db::UsageLog>) -> Self {
        if let Backend::OpenAi(provider) = &mut self.backend {
            *provider = provider.clone().logged(log, "embed");
        }
        self
    }

    /// True when both handles share one probed profile (the same live instance).
    #[must_use]
    pub fn same_instance(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.profile, &other.profile)
    }

    #[must_use]
    pub fn same_route(&self, other: &Self) -> bool {
        self.model == other.model && self.backend_id() == other.backend_id()
    }

    fn backend_id(&self) -> String {
        match &self.backend {
            Backend::OpenAi(p) => p.embedding_backend_id(),
            Backend::Ollama { base, .. } => format!("ollama:{base}"),
        }
    }

    #[must_use]
    pub fn is_gemma(&self) -> bool {
        is_gemma(&self.model)
    }

    #[must_use]
    pub fn is_ollama(&self) -> bool {
        matches!(self.backend, Backend::Ollama { .. })
    }

    #[must_use]
    pub fn identity(&self) -> Option<&EmbedIdentity> {
        self.profile.get().map(|p| &p.identity)
    }

    #[must_use]
    pub fn supports_images(&self) -> bool {
        self.profile.get().is_some_and(|p| p.images)
    }

    #[must_use]
    pub fn status(&self) -> String {
        self.identity().map_or_else(
            || "embeddings: probing…".to_string(),
            |id| {
                let place = if self.is_ollama() {
                    "local"
                } else {
                    &id.backend
                };
                let media = if self.supports_images() {
                    " · images"
                } else {
                    " · text only"
                };
                format!(
                    "embeddings: {place} · {} · {}d{media}",
                    id.model, id.dimension
                )
            },
        )
    }

    fn query_text(&self, text: &str) -> String {
        if self.is_gemma() {
            format!("task: search result | query: {text}")
        } else {
            text.to_string()
        }
    }

    #[must_use]
    pub fn document(&self, title: &str, text: &str) -> EmbedInput {
        EmbedInput::Text(if self.is_gemma() {
            format!("title: {title} | text: {text}")
        } else {
            format!("file: {title}\n{text}")
        })
    }

    /// Probe with synthetic content only; never sends a space's data during discovery.
    ///
    /// # Errors
    /// Reports unavailable runtimes, missing models and invalid responses.
    pub async fn prepare(&self) -> Result<()> {
        self.profile.get_or_try_init(|| self.probe()).await?;
        Ok(())
    }

    async fn probe(&self) -> Result<Profile> {
        let expected = self.is_gemma().then_some(768);
        let mut revision = None;
        let mut native = false;
        let mut images = false;
        let vectors = match &self.backend {
            Backend::OpenAi(provider) => {
                provider
                    .embed(&self.model, vec![self.query_text("probe")])
                    .await?
            }
            Backend::Ollama { client, base } => {
                // /tags supplies the manifest digest; /show supplies model capabilities.
                let tags = client
                    .get(format!("{base}/api/tags"))
                    .send()
                    .await
                    .context("not reachable — is `ollama serve` running?")?;
                if tags.status().is_success() {
                    let tags: Value = tags.json().await?;
                    let installed = tags["models"].as_array().and_then(|models| {
                        models
                            .iter()
                            .find(|m| model_matches(m["name"].as_str().unwrap_or(""), &self.model))
                    });
                    // A missing model would otherwise surface as an opaque 404 from /api/embed.
                    let installed = installed.with_context(|| {
                        format!(
                            "model {} is not installed — run `ollama pull {}`",
                            self.model, self.model
                        )
                    })?;
                    revision = installed["digest"].as_str().map(str::to_owned);
                }
                let show = client
                    .post(format!("{base}/api/show"))
                    .json(&json!({"model": self.model}))
                    .send()
                    .await?;
                let mut vision = false;
                if show.status().is_success() {
                    let show: Value = show.json().await?;
                    vision = show["capabilities"]
                        .as_array()
                        .is_some_and(|caps| caps.iter().any(|c| c == "vision"));
                    if revision.is_none() {
                        revision = show["digest"].as_str().map(str::to_owned);
                    }
                }
                let response = self
                    .native_request(&[EmbedInput::Text(self.query_text("probe"))])
                    .await?;
                if matches!(response.status().as_u16(), 404 | 405 | 501) {
                    self.compatible_text(vec![self.query_text("probe")]).await?
                } else {
                    native = true;
                    let vectors = native_vectors(&json_body(response).await?, 1, expected)?;
                    // A catalog label alone is not proof of an image-capable embedding handler.
                    if vision && self.is_gemma() {
                        let input = probe_image()?;
                        if let Ok(response) = self.native_request(&[input]).await
                            && response.status().is_success()
                            && let Ok(body) = response.json().await
                        {
                            images = native_vectors(&body, 1, expected).is_ok();
                        }
                    }
                    vectors
                }
            }
        };
        let vectors = validate_vectors(vectors, 1, expected)?;
        Ok(Profile {
            identity: self.make_identity(revision, vectors[0].len(), images),
            native,
            images,
        })
    }

    fn make_identity(
        &self,
        revision: Option<String>,
        dimension: usize,
        images: bool,
    ) -> EmbedIdentity {
        EmbedIdentity {
            backend: self.backend_id(),
            model: self.model.clone(),
            revision,
            dimension,
            prefix_scheme: if self.is_gemma() {
                "gemma-search-v1"
            } else {
                "file-v1"
            }
            .to_string(),
            preprocessing_version: if self.is_gemma() {
                // chunk window, image downscale and whether images are embedded at all.
                format!("bytes-1000-overlap-100;png-768-v1;images={images}")
            } else {
                "lines-40;file-prefix-v1".to_string()
            },
        }
    }

    async fn native_request(&self, inputs: &[EmbedInput]) -> Result<reqwest::Response> {
        let Backend::Ollama { client, base } = &self.backend else {
            anyhow::bail!("native embeddings require Ollama");
        };
        let input = inputs
            .iter()
            .map(|input| match input {
                EmbedInput::Text(text) => Ok(json!(text)),
                EmbedInput::Image { bytes, mime } => {
                    ensure!(
                        crate::extract::vision_mime(bytes) == Some(mime.as_str()),
                        "image MIME mismatch"
                    );
                    // Ollama accepts only text/image/audio keys, not a separate MIME field.
                    Ok(json!({"image": base64::engine::general_purpose::STANDARD.encode(bytes)}))
                }
            })
            .collect::<Result<Vec<_>>>()?;
        client
            .post(format!("{base}/api/embed"))
            .json(
                &json!({"model": self.model, "input": input, "truncate": false, "dimensions": 768}),
            )
            .send()
            .await
            .context("not reachable — is `ollama serve` running?")
    }

    async fn compatible_text(&self, texts: Vec<String>) -> Result<Vec<Vec<f32>>> {
        match &self.backend {
            Backend::OpenAi(p) => p.embed(&self.model, texts).await,
            Backend::Ollama { base, .. } => {
                OpenRouter::local(&format!("{base}/v1"))?
                    .embed(&self.model, texts)
                    .await
            }
        }
    }

    /// Embed already formatted documents or validated image inputs.
    ///
    /// # Errors
    /// Rejects unsupported modalities, over-budget text and malformed vectors.
    pub async fn embed(&self, inputs: Vec<EmbedInput>) -> Result<Vec<Vec<f32>>> {
        ensure!(!inputs.is_empty(), "empty embedding batch");
        self.prepare().await?;
        let profile = self.profile.get().expect("prepared profile");
        for input in &inputs {
            match input {
                EmbedInput::Text(text) if self.is_gemma() => {
                    ensure!(
                        text.len() <= 8192 - 32,
                        "embedding input exceeds the 8192-token budget"
                    );
                }
                EmbedInput::Image { .. } => ensure!(
                    profile.images,
                    "image embeddings unsupported; audio/video embeddings are not implemented"
                ),
                EmbedInput::Text(_) => {}
            }
        }
        let count = inputs.len();
        let vectors = if profile.native {
            native_vectors(
                &json_body(self.native_request(&inputs).await?).await?,
                count,
                Some(profile.identity.dimension),
            )?
        } else {
            let texts = inputs
                .into_iter()
                .map(|input| match input {
                    EmbedInput::Text(text) => Ok(text),
                    EmbedInput::Image { .. } => {
                        anyhow::bail!("image embeddings unsupported by text endpoint")
                    }
                })
                .collect::<Result<Vec<_>>>()?;
            self.compatible_text(texts).await?
        };
        validate_vectors(vectors, count, Some(profile.identity.dimension))
    }

    /// Query formatting is applied here, exactly once, never in transport code.
    ///
    /// # Errors
    /// Same as [`Self::embed`].
    pub async fn query(&self, query: &str) -> Result<Vec<f32>> {
        Ok(self
            .embed(vec![EmbedInput::Text(self.query_text(query))])
            .await?
            .remove(0))
    }
}

/// A successful JSON body, or the runtime's own error text (no request data).
async fn json_body(response: reqwest::Response) -> Result<Value> {
    let status = response.status();
    if status.is_success() {
        return Ok(response.json().await?);
    }
    let text = response.text().await.unwrap_or_default();
    let reason = serde_json::from_str::<Value>(&text)
        .ok()
        .and_then(|v| v["error"].as_str().map(str::to_owned))
        .unwrap_or(text);
    anyhow::bail!(
        "embedding runtime returned {status}: {}",
        reason.chars().take(300).collect::<String>()
    )
}

#[must_use]
pub fn is_gemma(model: &str) -> bool {
    model.rsplit('/').next().unwrap_or(model).split(':').next() == Some("embeddinggemma-2")
}

fn model_matches(installed: &str, requested: &str) -> bool {
    installed == requested || installed == format!("{requested}:latest")
}

/// Prefer the text baseline, then the multimodal tag, only after a successful probe.
pub async fn discover(endpoint: &str) -> Option<(String, Embedder)> {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(2))
        .build()
        .ok()?;
    let base = endpoint.trim_end_matches('/').trim_end_matches("/v1");
    let tags: Value = client
        .get(format!("{base}/api/tags"))
        .send()
        .await
        .ok()?
        .error_for_status()
        .ok()?
        .json()
        .await
        .ok()?;
    let models = tags["models"].as_array()?;
    for model in ["embeddinggemma-2:270m", "embeddinggemma-2"] {
        if models
            .iter()
            .any(|m| model_matches(m["name"].as_str().unwrap_or(""), model))
        {
            let embedder = Embedder::ollama(endpoint, model.to_string()).ok()?;
            if embedder.prepare().await.is_ok() {
                return Some((format!("local:{model}"), embedder));
            }
        }
    }
    None
}

pub(super) fn indexed_vectors(
    body: &Value,
    count: usize,
    dimension: Option<usize>,
) -> Result<Vec<Vec<f32>>> {
    let data = body["data"]
        .as_array()
        .context("embeddings response has no data")?;
    ensure!(data.len() == count, "embedding response count mismatch");
    let mut ordered = vec![None; count];
    // Servers that omit `index` everywhere are positional; a mix is malformed.
    let positional = data.iter().all(|item| item.get("index").is_none());
    for (position, item) in data.iter().enumerate() {
        let index = if positional {
            position
        } else {
            usize::try_from(item["index"].as_u64().context("embedding index missing")?)?
        };
        ensure!(
            index < count && ordered[index].is_none(),
            "invalid or duplicate embedding index"
        );
        ordered[index] = Some(parse_vector(&item["embedding"])?);
    }
    validate_vectors(
        ordered
            .into_iter()
            .map(|v| v.expect("all indices present"))
            .collect(),
        count,
        dimension,
    )
}

fn native_vectors(body: &Value, count: usize, dimension: Option<usize>) -> Result<Vec<Vec<f32>>> {
    let vectors = body["embeddings"]
        .as_array()
        .context("embeddings missing")?;
    validate_vectors(
        vectors.iter().map(parse_vector).collect::<Result<_>>()?,
        count,
        dimension,
    )
}

#[allow(clippy::cast_possible_truncation)] // Range and finiteness are checked after conversion.
fn parse_vector(value: &Value) -> Result<Vec<f32>> {
    value
        .as_array()
        .context("embedding is not an array")?
        .iter()
        .map(|n| {
            let n = n.as_f64().context("embedding coordinate is not numeric")? as f32;
            ensure!(n.is_finite(), "non-finite embedding coordinate");
            Ok(n)
        })
        .collect()
}

#[allow(clippy::cast_possible_truncation)] // Normalized coordinates fit in f32.
fn validate_vectors(
    mut vectors: Vec<Vec<f32>>,
    count: usize,
    dimension: Option<usize>,
) -> Result<Vec<Vec<f32>>> {
    ensure!(vectors.len() == count, "embedding response count mismatch");
    let dimension = dimension
        .or_else(|| vectors.first().map(Vec::len))
        .context("no vectors")?;
    ensure!(dimension > 0, "empty embedding vector");
    for v in &mut vectors {
        ensure!(
            v.len() == dimension,
            "embedding dimension mismatch: expected {dimension}"
        );
        ensure!(
            v.iter().all(|x| x.is_finite()),
            "non-finite embedding coordinate"
        );
        let norm = v.iter().map(|x| f64::from(*x).powi(2)).sum::<f64>().sqrt();
        ensure!(norm > 0.0 && norm.is_finite(), "invalid embedding norm");
        for x in v {
            *x = (f64::from(*x) / norm) as f32;
        }
    }
    Ok(vectors)
}

/// Bound compressed size and decoded allocations, then convert to RGB PNG.
/// Google's processor uses 16px patches pooled 3x3, at most 280 soft tokens;
/// a 768px square uses 256 pooled patches. The runtime retains final preprocessing.
///
/// # Errors
/// Rejects oversized, unsupported or malformed images.
pub fn image_input(path: &Path) -> Result<EmbedInput> {
    ensure!(
        std::fs::metadata(path)?.len() <= 20 * 1024 * 1024,
        "image embedding limit is 20 MiB"
    );
    let bytes = std::fs::read(path)?;
    let mut reader = image::ImageReader::new(Cursor::new(bytes)).with_guessed_format()?;
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(16_384);
    limits.max_image_height = Some(16_384);
    limits.max_alloc = Some(128 * 1024 * 1024);
    reader.limits(limits);
    let image = reader.decode()?.thumbnail(768, 768).to_rgb8();
    png_input(&image)
}

fn png_input(image: &image::RgbImage) -> Result<EmbedInput> {
    let mut out = Cursor::new(Vec::new());
    image.write_to(&mut out, image::ImageFormat::Png)?;
    Ok(EmbedInput::Image {
        bytes: out.into_inner(),
        mime: "image/png".to_string(),
    })
}

/// A small synthetic picture (never user data) large enough for a vision
/// encoder's patching, used only to test that the runtime accepts images.
#[allow(clippy::cast_possible_truncation)] // x, y < 64, so x * 4 fits in u8.
fn probe_image() -> Result<EmbedInput> {
    png_input(&image::RgbImage::from_fn(64, 64, |x, y| {
        image::Rgb([(x * 4) as u8, (y * 4) as u8, 128])
    }))
}

#[cfg(test)]
#[path = "embedding_tests.rs"]
pub(crate) mod tests;
