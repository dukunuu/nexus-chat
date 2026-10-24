//! Keyless web search and failover. Endpoints and the Exa budget are shared
//! by toolbox copies so a parallel research run cannot fan out unboundedly.

use super::*;
use crate::provider::BackendTag;
use serde_json::Value;

#[cfg(not(test))]
const EXA_MCP: &str = "https://mcp.exa.ai/mcp";
#[cfg(test)]
const EXA_MCP: &str = "http://127.0.0.1:1/mcp";
#[cfg(not(test))]
const LANGSEARCH: &str = "https://api.langsearch.com/v1/web-search";
#[cfg(test)]
const LANGSEARCH: &str = "http://127.0.0.1:1/web-search";
#[cfg(not(test))]
const BRAVE: &str = "https://search.brave.com/search";
#[cfg(test)]
const BRAVE: &str = "http://127.0.0.1:1/brave";

pub(super) struct WebSearch {
    pub exa: String,
    pub langsearch: String,
    pub duckduckgo: String,
    pub brave: String,
    permits: tokio::sync::Semaphore,
}

impl Default for WebSearch {
    fn default() -> Self {
        Self {
            exa: EXA_MCP.into(),
            langsearch: LANGSEARCH.into(),
            duckduckgo: DUCKDUCKGO_HTML.into(),
            brave: BRAVE.into(),
            permits: tokio::sync::Semaphore::new(2),
        }
    }
}

#[derive(Debug)]
pub(super) struct SearchResults {
    hits: Vec<SearchHit>,
    source: Option<&'static str>,
    recency_unapplied: bool,
}

impl SearchResults {
    pub(super) fn format(self, query: &str, discussions: &[DiscussionHit]) -> String {
        let results = format_web_results(query, self.hits, discussions);
        let Some(source) = self.source else {
            return results;
        };
        let recency = if self.recency_unapplied {
            "; requested recency filter not applied"
        } else {
            ""
        };
        format!("via {source}{recency}\n\n{results}")
    }
}

impl ToolBox {
    pub(super) async fn search(
        &self,
        query: &str,
        recency: Option<&str>,
        include: &[String],
        exclude: &[String],
    ) -> anyhow::Result<SearchResults> {
        let rewritten = rewrite_query_with_domains(query, include, exclude);
        let choices: &[&str] = match self.search_provider.as_str() {
            "langsearch" => &["langsearch"],
            "searxng" => &["searxng"],
            "exa" => &["exa"],
            "provider" => &["provider"],
            "duckduckgo" => &["duckduckgo"],
            _ => &[
                "langsearch",
                "searxng",
                "exa",
                "provider",
                "duckduckgo",
                "brave",
            ],
        };
        let auto = choices.len() > 1;
        let mut failures = Vec::new();
        for &backend in choices {
            if auto
                && ((backend == "langsearch" && self.langsearch_key.is_none())
                    || (backend == "searxng" && self.searxng_url.is_none()))
            {
                continue;
            }
            // Bound each attempt independently: a timeout must still allow
            // later backends to run and appear in an all-failed diagnostic.
            // Provider search runs a model, and Exa may first queue for a
            // permit behind a research run's other searches (its request
            // has its own 15 s limit), so both get longer.
            let limit = match backend {
                "provider" | "exa" => 60,
                _ => 15,
            };
            let attempt = tokio::time::timeout(
                Duration::from_secs(limit),
                self.search_using(backend, &rewritten, recency, include, exclude),
            )
            .await;
            let result = match attempt {
                Ok(result) => result,
                Err(_) => Err(anyhow::anyhow!("timed out after {limit} seconds")),
            };
            match result {
                Ok(result) if !result.hits.is_empty() => return Ok(result),
                Ok(_) => failures.push(format!("{} returned no results", backend_name(backend))),
                Err(error) => failures.push(format!("{}: {error}", backend_name(backend))),
            }
        }
        anyhow::bail!(
            "all web search backends failed: {}. Try again in a minute.",
            failures.join("; ")
        )
    }

