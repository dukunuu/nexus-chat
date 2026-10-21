use super::*;
use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

fn wikipedia() -> Value {
    json!({"query":{"pages":{
        "2":{"index":2,"title":"Second","fullurl":"https://en.wikipedia.org/wiki/Second","thumbnail":{"source":"https://upload.wikimedia.org/second.jpg"}},
        "1":{"index":1,"title":"LeBron James","fullurl":"https://en.wikipedia.org/wiki/LeBron_James","thumbnail":{"source":"https://upload.wikimedia.org/lebron.jpg"}},
        "3":{"title":"No image"}
    }}})
}

fn commons() -> Value {
    json!({"query":{"pages":{
        "1":{"title":"File:Player","imageinfo":[{"thumburl":"https://upload.wikimedia.org/player.jpg","url":"https://upload.wikimedia.org/original.jpg","mime":"image/jpeg","descriptionurl":"https://commons.wikimedia.org/wiki/File:Player","extmetadata":{"LicenseShortName":{"value":"CC BY-SA 4.0"}}}]},
        "2":{"title":"File:SVG","imageinfo":[{"url":"https://upload.wikimedia.org/logo.svg","mime":"image/svg+xml"}]},
        "3":{"title":"File:PNG","imageinfo":[{"url":"https://upload.wikimedia.org/plain.png","mime":"image/png","descriptionurl":"https://commons.wikimedia.org/wiki/File:PNG"}]}
    }}})
}

fn openverse() -> Value {
    json!({"results":[
        {"title":"Court","url":"https://photos.example/court.jpg","thumbnail":"https://photos.example/thumb.jpg","foreign_landing_url":"https://flickr.com/photo/1","license":"by","source":"flickr"},
        {"title":"Fallback","thumbnail":"https://photos.example/fallback.webp"},
        {"title":"Insecure","url":"http://photos.example/private.jpg"}
    ]})
}

fn searxng() -> Value {
    json!({"results":[
        {"title":"Game","img_src":"https://photos.example/game.gif","thumbnail_src":"https://photos.example/thumb.gif","url":"https://sports.example/game"},
        {"title":"Fallback","thumbnail_src":"https://photos.example/small.png","url":"https://sports.example/small"}
    ]})
}

