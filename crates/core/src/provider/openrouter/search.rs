//! Native server-side search, using only this client's backend and credentials.

use super::{OpenRouter, ProviderFlavor, sse_event_data};
use anyhow::{Context as _, Result};

impl OpenRouter {
    #[cfg(test)]
    pub(crate) fn with_search_endpoint(mut self, endpoint: String) -> Self {
        self.search_endpoint = Some(endpoint);
        self
    }

    pub(crate) async fn web_search(
        &self,
        model: &str,
        query: &str,
        allowed: &[String],
        excluded: &[String],
    ) -> Result<serde_json::Value> {
        use crate::provider::BackendTag;
        anyhow::ensure!(
            !matches!(
                self.backend_tag(),
                BackendTag::Local | BackendTag::OpencodeGo
            ),
            "session provider has no built-in web search"
        );
        #[cfg(not(test))]
        let base = self.api_base();
        #[cfg(test)]
        let base = self
            .search_endpoint
            .as_deref()
            .unwrap_or("http://127.0.0.1:1");
        let prompt = format!("Search the web for: {query}\nList what you found with citations.");
        let (path, body) = if self.flavor == ProviderFlavor::OpenRouter {
            (
                "chat/completions",
                serde_json::json!({
                    "model": model, "stream": false,
                    "messages": [{"role": "user", "content": prompt}],
                    "tools": [{"type": "openrouter:web_search", "parameters": {
                        "engine": "exa", "max_results": 8,
                        "allowed_domains": allowed, "excluded_domains": excluded
                    }}],
                    "max_tool_calls": 1, "usage": {"include": true}
                }),
            )
        } else {
            (
                if self.flavor == ProviderFlavor::OpenAiCodex {
                    "codex/responses"
                } else {
                    "responses"
                },
                serde_json::json!({
                    "model": model, "store": false,
                    "stream": self.flavor == ProviderFlavor::OpenAiCodex,
                    "instructions": "Search the web and cite the sources you find.",
                    "input": [{"role": "user", "content": [{"type": "input_text", "text": prompt}]}],
                    "tools": [{"type": "web_search"}]
                }),
            )
        };
        let request = self.client.post(format!("{base}/{path}")).json(&body);
        let request = if self.flavor == ProviderFlavor::OpenAiCodex {
            request.headers(self.codex_headers(true)?)
        } else {
            self.authenticate(request)
        };
        let response = request.send().await?.error_for_status()?.text().await?;
        // Never expose a credential even if an upstream error or citation echoes it.
        let response = if self.key.is_empty() {
            response
        } else {
            response.replace(&self.key, "[redacted]")
        };
        let value = if self.flavor == ProviderFlavor::OpenAiCodex {
            if let Ok(value) = serde_json::from_str::<serde_json::Value>(&response) {
                value
            } else {
                let mut completed = None;
                // The final response can arrive with an empty `output`; the
                // items themselves come one per `output_item.done` event.
                let mut items = Vec::new();
                for event in response.replace("\r\n", "\n").split("\n\n") {
                    let data = sse_event_data(event);
                    let Ok(value) = serde_json::from_str::<serde_json::Value>(&data) else {
                        continue;
                    };
                    match value["type"].as_str() {
                        Some("response.output_item.done") => {
                            items.extend(value.get("item").cloned());
                        }
                        Some("response.completed") => completed = value.get("response").cloned(),
                        Some("response.failed" | "response.incomplete" | "error") => {
                            anyhow::bail!("provider web search failed")
                        }
                        _ => {}
                    }
                }
                let mut completed = completed
                    .context("provider web search stream ended without a completed response")?;
                if completed["output"].as_array().is_none_or(Vec::is_empty) {
                    completed["output"] = serde_json::Value::Array(items);
                }
                completed
            }
        } else {
            serde_json::from_str(&response)?
        };
        anyhow::ensure!(
            value.get("error").is_none_or(serde_json::Value::is_null),
            "provider web search error"
        );
        Ok(value)
    }
}