    async fn search_using(
        &self,
        backend: &str,
        query: &str,
        recency: Option<&str>,
        include: &[String],
        exclude: &[String],
    ) -> anyhow::Result<SearchResults> {
        let mut source = None;
        let mut hits = match backend {
            "langsearch" => {
                let key = self.langsearch_key.as_deref().ok_or_else(|| {
                    anyhow::anyhow!("LangSearch selected but no API key is configured")
                })?;
                langsearch_search(
                    &self.client,
                    &self.web_search.langsearch,
                    key,
                    query,
                    recency,
                )
                .await?
            }
            "searxng" => {
                let url = self.searxng_url.as_deref().ok_or_else(|| {
                    anyhow::anyhow!("SearXNG selected but no instance URL is configured")
                })?;
                searxng_search(&self.client, url, query, recency).await?
            }
            "exa" => {
                source = Some("Exa (keyless)");
                let _permit = self.web_search.permits.acquire().await?;
                let response = self.client.post(&self.web_search.exa)
                    .header(reqwest::header::ACCEPT, "application/json, text/event-stream")
                    .timeout(Duration::from_secs(15))
                    .json(&serde_json::json!({
                        "jsonrpc": "2.0", "id": 1, "method": "tools/call",
                        "params": {"name": "web_search_exa", "arguments": {
                            "query": query, "numResults": 8,
                            "objective": format!("Find sources for: {}", query.chars().take(200).collect::<String>())
                        }}
                    }))
                    .send().await?.error_for_status()?.text().await?;
                parse_exa(&response)?
            }
            "provider" => {
                let (provider, model) = self
                    .search_backend
                    .as_ref()
                    .ok_or_else(|| anyhow::anyhow!("no session provider is configured"))?;
                source = Some(match provider.backend_tag() {
                    BackendTag::OpenRouter => "OpenRouter web search",
                    BackendTag::Codex => "Codex web search",
                    BackendTag::OpenAi => "OpenAI web search",
                    BackendTag::OpencodeGo | BackendTag::Local => {
                        anyhow::bail!("session provider has no built-in web search")
                    }
                });
                let response = provider.web_search(model, query, include, exclude).await?;
                if let Some(usage) = response
                    .get("usage")
                    .and_then(crate::provider::openrouter::parse_usage_value)
                {
                    self.record_search_usage(provider, model, usage);
                }
                parse_provider_hits(&response, provider.backend_tag())
            }
            "duckduckgo" => {
                duckduckgo_search(&self.client, &self.web_search.duckduckgo, query).await?
            }
            _ => brave_search(&self.client, &self.web_search.brave, query).await?,
        };
        if let Some(key) = self.langsearch_key.as_deref().filter(|key| !key.is_empty()) {
            for hit in &mut hits {
                hit.title = hit.title.replace(key, "[redacted]");
                hit.url = hit.url.replace(key, "[redacted]");
                hit.snippet = hit.snippet.replace(key, "[redacted]");
            }
        }
        Ok(SearchResults {
            hits,
            source,
            recency_unapplied: recency.is_some() && source.is_some(),
        })
    }

    fn record_search_usage(
        &self,
        provider: &OpenRouter,
        model: &str,
        usage: crate::provider::Usage,
    ) {
        if !self.record_events {
            return;
        }
        let Some(path) = &self.db_path else { return };
        let Ok(db) = crate::db::Db::open(path) else {
            return;
        };
        let cost = usage.cost.or_else(|| {
            db.request_cost(
                model,
                usage.prompt_tokens,
                usage.completion_tokens,
                usage.cache_read_tokens,
                usage.cache_creation_tokens,
            )
        });
        let _ = db.log_usage(
            provider.backend_tag().name(),
            model,
            usage.prompt_tokens,
            usage.completion_tokens,
            usage.cache_read_tokens,
            usage.cache_creation_tokens,
            usage.prompt_convention,
            cost,
            usage.cost.is_some(),
            Some(&self.session_id),
            Some(&self.search_space_id),
        );
    }
}

fn backend_name(backend: &str) -> &str {
    match backend {
        "langsearch" => "LangSearch",
        "searxng" => "SearXNG",
        "exa" => "Exa",
        "provider" => "provider web search",
        "duckduckgo" => "DuckDuckGo",
        _ => "Brave",
    }
}

fn snippet(text: &str) -> String {
    text.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .chars()
        .take(500)
        .collect()
}

