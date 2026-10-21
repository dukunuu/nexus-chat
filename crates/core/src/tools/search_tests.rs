use super::*;
use tokio::io::{AsyncBufReadExt as _, AsyncReadExt as _, AsyncWriteExt as _, BufReader};

const ATOM: &str = r#"<?xml version="1.0"?><feed xmlns="http://www.w3.org/2005/Atom">
<entry><title>Rust &amp; Go &#x2014; worth it?</title>
<link type="text/html" href="https://www.reddit.com/r/rust/comments/abc/comparison/?a=1&amp;b=2" rel="alternate"/>
<category label="r/rust" term="rust"/><updated>2026-10-21T12:00:00Z</updated>
<author><name>/u/alice</name></author>
<content type="html">&lt;p&gt;Rust &amp;amp; Go: &amp;quot;yes&amp;quot; &#39;really&#39; &#x1F980;&lt;/p&gt;</content></entry>
<entry><title><![CDATA[Go <vs> Rust & friends]]></title>
<link rel='alternate' href='https://www.reddit.com/r/golang/comments/def/comparison/' type='text/html'/>
<category term='golang' label='r/golang'/><author><name>bob</name></author>
<updated>2026-10-20T12:00:00Z</updated><content type='html'><![CDATA[<p>Simple &amp; fast.</p>]]></content></entry>
</feed>"#;

const HN: &str = r#"{"hits":[
{"title":"Rust versus Go","url":"https://example.com/article","objectID":"123","points":42,"num_comments":7},
{"title":"Rust experience","objectID":"456","points":12,"num_comments":3}
]}"#;
const WEB: &str = r#"{"results":[
{"title":"Rust guide","url":"https://example.com/rust","content":"Rust reference"},
{"title":"Rust handbook","url":"https://example.org/rust","content":"Rust examples"},
{"title":"Go guide","url":"https://go.dev/doc","content":"Go reference"}
]}"#;

struct Request {
    method: String,
    url: reqwest::Url,
    headers: String,
    body: String,
}

struct Mock {
    base: String,
    requests: Arc<Mutex<Vec<Request>>>,
    task: tokio::task::JoinHandle<()>,
}

impl Drop for Mock {
    fn drop(&mut self) {
        self.task.abort();
    }
}

async fn mock(handler: impl Fn(&Request) -> (u16, String) + Send + 'static) -> Mock {
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
        .await
        .unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let requests = Arc::new(Mutex::new(Vec::new()));
    let recorded = requests.clone();
    let task = tokio::spawn(async move {
        loop {
            let (stream, _) = listener.accept().await.unwrap();
            let mut stream = BufReader::new(stream);
            let mut first = String::new();
            stream.read_line(&mut first).await.unwrap();
            let mut words = first.split_whitespace();
            let method = words.next().unwrap().to_string();
            let url =
                reqwest::Url::parse(&format!("http://localhost{}", words.next().unwrap())).unwrap();
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
                url,
                headers,
                body: String::from_utf8(body).unwrap(),
            };
            let (status, body) = handler(&request);
            recorded.lock().unwrap().push(request);
            let response = format!(
                "HTTP/1.1 {status} Mock\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            stream
                .get_mut()
                .write_all(response.as_bytes())
                .await
                .unwrap();
        }
    });
    Mock {
        base,
        requests,
        task,
    }
}

fn search_config(mock: &Mock, authenticated: bool) -> DiscussionSearch {
    DiscussionSearch {
        reddit_base: mock.base.clone(),
        oauth_base: format!("{}/oauth", mock.base),
        hn_url: format!("{}/hn", mock.base),
        credentials: authenticated.then(|| ("fixture-id".into(), "fixture-secret".into())),
        token: tokio::sync::Mutex::new(None),
    }
}

fn toolbox(mock: &Mock) -> ToolBox {
    let mut tb = ToolBox::new(
        PathBuf::new(),
        Some(mock.base.clone()),
        None,
        "searxng".into(),
        vec![],
        None,
        None,
        None,
    );
    tb.discussions = Arc::new(search_config(mock, false));
    tb
}

