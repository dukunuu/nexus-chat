//! Embedding selection and capability probing on the existing embed channel.

use crate::provider::embedding::{Embedder, discover, is_gemma};
use crate::provider::local::LocalRuntime;

use super::App;

impl App {
    pub(super) fn configured_embedder(&self) -> Option<Embedder> {
        let model = self.embedding_model.trim();
        if model.is_empty() {
            return None;
        }
        // EmbeddingGemma 2 is served through Ollama's native API. A local
        // choice never falls through to a cloud backend: if the endpoint is
        // invalid there is simply no embedder.
        if let Some(raw) = model.strip_prefix("local:")
            && is_gemma(raw)
            && self
                .saved
                .local
                .as_ref()
                .is_none_or(|c| c.provider == LocalRuntime::Ollama)
        {
            return Embedder::ollama(self.embedding_endpoint(), raw.to_string()).ok();
        }
        self.backends.resolve(model).map(|(p, model)| {
            Embedder::openai(p.logged(self.background_usage_log(), "embed"), model)
        })
    }

    pub(super) fn file_embedder(&self) -> Option<Embedder> {
        let configured = self.configured_embedder()?;
        Some(
            self.embedding_backend
                .as_ref()
                .filter(|b| b.same_route(&configured))
                .cloned()
                .unwrap_or(configured)
                .logged(self.background_usage_log()),
        )
    }

    /// Forget the probed runtime (settings changed, or a fresh scan wants to
    /// re-read the model digest and capabilities). Cached vectors stay: they
    /// are dropped lazily when the new identity differs.
    pub fn reset_embedding_backend(&mut self) {
        self.embed_rx = None;
        self.embedding_backend = None;
        self.embedding_discovery = None;
        self.embedding_chunks.clear();
        self.embedding_chat_job = None;
        self.embedding_status.clear();
    }

    fn embedding_endpoint(&self) -> &str {
        self.saved
            .local
            .as_ref()
            .map_or("http://127.0.0.1:11434", |c| c.endpoint())
    }

    /// Pick the embedding model automatically, without blocking startup or
    /// sending file content: local `EmbeddingGemma` 2 when Ollama has it
    /// installed, otherwise the backend default chosen at construction
    /// (`default_embedding_model`, empty where a backend has none, which leaves
    /// keyword search only). There is no setting to override this.
    pub fn discover_embeddings(&mut self) {
        if self.embed_rx.is_some()
            || self
                .saved
                .local
                .as_ref()
                .is_some_and(|c| c.provider != LocalRuntime::Ollama)
        {
            return;
        }
        let Ok(handle) = tokio::runtime::Handle::try_current() else {
            return;
        };
        let endpoint = self.embedding_endpoint().to_string();
        let selected = std::sync::Arc::new(std::sync::OnceLock::new());
        self.embedding_discovery = Some(selected.clone());
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        self.embed_rx = Some(rx);
        let space = self.active_space.id.clone();
        handle.spawn(async move {
            if let Some(result) = discover(&endpoint).await {
                let _ = selected.set(result);
            }
            let _ = tx.send((space, String::new(), Ok(Vec::new())));
        });
    }

    pub(super) fn probe_embeddings(&mut self, embedder: Embedder) {
        let embedder = embedder.logged(self.background_usage_log());
        let Ok(handle) = tokio::runtime::Handle::try_current() else {
            return;
        };
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        self.embed_rx = Some(rx);
        self.embedding_status = "embeddings: probing…".to_string();
        let space = self.active_space.id.clone();
        handle.spawn(async move {
            let result = embedder
                .prepare()
                .await
                .map(|()| Vec::new())
                .map_err(|e| e.to_string());
            let _ = tx.send((space, String::new(), result));
        });
    }

    pub(super) fn on_embedding_probe(&mut self, result: Result<Vec<(i64, Vec<f32>)>, String>) {
        if let Some(selection) = self.embedding_discovery.take() {
            if let Some((model, embedder)) = selection.get() {
                self.embedding_model.clone_from(model);
                self.embedding_backend = Some(embedder.clone());
            } else {
                // No local candidate: keep the previous default unchanged.
                self.start_embedding();
                return;
            }
        }
        match result {
            Ok(_) => {
                if let Some(backend) = &self.embedding_backend {
                    self.embedding_status = backend.status();
                    if let Some(identity) = backend.identity() {
                        let _ = self
                            .db
                            .invalidate_embedding_identity(&self.active_space.id, &identity.key());
                    }
                }
                self.push_status(self.embedding_status.clone());
                self.refresh_toolbox();
                // The identity is known now: chat backfill need not wait a tick.
                self.next_chat_index = std::time::Instant::now();
                // Images held during the probe can now use the actual capability.
                self.rescan_files_with(&crate::extract::ExtractConfig::load());
            }
            Err(error) => {
                self.embedding_status = format!("embeddings: {error}");
                self.push_status(self.embedding_status.clone());
            }
        }
    }

    pub(super) fn automatic_image_ocr(&self) -> bool {
        self.ocr_images.unwrap_or_else(|| {
            // Hold image OCR during a local probe as well: it must not race a
            // paid caption request before the runtime's capability is known.
            !self.embedding_backend.as_ref().map_or_else(
                || {
                    self.embedding_model
                        .strip_prefix("local:")
                        .is_some_and(is_gemma)
                },
                |b| b.supports_images() || (b.is_ollama() && b.identity().is_none()),
            )
        })
    }
}

#[cfg(test)]
#[path = "embedding_tests.rs"]
mod tests;