fn parse_exa(body: &str) -> anyhow::Result<Vec<SearchHit>> {
    let response: Value = if let Ok(value) = serde_json::from_str(body) {
        value
    } else {
        body.replace("\r\n", "\n")
            .split("\n\n")
            .filter_map(|event| {
                let data = event
                    .lines()
                    .filter_map(|line| line.strip_prefix("data:"))
                    .map(str::trim_start)
                    .collect::<Vec<_>>()
                    .join("\n");
                serde_json::from_str::<Value>(&data).ok()
            })
            .find(|value| value.get("result").is_some() || value.get("error").is_some())
            .ok_or_else(|| anyhow::anyhow!("invalid Exa response"))?
    };
    anyhow::ensure!(
        response.get("error").is_none_or(Value::is_null),
        "Exa JSON-RPC error"
    );
    anyhow::ensure!(response["result"]["isError"] != true, "Exa tool error");
    let mut hits = Vec::new();
    if let Some(content) = response["result"]["content"].as_array() {
        for text in content.iter().filter_map(|part| part["text"].as_str()) {
            // A hit starts at a line beginning "Title: "; the words inside
            // highlights must not start a new one.
            let text = format!("\n{text}");
            for block in text.split("\nTitle: ").skip(1) {
                let mut lines = block.lines();
                let title = lines.next().unwrap_or_default().trim().to_string();
                let mut url = None;
                let mut published = None;
                let mut highlights = Vec::new();
                let mut in_highlights = false;
                for line in lines {
                    if let Some(value) = line.strip_prefix("URL: ") {
                        url = Some(value.trim().to_string());
                    } else if let Some(value) = line.strip_prefix("Published: ") {
                        published = (value.trim() != "N/A").then(|| value.trim().to_string());
                    } else if let Some(value) = line.strip_prefix("Highlights:") {
                        in_highlights = true;
                        highlights.push(value);
                    } else if in_highlights {
                        highlights.push(line);
                    }
                }
                if let Some(url) =
                    url.filter(|url| url.starts_with("https://") || url.starts_with("http://"))
                    && !title.is_empty()
                {
                    let highlights = snippet(&highlights.join(" "));
                    hits.push(SearchHit {
                        title,
                        url,
                        snippet: published.map_or_else(
                            || highlights.clone(),
                            |date| snippet(&format!("{date} · {highlights}")),
                        ),
                        image: None,
                    });
                }
            }
        }
    }
    Ok(hits.into_iter().take(8).collect())
}

fn parse_provider_hits(response: &Value, tag: BackendTag) -> Vec<SearchHit> {
    let mut hits = Vec::new();
    if tag == BackendTag::OpenRouter {
        if let Some(annotations) = response
            .pointer("/choices/0/message/annotations")
            .and_then(Value::as_array)
        {
            for annotation in annotations {
                if annotation["type"] == "url_citation" {
                    add_citation(&mut hits, &annotation["url_citation"], "");
                }
            }
        }
    } else if let Some(output) = response["output"].as_array() {
        for item in output {
            for content in item["content"].as_array().into_iter().flatten() {
                let text = content["text"].as_str().unwrap_or_default();
                for annotation in content["annotations"].as_array().into_iter().flatten() {
                    if annotation["type"] != "url_citation" {
                        continue;
                    }
                    // Responses indices are character offsets, not UTF-8 byte offsets.
                    let start = annotation["start_index"]
                        .as_u64()
                        .and_then(|n| usize::try_from(n).ok())
                        .unwrap_or(0);
                    let end = annotation["end_index"]
                        .as_u64()
                        .and_then(|n| usize::try_from(n).ok())
                        .unwrap_or(start);
                    let span: String = text
                        .chars()
                        .skip(start)
                        .take(end.saturating_sub(start))
                        .collect();
                    add_citation(&mut hits, annotation, &span);
                }
            }
        }
    }
    hits.truncate(8);
    hits
}

fn add_citation(hits: &mut Vec<SearchHit>, citation: &Value, span: &str) {
    let Some(url) = citation["url"].as_str() else {
        return;
    };
    if !(url.starts_with("https://") || url.starts_with("http://"))
        || hits.iter().any(|hit| hit.url == url)
    {
        return;
    }
    hits.push(SearchHit {
        title: citation["title"].as_str().unwrap_or(url).to_string(),
        url: url.to_string(),
        snippet: snippet(citation["content"].as_str().unwrap_or(span)),
        image: None,
    });
}