#[test]
fn atom_entities_cdata_and_attribute_order() {
    let entries = parse_reddit_atom(ATOM);
    assert_eq!(entries.len(), 2);
    assert_eq!(entries[0].title, "Rust & Go — worth it?");
    assert!(entries[0].url.ends_with("?a=1&b=2"));
    assert_eq!(entries[0].subreddit, "r/rust");
    assert_eq!(entries[0].updated, "2026-10-21");
    assert_eq!(entries[0].author, "u/alice");
    assert_eq!(entries[0].content, "Rust & Go: \"yes\" 'really' 🦀");
    assert_eq!(entries[1].title, "Go <vs> Rust & friends");
    assert_eq!(entries[1].author, "u/bob");
    assert_eq!(entries[1].content, "Simple & fast.");
    assert_eq!(
        atom_unescape("&lt;&gt;&quot;&#39;&#x41;&#65;&amp;lt; &unknown;"),
        "<>\"'AA&lt; &unknown;"
    );
    assert_eq!(
        atom_text("<![CDATA[&amp; <entry>]]> &amp;"),
        "&amp; <entry> &"
    );
}

#[test]
fn atom_missing_fields_and_junk_do_not_invent_links_or_entries() {
    for junk in [
        "",
        "not XML",
        "<entry",
        "<entry><title>broken",
        "<entryevil>junk</entryevil>",
        "<!-- <entry>fake</entry> -->",
        "<![CDATA[<entry>fake</entry>]]>",
    ] {
        assert!(parse_reddit_atom(junk).is_empty());
    }
    let entries = parse_reddit_atom(
        r#"<feed>
<entry><title>no URL</title></entry>
<entry><link href="https://reddit.com.evil.test/r/x"/></entry>
<entry><link href="https://reddit.com/r/x/comments/id/title/"/></entry>
</feed>"#,
    );
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].title, "(untitled)");
    assert_eq!(entries[0].author, "u/[deleted]");
    assert!(entries[0].content.is_empty());
    assert_eq!(
        atom_attr(
            "link data-href='bad' title='a > b' href = 'good&amp;ok'",
            "href"
        ),
        Some("good&ok".into())
    );
    assert_eq!(atom_attr("link href=broken", "href"), None);
    assert_eq!(
        atom_elements(
            "<entry><content><![CDATA[</entry><entry>fake]]></content></entry>",
            "entry"
        )
        .len(),
        1
    );
}

#[tokio::test]
async fn rss_search_uses_public_feed_and_real_user_agent() {
    let mock = mock(|_| (200, ATOM.into())).await;
    let hits = reddit_search(
        &reqwest::Client::new(),
        "Rust & Go",
        &search_config(&mock, false),
    )
    .await
    .unwrap();
    assert_eq!(hits.len(), 2);
    let result = format_discussion_hits(&[], &hits);
    assert!(result.contains("r/rust · 2026-10-21 · u/alice\n    Rust & Go"));
    let requests = mock.requests.lock().unwrap();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].url.path(), "/search.rss");
    let query: HashMap<_, _> = requests[0].url.query_pairs().collect();
    assert_eq!(query["q"], "Rust & Go");
    assert_eq!(query["sort"], "relevance");
    assert_eq!(query["limit"], "10");
    assert!(requests[0].headers.contains(REDDIT_USER_AGENT));
    assert!(!requests[0].headers.contains("authorization"));
}

