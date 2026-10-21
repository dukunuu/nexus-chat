//! Native studio shell. Tests exercise the backend without constructing a window.

mod backend;

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{Context as _, Result};
use backend::Backend;
use nexus_core::{db, remote_image, space};
use tao::dpi::LogicalSize;
use tao::event::{Event, WindowEvent};
use tao::event_loop::{ControlFlow, EventLoopBuilder};
use tao::window::WindowBuilder;
use wry::http::Response;
use wry::{NewWindowResponse, WebViewBuilder};

#[cfg(target_os = "windows")]
const HOME: &str = "http://nexus.localhost/index.html";
#[cfg(not(target_os = "windows"))]
const HOME: &str = "nexus://localhost/index.html";

fn main() -> Result<()> {
    let runtime = tokio::runtime::Runtime::new()?;
    let _runtime = runtime.enter();
    let path = std::env::args_os().nth(1).map(PathBuf::from);
    let space = space::Space::open()?;
    let db = db::Db::open(&space.db_path())?;
    let default_id = db.default_space_id()?;
    let active = db
        .list_spaces()?
        .into_iter()
        .find(|s| s.id == default_id)
        .context("missing active space")?;
    let backend = Arc::new(Mutex::new(Backend::start(
        path.as_deref(),
        &space.files_dir(&active.name),
        &space.root,
    )?));
    let event_loop = EventLoopBuilder::<String>::with_user_event().build();
    let window = WindowBuilder::new()
        .with_title(format!(
            "nexus studio — {}",
            backend.lock().expect("backend lock").deck.title
        ))
        .with_inner_size(LogicalSize::new(1280.0, 800.0))
        .with_min_inner_size(LogicalSize::new(900.0, 600.0))
        .build(&event_loop)?;
    let proxy = event_loop.create_proxy();
    let protocol_backend = Arc::clone(&backend);
    let handle = runtime.handle().clone();
    let builder = WebViewBuilder::new()
        .with_devtools(false)
        .with_asynchronous_custom_protocol("nexus".into(), move |_, request, responder| {
            let uri = request.uri().to_string();
            let backend = Arc::clone(&protocol_backend);
            handle.spawn(async move {
                let remote = backend.lock().expect("backend lock").remote_image(&uri);
                let result = if let Ok((fetcher, src)) = remote {
                    remote_bytes(&backend, &fetcher, &src).await
                } else {
                    backend.lock().expect("backend lock").serve(&uri)
                };
                let (status, mime, bytes) = match result {
                    Ok((mime, bytes)) => (200, mime, bytes),
                    Err(_) => (
                        404,
                        "text/plain; charset=utf-8",
                        b"Resource unavailable".to_vec(),
                    ),
                };
                let response = Response::builder()
                    .status(status)
                    .header("Content-Type", mime)
                    // WebKit custom schemes can have an opaque origin. Only
                    // embedded pages may navigate here; allow their ES modules.
                    .header("Access-Control-Allow-Origin", "*")
                    .header("Content-Security-Policy", csp())
                    .header("X-Content-Type-Options", "nosniff")
                    .header("Cache-Control", "no-store")
                    .body(bytes)
                    .expect("static response headers");
                responder.respond(response);
            });
        })
        .with_navigation_handler(|url| url == HOME || url == HOME.replace("/index.html", "/"))
        .with_new_window_req_handler(|_, _| NewWindowResponse::Deny)
        .with_ipc_handler(move |request| {
            if backend::protocol_path(&request.uri().to_string())
                .is_ok_and(|path| matches!(path.as_str(), "/" | "/index.html"))
            {
                let _ = proxy.send_event(request.body().clone());
            }
        })
        .with_url(HOME);
    #[cfg(not(target_os = "linux"))]
    let webview = builder.build(&window)?;
    #[cfg(target_os = "linux")]
    let webview = {
        use tao::platform::unix::WindowExtUnix as _;
        use wry::WebViewBuilderExtUnix as _;
        builder.build_gtk(window.default_vbox().context("missing GTK container")?)?
    };
    event_loop.run(move |event, _, control_flow| {
        *control_flow = ControlFlow::Wait;
        match event {
            Event::UserEvent(source) => {
                let mut backend = backend.lock().expect("backend lock");
                let event = backend.dispatch(&source);
                window.set_title(&format!("nexus studio — {}", backend.deck.title));
                if let Err(error) = webview.evaluate_script(&event.script()) {
                    eprintln!("studio: {error}");
                }
            }
            Event::WindowEvent {
                event: WindowEvent::CloseRequested,
                ..
            } => *control_flow = ControlFlow::Exit,
            _ => {}
        }
    });
}

async fn remote_bytes(
    backend: &Arc<Mutex<Backend>>,
    fetcher: &Arc<remote_image::Fetcher>,
    src: &str,
) -> Result<(&'static str, Vec<u8>)> {
    // Downloads use core's HTTPS-only, DNS-vetted, bounded fetcher. The page
    // only receives sanitized PNGs, never an external URL or cache path.
    for _ in 0..150 {
        match fetcher.request(src) {
            remote_image::Status::Ready(path) => {
                let uri = format!(
                    "nexus://localhost/assets/images/{}",
                    path.file_name()
                        .context("image filename")?
                        .to_string_lossy()
                );
                let backend = backend.lock().expect("backend lock");
                anyhow::ensure!(
                    Arc::ptr_eq(fetcher, &backend.fetcher),
                    "deck changed during download"
                );
                return backend.serve(&uri);
            }
            remote_image::Status::Loading => tokio::time::sleep(Duration::from_millis(50)).await,
            _ => anyhow::bail!("image unavailable"),
        }
    }
    anyhow::bail!("image download timed out")
}

fn csp() -> &'static str {
    #[cfg(target_os = "windows")]
    {
        "default-src 'self' http://nexus.localhost; img-src 'self' http://nexus.localhost data:; style-src 'self' 'unsafe-inline'; script-src 'self' http://nexus.localhost; connect-src 'none'; object-src 'none'; frame-src 'none'; base-uri 'none'; form-action 'none'"
    }
    #[cfg(not(target_os = "windows"))]
    {
        "default-src 'self' nexus:; img-src 'self' nexus: data:; style-src 'self' 'unsafe-inline'; script-src 'self' nexus:; connect-src 'none'; object-src 'none'; frame-src 'none'; base-uri 'none'; form-action 'none'"
    }
}