async fn mock(
    handler: impl Fn(&str) -> (u16, &'static str, Vec<u8>, Duration) + Send + Sync + 'static,
) -> (String, Arc<Mutex<Vec<String>>>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let requests = Arc::new(Mutex::new(Vec::new()));
    let captured = requests.clone();
    let handler = Arc::new(handler);
    tokio::spawn(async move {
        loop {
            let Ok((mut stream, _)) = listener.accept().await else {
                return;
            };
            let handler = handler.clone();
            let captured = captured.clone();
            tokio::spawn(async move {
                let mut request = Vec::new();
                let mut buffer = [0; 1024];
                while !request.windows(4).any(|part| part == b"\r\n\r\n") {
                    let count = stream.read(&mut buffer).await.unwrap();
                    if count == 0 {
                        return;
                    }
                    request.extend_from_slice(&buffer[..count]);
                }
                let request = String::from_utf8(request).unwrap();
                let path = request
                    .lines()
                    .next()
                    .unwrap()
                    .split_whitespace()
                    .nth(1)
                    .unwrap();
                let (status, mime, body, delay) = handler(path);
                captured.lock().unwrap().push(request);
                tokio::time::sleep(delay).await;
                let head = format!(
                    "HTTP/1.1 {status} Mock\r\nContent-Type: {mime}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                );
                let _ = stream.write_all(head.as_bytes()).await;
                let _ = stream.write_all(&body).await;
            });
        }
    });
    (base, requests)
}

fn toolbox() -> ToolBox {
    ToolBox::new(
        PathBuf::new(),
        None,
        None,
        "auto".into(),
        Vec::new(),
        None,
        None,
        None,
    )
}

fn png() -> Vec<u8> {
    let mut bytes = std::io::Cursor::new(Vec::new());
    image::RgbImage::from_pixel(4, 4, image::Rgb([20, 50, 80]))
        .write_to(&mut bytes, image::ImageFormat::Png)
        .unwrap();
    bytes.into_inner()
}

#[test]
fn all_image_provider_fixtures_parse_and_keep_provenance() {
    let wiki = parse_image_hits(&wikipedia(), 0);
    assert_eq!(wiki.len(), 2);
    assert_eq!(wiki[0].title, "LeBron James");
    let commons = parse_image_hits(&commons(), 1);
    assert_eq!(commons.len(), 2);
    assert_eq!(commons[0].url, "https://upload.wikimedia.org/player.jpg");
    assert_eq!(commons[0].license, "CC BY-SA 4.0");
    assert!(commons[1].url.ends_with("plain.png"));
    let open = parse_image_hits(&openverse(), 2);
    assert_eq!(open.len(), 2);
    assert_eq!(open[0].source, "flickr");
    assert_eq!(open[0].page, "https://flickr.com/photo/1");
    assert!(open[1].url.ends_with("fallback.webp"));
    let searx = parse_image_hits(&searxng(), 3);
    assert_eq!(searx.len(), 2);
    assert!(searx[0].url.ends_with("game.gif"));
    assert!(searx[1].url.ends_with("small.png"));
    assert_eq!(
        format_image_results("LeBron James", &wiki[..1]),
        "[1] LeBron James\n    https://upload.wikimedia.org/lebron.jpg\n    image · en.wikipedia.org · page https://en.wikipedia.org/wiki/LeBron_James"
    );
}

#[test]
fn image_results_interleave_deduplicate_cap_and_explain_empty() {
    let wiki = parse_image_hits(&wikipedia(), 0);
    let mut commons = parse_image_hits(&commons(), 1);
    commons.push(wiki[0].clone());
    let open: Vec<_> = (0..10)
        .map(|index| ImageHit {
            url: format!("https://photos.example/{index}.png"),
            ..wiki[0].clone()
        })
        .collect();
    let merged = merge_image_hits(&[wiki.clone(), commons.clone(), open]);
    assert_eq!(merged.len(), 8);
    assert_eq!(merged[0].url, wiki[0].url);
    assert_eq!(merged[1].url, commons[0].url);
    assert_eq!(merged[3].url, wiki[1].url);
    assert_eq!(
        merged.iter().filter(|hit| hit.url == wiki[0].url).count(),
        1
    );
    assert_eq!(
        format_image_results("unknown", &[]),
        "no images found for \"unknown\" · try a more specific name"
    );
}

#[tokio::test]
async fn image_search_routes_and_survives_one_provider_failing() {
    let (base, requests) = mock(|path| {
        let (status, body) = if path.starts_with("/wiki?") {
            (503, json!({}))
        } else if path.starts_with("/commons?") {
            (200, commons())
        } else if path.starts_with("/open?") {
            (200, openverse())
        } else {
            (200, searxng())
        };
        (
            status,
            "application/json",
            body.to_string().into_bytes(),
            Duration::from_millis(20),
        )
    })
    .await;
    let mut toolbox = toolbox();
    toolbox.image_search = ImageSearch {
        wikipedia: format!("{base}/wiki"),
        commons: format!("{base}/commons"),
        openverse: format!("{base}/open"),
    };
    toolbox.searxng_url = Some(base);
    let args = r#"{"mode":"images","query":"LeBron James"}"#;
    assert!(is_read_only_tool("search", args));
    assert_eq!(public_call("search", args).unwrap().0, "image_search");
    let (result, _) = toolbox.run("search", args).await;
    assert!(result.contains("player.jpg"), "{result}");
    assert!(result.contains("court.jpg"), "{result}");
    assert!(result.contains("game.gif"), "{result}");
    let requests = requests.lock().unwrap();
    assert_eq!(requests.len(), 4);
    for request in requests.iter() {
        let lower = request.to_ascii_lowercase();
        assert!(lower.contains("user-agent: nexus-chat/"));
        assert!(lower.contains("https://github.com/dukunuu/nexus-chat"));
        assert!(!lower.contains("authorization:") && !lower.contains("cookie:"));
    }
    assert!(
        requests
            .iter()
            .any(|request| request.contains("categories=images"))
    );
    assert!(requests.iter().any(|request| request.contains("gsrnamespace=6") && request.contains("iiurlwidth=800")));
    assert!(
        requests
            .iter()
            .any(|request| request.contains("pithumbsize=800"))
    );
    assert!(
        requests
            .iter()
            .any(|request| request.contains("page_size=6"))
    );
}

#[tokio::test]
async fn image_check_reports_real_404_at_the_authored_path() {
    let (base, requests) = mock(|_| (404, "text/plain", Vec::new(), Duration::ZERO)).await;
    let root = std::env::temp_dir().join(format!("nexus-check-{}", uuid::Uuid::new_v4()));
    let fetcher = crate::remote_image::Fetcher::for_test(&root);
    let url = format!("{base}/missing.jpg");
    let (errors, note) = check_ui_images(
        vec![("view[0].image".into(), &url), ("view[1].src".into(), &url)],
        Some(&fetcher),
        Duration::from_secs(2),
    )
    .await;
    assert!(note.is_empty());
    assert_eq!(errors.len(), 1);
    assert_eq!(errors[0].path, "view[0].image");
    assert!(errors[0].message.contains("404"), "{:?}", errors[0]);
    assert!(errors[0].message.contains("copy the URL exactly"));
    assert_eq!(requests.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn image_check_caches_decoded_images_for_a_second_preview_fetcher() {
    let (base, requests) = mock(|_| (200, "image/png", png(), Duration::ZERO)).await;
    let root = std::env::temp_dir().join(format!("nexus-check-{}", uuid::Uuid::new_v4()));
    let fetcher = crate::remote_image::Fetcher::for_test(&root);
    let url = format!("{base}/good.png");
    let (errors, note) = check_ui_images(
        vec![("view[0].src".into(), &url)],
        Some(&fetcher),
        Duration::from_secs(2),
    )
    .await;
    assert!(errors.is_empty() && note.is_empty());
    let preview = crate::remote_image::Fetcher::for_test(&root);
    let crate::remote_image::Status::Ready(path) = preview.request(&url) else {
        panic!("verified image was not cached");
    };
    assert_eq!(image::open(path).unwrap().width(), 4);
    assert_eq!(requests.lock().unwrap().len(), 1);
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn ui_check_off_skips_images_and_good_cached_document_passes() {
    let root = std::env::temp_dir().join(format!("nexus-check-{}", uuid::Uuid::new_v4()));
    let fetcher = crate::remote_image::Fetcher::new(&root);
    let url = "https://127.0.0.1:1/good.png";
    let mut toolbox = toolbox();
    toolbox.interactive_ui = true;
    toolbox.remote_images = Some(fetcher.clone());
    let args = json!({"source":json!({"version":1,"title":"Photo","view":[{"type":"hero","title":"Player","image":url}]}).to_string()}).to_string();
    fetcher.set_enabled(false);
    let result = toolbox.run("ui_check", &args).await.0;
    assert!(result.starts_with("ok ·"), "{result}");
    assert!(result.contains("images skipped · remote_images off"));
    assert!(!root.exists());
    fetcher.set_enabled(true);
    fetcher.seed(url, &png()).unwrap();
    let result = toolbox.run("ui_check", &args).await.0;
    assert!(
        result.starts_with("ok ·") && !result.contains("skipped"),
        "{result}"
    );
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn image_check_timeout_is_only_a_warning_and_vetting_still_applies() {
    let (base, _) = mock(|_| (200, "image/png", png(), Duration::from_millis(200))).await;
    let root = std::env::temp_dir().join(format!("nexus-check-{}", uuid::Uuid::new_v4()));
    let fetcher = crate::remote_image::Fetcher::for_test(&root);
    let url = format!("{base}/slow.png");
    let (errors, note) = check_ui_images(
        vec![("view[0].src".into(), &url)],
        Some(&fetcher),
        Duration::from_millis(40),
    )
    .await;
    assert!(errors.is_empty());
    assert!(note.contains("warning · image check timed out"), "{note}");
    fetcher.fetch(&url).await.unwrap();
    let strict = crate::remote_image::Fetcher::new(&root);
    let (errors, _) = check_ui_images(
        vec![("view[0].image".into(), "https://127.0.0.1:1/private.png")],
        Some(&strict),
        Duration::from_secs(1),
    )
    .await;
    assert_eq!(errors.len(), 1);
    assert!(errors[0].message.contains("non-public"), "{:?}", errors[0]);
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn image_verification_stops_at_eight_unique_urls() {
    let (base, requests) = mock(|_| (200, "image/png", png(), Duration::ZERO)).await;
    let root = std::env::temp_dir().join(format!("nexus-check-{}", uuid::Uuid::new_v4()));
    let fetcher = crate::remote_image::Fetcher::for_test(&root);
    let urls: Vec<_> = (0..10).map(|index| format!("{base}/{index}.png")).collect();
    let images = urls
        .iter()
        .enumerate()
        .map(|(index, url)| (format!("view[{index}].src"), url.as_str()))
        .collect();
    let (errors, note) = check_ui_images(images, Some(&fetcher), Duration::from_secs(2)).await;
    assert!(errors.is_empty());
    assert!(note.contains("only the first 8 images"));
    assert_eq!(requests.lock().unwrap().len(), 8);
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn failed_cached_image_is_an_error_in_the_public_ui_check_result() {
    let root = std::env::temp_dir().join(format!("nexus-check-{}", uuid::Uuid::new_v4()));
    let fetcher = crate::remote_image::Fetcher::new(&root);
    let url = "https://photos.example/missing.jpg";
    fetcher.seed_failure(url);
    let mut toolbox = toolbox();
    toolbox.interactive_ui = true;
    toolbox.remote_images = Some(fetcher);
    let source =
        json!({"version":1,"title":"Photo","view":[{"type":"hero","title":"Player","image":url}]})
            .to_string();
    let (result, status) = toolbox
        .run("ui_check", &json!({"source":source}).to_string())
        .await;
    assert!(result.starts_with("1 error\n"), "{result}");
    assert!(
        result.contains("view[0].image: image did not load (image unavailable)"),
        "{result}"
    );
    assert_eq!(status, "UI check · 1 errors");
}

#[tokio::test]
async fn default_image_providers_are_hermetic_and_explain_no_results() {
    let mut toolbox = toolbox();
    toolbox.research_only = true;
    let (result, _) = toolbox
        .run("search", r#"{"mode":"images","query":"unknown"}"#)
        .await;
    assert_eq!(
        result,
        "no images found for \"unknown\" · try a more specific name"
    );
}

#[test]
fn every_document_image_field_has_a_located_path() {
    let document = crate::interactive::Document::parse(&json!({"version":1,"title":"Images","view":[
        {"type":"image","src":"https://img.example/1.png"},
        {"type":"hero","title":"Hero","image":"https://img.example/2.png"},
        {"type":"cards","items":[{"title":"Card","text":"Body","image":"https://img.example/3.png"}]}
    ]}).to_string()).unwrap();
    assert_eq!(
        document
            .images()
            .iter()
            .map(|(path, _)| path.as_str())
            .collect::<Vec<_>>(),
        ["view[0].src", "view[1].image", "view[2].items[0].image"]
    );
    let document = crate::interactive::Document::parse(&json!({"version":1,"title":"Tile","view":[{"type":"buttons","items":[{"label":"Tile","action":"pick","image":"https://img.example/4.png"}]}],"actions":{"pick":[{"op":"answer"}]}}).to_string()).unwrap();
    assert_eq!(
        document.images(),
        [("view[0].items[0].image".into(), "https://img.example/4.png")]
    );
}
