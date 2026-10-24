//! Loopback-only tests at the search tool / HTTP boundary.
use super::*;
use crate::provider::BackendTag;
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt as _, AsyncReadExt as _, AsyncWriteExt as _, BufReader};

struct Request {
    method: String,
    path: String,
    headers: String,
    body: Value,
}

struct Mock {
    base: String,
    requests: Arc<Mutex<Vec<Request>>>,
    peak: Arc<AtomicU64>,
    task: tokio::task::JoinHandle<()>,
}

impl Drop for Mock {
    fn drop(&mut self) {
        self.task.abort();
    }
}

async fn mock(handler: impl Fn(&Request) -> (u16, String) + Send + Sync + 'static) -> Mock {
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
        .await
        .unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let requests = Arc::new(Mutex::new(Vec::new()));
    let recorded = requests.clone();
    let peak = Arc::new(AtomicU64::new(0));
    let maximum = peak.clone();
    let active = Arc::new(AtomicU64::new(0));
    let handler = Arc::new(handler);
    let task = tokio::spawn(async move {
        loop {
            let (stream, _) = listener.accept().await.unwrap();
            let (recorded, maximum, active, handler) = (
                recorded.clone(),
                maximum.clone(),
                active.clone(),
                handler.clone(),
            );
            tokio::spawn(async move {
                let mut stream = BufReader::new(stream);
                let mut first = String::new();
                stream.read_line(&mut first).await.unwrap();
                let mut words = first.split_whitespace();
                let method = words.next().unwrap().to_string();
                let path = words.next().unwrap().to_string();
                let mut headers = String::new();
                let mut length = 0;
                loop {
                    let mut line = String::new();
                    assert!(stream.read_line(&mut line).await.unwrap() > 0);
                    if line == "\r\n" {
                        break;
                    }
                    if let Some((key, value)) = line.split_once(':')
                        && key.eq_ignore_ascii_case("content-length")
                    {
                        length = value.trim().parse().unwrap();
                    }
                    headers.push_str(&line);
                }
                let mut body = vec![0; length];
                stream.read_exact(&mut body).await.unwrap();
                let request = Request {
                    method,
                    path,
                    headers,
                    body: serde_json::from_slice(&body).unwrap_or(Value::Null),
                };
                let (status, body) = handler(&request);
                recorded.lock().unwrap().push(request);
                let count = active.fetch_add(1, Ordering::SeqCst) + 1;
                maximum.fetch_max(count, Ordering::SeqCst);
                tokio::time::sleep(Duration::from_millis(40)).await;
                let response = format!(
                    "HTTP/1.1 {status} Mock\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                stream
                    .get_mut()
                    .write_all(response.as_bytes())
                    .await
                    .unwrap();
                active.fetch_sub(1, Ordering::SeqCst);
            });
        }
    });
    Mock {
        base,
        requests,
        peak,
        task,
    }
}

fn exa_reply() -> Value {
    json!({"jsonrpc":"2.0", "id":1, "result":{"content":[{"type":"text", "text":
        "Title: Rust reference\nURL: https://doc.rust-lang.org/reference/\nPublished: 2026-10-23\nAuthor: Team\nHighlights:\n  Rust  documentation\n  with examples.\n\nTitle: Rust guide\nURL: https://example.org/rust\nPublished: N/A\nAuthor: N/A\nHighlights:\nAnother source."
    }]}})
}

fn native_reply() -> Value {
    json!({"choices":[{"message":{"annotations":[
        {"type":"url_citation", "url_citation":{"url":"https://example.org/rust", "title":"Rust source", "content":"Rust documentation"}}
    ]}}], "usage":{"prompt_tokens":10, "completion_tokens":4, "cost":0.0073}})
}

fn toolbox(server: &Mock) -> ToolBox {
    let mut tb = ToolBox::research(None, None, "auto".into(), vec![], None);
    let endpoints = Arc::get_mut(&mut tb.web_search).unwrap();
    endpoints.exa = format!("{}/exa", server.base);
    endpoints.langsearch = format!("{}/langsearch", server.base);
    endpoints.duckduckgo = format!("{}/ddg", server.base);
    endpoints.brave = format!("{}/brave", server.base);
    tb.search_backend = Some((
        OpenRouter::openrouter_flavor("fixture-provider-key".into())
            .with_search_endpoint(format!("{}/native", server.base)),
        "openai/helper".into(),
    ));
    tb
}

async fn search(tb: &ToolBox) -> String {
    tb.run("search", r#"{"mode":"web","query":"Rust documentation"}"#)
        .await
        .0
}

#[tokio::test]
async fn exa_sse_and_json_are_keyless_search_results_with_dates_and_recency_notice() {
    for sse in [false, true] {
        let server = mock(move |_| {
            (
                200,
                if sse {
                    format!("event: message\r\ndata: {}\r\n\r\n", exa_reply())
                } else {
                    exa_reply().to_string()
                },
            )
        })
        .await;
        let mut tb = toolbox(&server);
        tb.search_provider = "exa".into();
        let output = tb
            .run(
                "search",
                r#"{"mode":"web","query":"Rust documentation","recency":"week"}"#,
            )
            .await
            .0;
        assert!(
            output.starts_with("via Exa (keyless); requested recency filter not applied\n"),
            "{output}"
        );
        assert!(output.contains("[1] Rust reference\n    https://doc.rust-lang.org/reference/\n    2026-10-23 · Rust documentation with examples."), "{output}");
        assert!(
            output.contains("[2] Rust guide\n    https://example.org/rust\n    Another source.")
        );
        assert!(!output.contains("N/A"));
        let sources = crate::app::tool_call_view(
            "search",
            r#"{"mode":"web","query":"Rust documentation"}"#,
            &output,
        )
        .sources;
        assert_eq!(sources.len(), 2);
        let requests = server.requests.lock().unwrap();
        assert_eq!(requests.len(), 1); // no initialize handshake
        let request = &requests[0];
        assert_eq!(request.method, "POST");
        assert_eq!(request.path, "/exa");
        assert!(
            request
                .headers
                .contains("accept: application/json, text/event-stream")
        );
        assert!(request.headers.contains("content-type: application/json"));
        assert!(!request.headers.contains("authorization"));
        assert_eq!(request.body["jsonrpc"], "2.0");
        assert_eq!(request.body["method"], "tools/call");
        assert_eq!(request.body["params"]["name"], "web_search_exa");
        let args = &request.body["params"]["arguments"];
        assert_eq!(args["query"], "Rust documentation");
        assert_eq!(args["numResults"], 8);
        assert!(
            args["objective"]
                .as_str()
                .unwrap()
                .contains("Rust documentation")
        );
        assert!(args.get("recency").is_none());
        assert!(!output.contains("fixture-provider-key"));
    }
}

#[tokio::test]
async fn exa_failures_continue_to_the_same_session_provider() {
    for (status, body) in [
        (
            200,
            json!({"result":{"isError":true,"content":[]}}).to_string(),
        ),
        (
            200,
            json!({"error":{"code":-32603,"message":"failed"}}).to_string(),
        ),
        (429, "limited".into()),
        (503, "unavailable".into()),
        (
            200,
            json!({"result":{"content":[{"text":"not search results"}]}}).to_string(),
        ),
    ] {
        let server = mock(move |r| {
            if r.path == "/exa" {
                (status, body.clone())
            } else {
                (200, native_reply().to_string())
            }
        })
        .await;
        let output = search(&toolbox(&server)).await;
        assert!(
            output.starts_with("via OpenRouter web search\n"),
            "{output}"
        );
        assert!(output.contains("Rust source"));
        let requests = server.requests.lock().unwrap();
        assert_eq!(
            requests.iter().map(|r| r.path.as_str()).collect::<Vec<_>>(),
            ["/exa", "/native/chat/completions"]
        );
    }
}

#[tokio::test]
async fn exa_parallel_calls_share_a_two_request_budget_even_across_toolbox_copies() {
    let server = mock(|_| (200, exa_reply().to_string())).await;
    let mut tb = toolbox(&server);
    tb.search_provider = "exa".into();
    let tb = Arc::new(tb);
    let copy = Arc::new(tb.copy(tb.clone()));
    let mut tasks = tokio::task::JoinSet::new();
    for i in 0..8 {
        let tb = if i % 2 == 0 { tb.clone() } else { copy.clone() };
        tasks.spawn(async move { search(&tb).await });
    }
    while let Some(output) = tasks.join_next().await {
        assert!(output.unwrap().starts_with("via Exa"));
    }
    assert_eq!(server.requests.lock().unwrap().len(), 8);
    assert_eq!(server.peak.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn openrouter_server_tool_receives_domain_filters_and_returns_annotations() {
    let server = mock(|_| (200, native_reply().to_string())).await;
    let mut tb = toolbox(&server);
    tb.search_provider = "provider".into();
    tb.blocked_domains = vec!["blocked.test".into()];
    let output = tb.run("search", r#"{"mode":"web","query":"Rust documentation","include_domains":["rust-lang.org"],"exclude_domains":["spam.test"]}"#).await.0;
    assert!(
        output.contains("[1] Rust source\n    https://example.org/rust\n    Rust documentation")
    );
    let requests = server.requests.lock().unwrap();
    assert_eq!(requests.len(), 1);
    let request = &requests[0];
    assert_eq!(request.method, "POST");
    assert_eq!(request.path, "/native/chat/completions");
    assert!(
        request
            .headers
            .contains("authorization: Bearer fixture-provider-key")
    );
    assert_eq!(request.body["model"], "openai/helper");
    assert_eq!(request.body["max_tool_calls"], 1);
    assert_eq!(
        request.body["tools"],
        json!([{"type":"openrouter:web_search", "parameters":{
            "engine":"exa", "max_results":8, "allowed_domains":["rust-lang.org"], "excluded_domains":["spam.test","blocked.test"]
        }}])
    );
    assert!(
        request.body["messages"][0]["content"]
            .as_str()
            .unwrap()
            .contains("Rust documentation")
    );
    assert!(!request.path.contains("fixture-provider-key"));
    assert!(!output.contains("fixture-provider-key"));
}

fn responses_reply() -> Value {
    json!({"error":null,"output":[{"type":"message","content":[{"type":"output_text","text":"é Rust documentation cited here.","annotations":[
        {"type":"url_citation","url":"https://example.org/rust","title":"Rust docs","start_index":2,"end_index":20},
        {"type":"url_citation","url":"https://example.org/rust","title":"duplicate","start_index":0,"end_index":1}
    ]}]}], "usage":{"input_tokens":20,"output_tokens":10}})
}

#[tokio::test]
async fn openai_and_codex_use_responses_web_search_and_cited_character_spans() {
    for codex in [false, true] {
        let server = mock(move |_| {
            (
                200,
                if codex {
                    format!(
                        "event: response.completed\ndata: {}\n\n",
                        json!({"type":"response.completed","response":responses_reply()})
                    )
                } else {
                    responses_reply().to_string()
                },
            )
        })
        .await;
        let payload = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .encode(r#"{"https://api.openai.com/auth":{"chatgpt_account_id":"fixture-account"}}"#);
        let token = format!("header.{payload}.signature");
        let provider = if codex {
            OpenRouter::openai_codex(token.clone())
        } else {
            OpenRouter::openai("fixture-openai-key".into())
        };
        let mut tb = toolbox(&server);
        tb.search_provider = "provider".into();
        tb.search_backend = Some((
            provider.with_search_endpoint(server.base.clone()),
            "helper-on-session".into(),
        ));
        let output = search(&tb).await;
        assert!(
            output.starts_with(if codex {
                "via Codex web search"
            } else {
                "via OpenAI web search"
            }),
            "{output}"
        );
        assert!(
            output.contains("[1] Rust docs\n    https://example.org/rust\n    Rust documentation"),
            "{output}"
        );
        assert!(!output.contains("duplicate"));
        assert!(!output.contains(&token));
        let requests = server.requests.lock().unwrap();
        assert_eq!(requests.len(), 1);
        let r = &requests[0];
        assert_eq!(r.method, "POST");
        assert_eq!(
            r.path,
            if codex {
                "/codex/responses"
            } else {
                "/responses"
            }
        );
        assert_eq!(r.body["tools"], json!([{"type":"web_search"}]));
        assert_eq!(r.body["model"], "helper-on-session");
        assert_eq!(r.body["store"], false);
        assert_eq!(r.body["stream"], codex);
        assert!(!r.body["instructions"].as_str().unwrap().is_empty());
        assert!(
            r.body["input"][0]["content"][0]["text"]
                .as_str()
                .unwrap()
                .contains("Rust documentation")
        );
        if codex {
            assert!(r.headers.contains("chatgpt-account-id: fixture-account"));
            assert!(r.headers.contains("accept: text/event-stream"));
            assert!(r.headers.contains("originator: nexus-chat"));
        } else {
            assert!(
                r.headers
                    .contains("authorization: Bearer fixture-openai-key")
            );
        }
        assert!(!r.path.contains("fixture-"));
    }
}

const DDG: &str = r#"<a class="result__a" href="https://example.org/rust">Rust docs</a><a class="result__snippet">Rust documentation</a>"#;
const BRAVE: &str = r#"<a href="https://example.org/rust"><div class="title search-snippet-title">Rust docs</div></a><div class="generic-snippet"><div class="content">Rust documentation</div></div>"#;

#[tokio::test]
async fn auto_tries_configured_backends_then_exa_provider_and_scrapers() {
    let paths = [
        "/langsearch",
        "/searx/search",
        "/exa",
        "/native/chat/completions",
        "/ddg",
        "/brave",
    ];
    for stop in 0..paths.len() {
        let server = mock(move |r| {
            if r.path.split('?').next().unwrap() != paths[stop] { return (503, "offline".into()); }
            (200, match stop {
                0 => json!({"data":{"webPages":{"value":[{"name":"Rust docs","url":"https://example.org/rust","snippet":"Rust documentation"}]}}}).to_string(),
                1 => json!({"results":[{"title":"Rust docs","url":"https://example.org/rust","content":"Rust documentation"}]}).to_string(),
                2 => exa_reply().to_string(),
                3 => native_reply().to_string(),
                4 => DDG.into(),
                _ => BRAVE.into(),
            })
        }).await;
        let mut tb = toolbox(&server);
        tb.langsearch_key = Some("fixture-langsearch-key".into());
        tb.searxng_url = Some(format!("{}/searx", server.base));
        let output = search(&tb).await;
        assert!(!output.starts_with("search failed"), "{stop}: {output}");
        let requests = server.requests.lock().unwrap();
        assert_eq!(
            requests
                .iter()
                .map(|r| r.path.split('?').next().unwrap())
                .collect::<Vec<_>>(),
            paths[..=stop]
        );
        assert!(requests.iter().all(|r| !r.path.contains("fixture-")));
        assert!(!output.contains("fixture-"));
    }
}

#[tokio::test]
async fn go_and_local_skip_native_search_and_all_failures_list_sources_and_one_hint() {
    for local in [false, true] {
        let server = mock(|_| (429, "upstream captcha".into())).await;
        let mut tb = toolbox(&server);
        let provider = if local {
            OpenRouter::local(&server.base).unwrap()
        } else {
            OpenRouter::opencode_go("fixture-go-key".into())
        };
        tb.search_backend = Some((
            provider.with_search_endpoint(format!("{}/native", server.base)),
            "model".into(),
        ));
        let output = search(&tb).await;
        assert!(
            output.starts_with("search failed: all web search backends failed:"),
            "{output}"
        );
        assert!(output.contains("Exa:"));
        assert!(
            output.contains("provider web search: session provider has no built-in web search")
        );
        assert!(output.contains("DuckDuckGo:"));
        assert!(output.contains("Brave:"));
        assert_eq!(output.matches("Try again in a minute.").count(), 1);
        let requests = server.requests.lock().unwrap();
        assert_eq!(requests.len(), 3);
        assert!(requests.iter().all(|r| !r.path.starts_with("/native")));
    }
}

#[tokio::test]
async fn native_usage_records_reported_cost_and_incognito_does_not_record() {
    let server = mock(|_| (200, native_reply().to_string())).await;
    let root = std::env::temp_dir().join(format!("nexus-keyless-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&root).unwrap();
    let path = root.join("nexus.db");
    let db = crate::db::Db::open(&path).unwrap();
    let mut tb = toolbox(&server);
    tb.search_provider = "provider".into();
    tb.db_path = Some(path);
    tb.record_events = true;
    assert!(search(&tb).await.starts_with("via OpenRouter"));
    let usage = db.usage_totals(None).unwrap();
    assert_eq!(usage.requests, 1);
    assert_eq!(usage.prompt_tokens, 10);
    assert_eq!(usage.completion_tokens, 4);
    assert!((usage.cost - 0.0073).abs() < f64::EPSILON);
    tb.record_events = false;
    search(&tb).await;
    assert_eq!(db.usage_totals(None).unwrap().requests, 1);
    drop(db);
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn snippets_are_collapsed_capped_and_credentials_are_not_returned_in_hits() {
    let server = mock(|_| (200, json!({"choices":[{"message":{"annotations":[{"type":"url_citation","url_citation":{
        "url":"https://example.org/fixture-provider-key", "title":"Rust fixture-provider-key", "content":format!("Rust\n\t{}", "é".repeat(900))
    }}]}}]}).to_string())).await;
    let mut tb = toolbox(&server);
    tb.search_provider = "provider".into();
    let output = search(&tb).await;
    assert!(!output.contains("fixture-provider-key"));
    assert!(!output.contains(&"é".repeat(501)));
    assert!(!output.contains('\t'));
    let request = server.requests.lock().unwrap();
    assert!(!request[0].path.contains("fixture-provider-key"));
}

#[tokio::test]
async fn exa_highlights_are_capped_and_optional_langsearch_credentials_stay_out_of_results() {
    let server = mock(|r| (200, if r.path == "/exa" {
        json!({"result":{"content":[{"text":format!("Title: Rust docs\nURL: https://example.org/rust\nPublished: N/A\nHighlights:\nRust\n\t{}", "é".repeat(900))}]}}).to_string()
    } else {
        json!({"data":{"webPages":{"value":[{"name":"Rust fixture-langsearch-key", "url":"https://example.org/fixture-langsearch-key", "snippet":"Rust fixture-langsearch-key"}]}}}).to_string()
    })).await;
    let mut tb = toolbox(&server);
    tb.search_provider = "exa".into();
    let output = search(&tb).await;
    assert!(!output.contains(&"é".repeat(501)));
    assert!(!output.contains('\t'));
    tb.search_provider = "langsearch".into();
    tb.langsearch_key = Some("fixture-langsearch-key".into());
    let output = search(&tb).await;
    assert!(output.contains("Rust [redacted]"));
    assert!(!output.contains("fixture-langsearch-key"));
    let requests = server.requests.lock().unwrap();
    assert!(
        requests
            .iter()
            .all(|r| !r.path.contains("fixture-langsearch-key"))
    );
    assert!(
        requests[1]
            .headers
            .contains("authorization: Bearer fixture-langsearch-key")
    );
}

#[test]
fn auto_search_helper_resolution_stays_on_the_session_even_with_another_picked_helper() {
    let root = std::env::temp_dir().join(format!("nexus-search-helper-{}", uuid::Uuid::new_v4()));
    let mut app = crate::app::App::new(
        crate::db::Db::open_in_memory().unwrap(),
        None,
        crate::space::Space { root: root.clone() },
    );
    app.backends.set(
        BackendTag::OpenRouter,
        OpenRouter::openrouter_flavor("fixture-or".into()),
    );
    app.backends
        .set(BackendTag::OpenAi, OpenRouter::openai("fixture-ai".into()));
    app.current_model = Some("openai:gpt-chat".into());
    app.helper_model = "other/provider-model".into();
    app.models = ["gpt-chat", "gpt-6-luna", "gpt-6.1-luna"]
        .into_iter()
        .map(|id| crate::provider::Model {
            id: id.into(),
            name: id.into(),
            backend: BackendTag::OpenAi,
            reasoning_efforts: vec![],
            context_length: None,
            supports_images: false,
            supports_image_generation: false,
            supports_video_generation: false,
            pricing: None,
        })
        .collect();
    let (provider, model) = app.resolve_utility_model_backend("").unwrap();
    assert_eq!(provider.backend_tag(), BackendTag::OpenAi);
    assert_eq!(model, "gpt-6.1-luna");
    app.models.retain(|m| m.id == "gpt-chat");
    let (provider, model) = app.resolve_utility_model_backend("").unwrap();
    assert_eq!(provider.backend_tag(), BackendTag::OpenAi);
    assert_eq!(model, "gpt-chat");
    drop(app);
    if root.exists() {
        std::fs::remove_dir_all(root).unwrap();
    }
}

#[tokio::test]
async fn codex_stream_with_an_empty_completed_output_uses_the_streamed_items() {
    let server = mock(|_| {
        let mut reply = responses_reply();
        let items = reply["output"].take();
        let mut body = String::new();
        for item in items.as_array().unwrap() {
            body.push_str(&format!(
                "event: response.output_item.done\ndata: {}\n\n",
                json!({"type":"response.output_item.done","item":item})
            ));
        }
        reply["output"] = json!([]);
        body.push_str(&format!(
            "event: response.completed\ndata: {}\n\n",
            json!({"type":"response.completed","response":reply})
        ));
        (200, body)
    })
    .await;
    let payload = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .encode(r#"{"https://api.openai.com/auth":{"chatgpt_account_id":"fixture-account"}}"#);
    let mut tb = toolbox(&server);
    tb.search_provider = "provider".into();
    tb.search_backend = Some((
        OpenRouter::openai_codex(format!("header.{payload}.signature"))
            .with_search_endpoint(server.base.clone()),
        "helper-on-session".into(),
    ));
    let output = search(&tb).await;
    assert!(
        output.contains("[1] Rust docs\n    https://example.org/rust"),
        "{output}"
    );
}

#[tokio::test]
async fn exa_highlights_mentioning_title_do_not_split_a_hit() {
    let server = mock(|_| {
        (
            200,
            json!({"jsonrpc":"2.0","id":1,"result":{"content":[{"type":"text","text":
                "Title: Page one\nURL: https://example.org/one\nPublished: N/A\nAuthor: N/A\nHighlights:\nThe header says Title: Page two inside a sentence.\n\nTitle: Real second\nURL: https://example.org/two\nPublished: N/A\nAuthor: N/A\nHighlights:\nSecond."
            }]}})
            .to_string(),
        )
    })
    .await;
    let mut tb = toolbox(&server);
    tb.search_provider = "exa".into();
    let output = search(&tb).await;
    assert!(output.contains("[1] Page one"), "{output}");
    assert!(
        output.contains("Title: Page two inside a sentence"),
        "{output}"
    );
    assert!(output.contains("[2] Real second"), "{output}");
    assert!(!output.contains("[3]"), "{output}");
}