#[tokio::test]
async fn oauth_uses_app_credentials_bearer_and_cached_token_until_expiry() {
    let mock = mock(|request| match request.url.path() {
        "/api/v1/access_token" => (200, r#"{"access_token":"fixture-token","expires_in":3600}"#.into()),
        "/oauth/search" => (200, r#"{"data":{"children":[{"data":{"title":"Rust comparison","permalink":"/r/rust/comments/abc/comparison/","subreddit":"rust","score":25,"num_comments":9,"selftext":"Useful discussion"}}]}}"#.into()),
        _ => panic!("unexpected RSS fallback"),
    }).await;
    let config = search_config(&mock, true);
    let client = reqwest::Client::new();
    for _ in 0..2 {
        let hits = reddit_search(&client, "Rust", &config).await.unwrap();
        let result = format_discussion_hits(&[], &hits);
        assert!(result.contains("r/rust · 25 points · 9 comments\n    Useful discussion"));
        for secret in ["fixture-id", "fixture-secret", "fixture-token"] {
            assert!(!result.contains(secret));
        }
    }
    config.token.lock().await.as_mut().unwrap().expires = Instant::now() - Duration::from_secs(1);
    reddit_search(&client, "Rust", &config).await.unwrap();
    let requests = mock.requests.lock().unwrap();
    let tokens: Vec<_> = requests
        .iter()
        .filter(|r| r.url.path() == "/api/v1/access_token")
        .collect();
    assert_eq!(tokens.len(), 2);
    assert_eq!(tokens[0].method, "POST");
    assert_eq!(tokens[0].body, "grant_type=client_credentials");
    let basic = base64::engine::general_purpose::STANDARD.encode("fixture-id:fixture-secret");
    assert!(
        tokens[0]
            .headers
            .contains(&format!("authorization: Basic {basic}"))
    );
    assert!(
        tokens[0]
            .headers
            .contains("application/x-www-form-urlencoded")
    );
    for request in requests.iter().filter(|r| r.url.path() == "/oauth/search") {
        assert!(
            request
                .headers
                .contains("authorization: Bearer fixture-token")
        );
        assert!(request.headers.contains(REDDIT_USER_AGENT));
        assert_eq!(
            request
                .url
                .query_pairs()
                .find(|(key, _)| key == "limit")
                .unwrap()
                .1,
            "10"
        );
    }
}

#[tokio::test]
async fn oauth_failures_fall_back_without_echoing_secrets() {
    for fail_token in [false, true] {
        let mock = mock(move |request| match request.url.path() {
            "/api/v1/access_token" if !fail_token => (
                200,
                r#"{"access_token":"fixture-token","expires_in":3600}"#.into(),
            ),
            "/search.rss" => (200, ATOM.into()),
            _ => (401, "fixture-secret fixture-token".into()),
        })
        .await;
        let hits = reddit_search(&reqwest::Client::new(), "Rust", &search_config(&mock, true))
            .await
            .unwrap();
        let result = format_discussion_hits(&[], &hits);
        assert!(result.contains("u/alice"));
        assert!(!result.contains("fixture-secret"));
        assert!(!result.contains("fixture-token"));
        let requests = mock.requests.lock().unwrap();
        assert_eq!(requests.last().unwrap().url.path(), "/search.rss");
        assert!(!requests.last().unwrap().headers.contains("authorization"));
    }
    let mock = mock(|_| (401, "fixture-secret fixture-token".into())).await;
    let error = reddit_search(&reqwest::Client::new(), "Rust", &search_config(&mock, true))
        .await
        .err()
        .unwrap()
        .to_string();
    assert!(!error.contains("fixture-secret"));
    assert!(!error.contains("fixture-token"));
}

fn thread_fixture(comment_text: &str) -> String {
    let mut xml = ATOM.replace("</feed>", "");
    // The second search entry stands in for a top-level comment.
    xml.push_str(r#"<entry><title>nested</title><link href="https://reddit.com/r/rust/comments/abc/comparison/nested/"/><author><name>nested</name></author><content><![CDATA[<p>Reply, not top-level</p><a href="https://reddit.com/r/rust/comments/abc/comparison/parent/">[parent]</a>]]></content></entry>"#);
    for index in 0..18 {
        let _ = write!(
            xml,
            "<entry><title>comment</title><link href='https://reddit.com/r/rust/comments/abc/comparison/c{index}/'/><author><name>user{index}</name></author><content><![CDATA[<p>{comment_text}</p>]]></content></entry>"
        );
    }
    xml.push_str("</feed>");
    xml
}

#[tokio::test]
async fn fetch_url_routes_reddit_threads_to_rss_and_formats_top_level_comments() {
    let mock = mock(|_| (200, thread_fixture("Helpful &amp; specific"))).await;
    let address = mock.base.trim_start_matches("http://").parse().unwrap();
    let mut tb = toolbox(&mock);
    tb.client = reqwest::Client::builder()
        .no_proxy()
        .resolve("www.reddit.com", address)
        .build()
        .unwrap();
    let port = reqwest::Url::parse(&mock.base).unwrap().port().unwrap();
    let url =
        format!("http://www.reddit.com:{port}/r/rust/comments/abc/comparison/?tracking=x#fragment");
    let (result, _) = tb
        .run("fetch_url", &serde_json::json!({"url":url}).to_string())
        .await;
    assert!(result.contains("Rust & Go — worth it? · r/rust · u/alice"));
    assert!(result.contains("u/bob: Simple & fast."));
    assert!(result.contains("u/user13: Helpful & specific"));
    assert!(!result.contains("u/user14:"));
    assert!(!result.contains("Reply, not top-level"));
    let requests = mock.requests.lock().unwrap();
    assert_eq!(
        requests[0].url.path(),
        "/r/rust/comments/abc/comparison/.rss"
    );
    assert_eq!(requests[0].url.query(), Some("limit=20"));
}

#[tokio::test]
async fn thread_text_and_comment_sizes_are_capped() {
    let mock = mock(|_| (200, thread_fixture(&"é".repeat(10_000)))).await;
    let result = fetch_reddit_thread(&reqwest::Client::new(), &mock.base)
        .await
        .unwrap();
    assert!(result.chars().count() <= MAX_TOOL_RESULT_CHARS);
    assert!(!result.contains(&"é".repeat(501)));
    assert!(!result.contains("u/user14:"));
}

#[tokio::test]
async fn thread_429_waits_two_seconds_and_retries_once() {
    for recover in [false, true] {
        let calls = AtomicU64::new(0);
        let mock = mock(move |_| {
            if calls.fetch_add(1, Ordering::Relaxed) == 0 || !recover {
                (429, "upstream error dump".into())
            } else {
                (200, ATOM.into())
            }
        })
        .await;
        let start = Instant::now();
        let result = fetch_reddit_thread(&reqwest::Client::new(), &mock.base)
            .await
            .unwrap();
        assert!(start.elapsed() >= Duration::from_secs(2));
        assert_eq!(mock.requests.lock().unwrap().len(), 2);
        if recover {
            assert!(result.contains("u/alice"));
        } else {
            assert_eq!(
                result,
                "Reddit is rate-limiting right now · try again in a minute"
            );
        }
    }
}

#[test]
fn thread_urls_are_detected_without_accepting_lookalike_hosts() {
    assert_eq!(
        reddit_thread_feed_url(
            "https://old.reddit.com/r/rust/comments/abc/title/comment/?x=1#fragment"
        )
        .unwrap()
        .as_str(),
        "https://old.reddit.com/r/rust/comments/abc/title/.rss?limit=20"
    );
    for url in [
        "https://reddit.com/search?q=x",
        "https://reddit.com.evil.test/r/x/comments/a/b/",
        "not a URL",
    ] {
        assert!(reddit_thread_feed_url(url).is_none());
    }
}

#[tokio::test]
async fn opinion_web_search_blends_only_three_threads_with_continued_numbering() {
    let mock = mock(|request| {
        (
            200,
            match request.url.path() {
                "/search" => WEB.into(),
                "/hn" => HN.into(),
                "/search.rss" => ATOM.into(),
                path => panic!("unexpected path {path}"),
            },
        )
    })
    .await;
    let (result, _) = toolbox(&mock)
        .run("search", r#"{"mode":"web","query":"best Rust vs Go"}"#)
        .await;
    assert!(result.contains("discussions:\n[4] Rust versus Go\n    https://news.ycombinator.com/item?id=123\n    HN · 42 points · 7 comments"));
    assert!(result.contains("[5] Rust & Go — worth it?\n    https://www.reddit.com/"));
    assert!(result.contains("[6] Rust experience"));
    assert!(!result.contains("[7]"));
    assert_eq!(mock.requests.lock().unwrap().len(), 3);
    let view = crate::app::tool_call_view(
        "search",
        r#"{"mode":"web","query":"best Rust vs Go"}"#,
        &result,
    );
    assert_eq!(view.sources.len(), 6);
    assert_eq!(view.sources[3].domain, "news.ycombinator.com");
    assert_eq!(view.sources[4].domain, "reddit.com");
    assert!(!view.sources[2].snippet.contains("discussions:"));
}

#[tokio::test]
async fn factual_queries_do_not_search_discussions_and_discussion_failures_preserve_web() {
    for (query, count) in [("Rust documentation", 1), ("best Rust", 3)] {
        let mock = mock(|request| {
            if request.url.path() == "/search" {
                (200, WEB.into())
            } else {
                (503, "offline".into())
            }
        })
        .await;
        let (result, _) = toolbox(&mock)
            .run(
                "search",
                &serde_json::json!({"mode":"web","query":query}).to_string(),
            )
            .await;
        let web: SearxngResponse = serde_json::from_str(WEB).unwrap();
        let hits: Vec<_> = web
            .results
            .into_iter()
            .map(|hit| SearchHit {
                title: hit.title,
                url: hit.url,
                snippet: hit.content,
                image: None,
            })
            .collect();
        assert_eq!(result, format_results(&hits));
        assert_eq!(mock.requests.lock().unwrap().len(), count);
    }
}

#[tokio::test]
async fn discussion_mode_interleaves_sources_and_explains_empty_results() {
    let server = mock(|request| {
        (
            200,
            if request.url.path() == "/hn" {
                HN.into()
            } else {
                ATOM.into()
            },
        )
    })
    .await;
    let (result, _) = toolbox(&server)
        .run("search", r#"{"mode":"discussion","query":"Rust"}"#)
        .await;
    assert!(result.starts_with("[1] Rust versus Go\n    https://news.ycombinator.com/item?id=123"));
    assert!(result.contains("[2] Rust & Go — worth it?"));
    assert!(result.contains("[3] Rust experience"));
    assert!(result.contains("[4] Go <vs> Rust & friends"));
    let view =
        crate::app::tool_call_view("search", r#"{"mode":"discussion","query":"Rust"}"#, &result);
    assert_eq!(view.sources.len(), 4);
    assert_eq!(view.sources[1].domain, "reddit.com");
    assert!(
        view.sources[1]
            .snippet
            .contains("r/rust · 2026-10-21 · u/alice")
    );
    let mock = mock(|_| (503, "unavailable".into())).await;
    let (empty, _) = toolbox(&mock)
        .run("search", r#"{"mode":"discussion","query":"Rust"}"#)
        .await;
    assert_eq!(
        empty,
        "no discussions found for \"Rust\" · try different words"
    );
}

#[test]
fn discussion_deduplication_preserves_interleaved_rank() {
    let hit = |title: &str, url: &str| DiscussionHit {
        title: title.into(),
        url: url.into(),
        meta: "HN · 1 points · 2 comments".into(),
    };
    let hn = [
        hit("first", "https://news.ycombinator.com/item?id=1"),
        hit("second", "https://news.ycombinator.com/item?id=2"),
    ];
    let reddit = [
        hit(
            "duplicate",
            "https://news.ycombinator.com/item?id=1&utm_source=x",
        ),
        hit("third", "https://reddit.com/r/x/comments/id/title/"),
    ];
    let result = format_discussion_hits(&hn, &reddit);
    assert!(result.contains("[1] first"));
    assert!(result.contains("[2] second"));
    assert!(result.contains("[3] third"));
    assert!(!result.contains("duplicate"));
}

#[test]
fn opinion_cues_are_case_insensitive_words_or_phrases() {
    for cue in [
        "best",
        "vs",
        "versus",
        "worth",
        "recommend",
        "recommendation",
        "review",
        "reviews",
        "experience",
        "opinion",
        "alternative",
        "alternatives",
        "should i",
        "is it",
        "reddit",
        "hn",
        "which",
    ] {
        assert!(
            is_opinion_query(&format!("Rust {} Go", cue.to_uppercase())),
            "{cue}"
        );
    }
    for query in [
        "Ulaanbaatar population 2026",
        "investment statistics",
        "versatile algorithms",
        "worthwhile history",
        "HNO3 formula",
    ] {
        assert!(!is_opinion_query(query), "{query}");
    }
}

fn relevance_fixture() -> Vec<SearchHit> {
    [
        (
            "Nauru statistics",
            "https://stats.nauru.nr",
            "Island census",
        ),
        (
            "Ulaanbaatar census",
            "https://example.com/census",
            "City figures",
        ),
        (
            "Tharparkar district",
            "https://example.com/tharparkar",
            "Sindh district",
        ),
        (
            "Mongolia city data",
            "https://example.com/ulaanbaatar",
            "City figures",
        ),
        (
            "City data",
            "https://example.org/data",
            "Ulaanbaatar population",
        ),
    ]
    .into_iter()
    .map(|(title, url, snippet)| SearchHit {
        title: title.into(),
        url: url.into(),
        snippet: snippet.into(),
        image: None,
    })
    .collect()
}

#[test]
fn relevance_drops_unrelated_hits_counts_them_and_keeps_sources_parseable() {
    let output = format_web_results("Uláanbaatar population", relevance_fixture(), &[]);
    assert!(!output.contains("Nauru"));
    assert!(!output.contains("Tharparkar"));
    assert!(output.ends_with("(dropped 2 off-topic results)"));
    let view = crate::app::tool_call_view(
        "search",
        r#"{"mode":"web","query":"Ulaanbaatar population"}"#,
        &output,
    );
    assert_eq!(view.sources.len(), 3);
    assert_eq!(view.sources[2].snippet, "Ulaanbaatar population");
}

#[test]
fn relevance_keeps_at_least_three_best_overlaps_or_all_if_fewer() {
    let mut hits = relevance_fixture();
    assert_eq!(filter_web_hits("census figures", &mut hits), 2);
    assert_eq!(hits.len(), 3);
    assert!(hits.iter().any(|hit| hit.title == "Ulaanbaatar census"));
    let mut hits = relevance_fixture();
    assert_eq!(filter_web_hits("unmatched", &mut hits), 2);
    assert_eq!(hits.len(), 3);
    hits.truncate(2);
    assert_eq!(filter_web_hits("unmatched", &mut hits), 0);
    let mut hits = relevance_fixture();
    assert_eq!(filter_web_hits("the and of to", &mut hits), 0);
    assert_eq!(
        relevance_tokens("CAFÉ naïve MÜNCHEN façade résumé"),
        ["cafe", "naive", "munchen", "facade", "resume"]
    );
}
