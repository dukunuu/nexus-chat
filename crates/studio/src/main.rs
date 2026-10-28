//! Native studio shell. Tests exercise the backend without constructing a window.

mod backend;

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{Context as _, Result};
use backend::Backend;
use nexus_core::deck::link::{Line, Message as LinkMessage};
use nexus_core::{db, remote_image, space};
use std::io::{BufRead as _, Write as _};
use tao::dpi::LogicalSize;
use tao::event::{Event, WindowEvent};
use tao::event_loop::{ControlFlow, EventLoopBuilder, EventLoopProxy};
use tao::window::WindowBuilder;
use wry::http::Response;
use wry::{NewWindowResponse, WebViewBuilder};

#[cfg(target_os = "windows")]
const HOME: &str = "http://nexus.localhost/index.html";
#[cfg(not(target_os = "windows"))]
const HOME: &str = "nexus://localhost/index.html";

enum Message {
    Command(String),
    /// A native dialog was answered (on its helper thread): finish the command.
    Dialog(backend::Answer),
    Link(LinkMessage),
    /// A line from nexus that speaks another link version: the window says so.
    LinkError(String),
    Poll,
}

fn main() -> Result<()> {
    let runtime = tokio::runtime::Runtime::new()?;
    let _runtime = runtime.enter();
    let mut args = std::env::args_os().skip(1);
    let first = args.next();
    let attached = first.as_deref() == Some(std::ffi::OsStr::new("--attach"));
    let path = if attached { args.next() } else { first }.map(PathBuf::from);
    let space = space::Space::open()?;
    let db = db::Db::open(&space.db_path())?;
    let default_id = db.default_space_id()?;
    let active = db
        .list_spaces()?
        .into_iter()
        .find(|s| s.id == default_id)
        .context("missing active space")?;
    let backend = Arc::new(Mutex::new(Backend::launch(
        path.as_deref(),
        &space.files_dir(&active.name),
        &space.root,
    )?));
    {
        let mut backend = backend.lock().expect("backend lock");
        backend.linked = attached;
        backend.hello();
        flush_link(&mut backend)?;
    }
    let event_loop = EventLoopBuilder::<Message>::with_user_event().build();
    let window = WindowBuilder::new()
        .with_title(format!(
            "nexus studio — {}",
            backend.lock().expect("backend lock").title()
        ))
        .with_inner_size(LogicalSize::new(1280.0, 800.0))
        .with_min_inner_size(LogicalSize::new(900.0, 600.0))
        .build(&event_loop)?;
    let proxy = event_loop.create_proxy();
    let dialog_proxy = proxy.clone();
    start_threads(&proxy, attached);
    let protocol_backend = Arc::clone(&backend);
    let handle = runtime.handle().clone();
    let builder = WebViewBuilder::new()
        .with_devtools(false)
        .with_asynchronous_custom_protocol("nexus".into(), move |_, request, responder| {
            let uri = request.uri().to_string();
            let backend = Arc::clone(&protocol_backend);
            handle.spawn(async move { responder.respond(respond(&backend, &uri).await) });
        })
        .with_navigation_handler(|url| url == HOME || url == HOME.replace("/index.html", "/"))
        .with_new_window_req_handler(|_, _| NewWindowResponse::Deny)
        .with_ipc_handler(move |request| {
            if backend::protocol_path(&request.uri().to_string())
                .is_ok_and(|path| matches!(path.as_str(), "/" | "/index.html"))
            {
                let _ = proxy.send_event(Message::Command(request.body().clone()));
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
            Event::UserEvent(message) => {
                if matches!(message, Message::Link(LinkMessage::Bye)) {
                    *control_flow = ControlFlow::Exit;
                    return;
                }
                if matches!(message, Message::Link(LinkMessage::Open { .. })) {
                    window.set_focus();
                }
                let (title, script) = handle_message(message, &backend, &dialog_proxy);
                if !script.is_empty() {
                    window.set_title(&title);
                    if let Err(error) = webview.evaluate_script(&script) {
                        eprintln!("studio: {error}");
                    }
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

/// The link's reader (nexus → studio on stdin, only when attached) and the
/// 500 ms tick that notices changes the model makes to the deck file.
fn start_threads(proxy: &EventLoopProxy<Message>, attached: bool) {
    if attached {
        let link_proxy = proxy.clone();
        std::thread::spawn(move || {
            for line in std::io::stdin().lock().lines() {
                let Ok(line) = line else {
                    break;
                };
                match Line::decode(&line) {
                    Ok(message) => {
                        if link_proxy.send_event(Message::Link(message)).is_err() {
                            return;
                        }
                    }
                    Err(error) if error.to_string().contains("update nexus") => {
                        let _ = link_proxy.send_event(Message::LinkError(error.to_string()));
                    }
                    Err(_) => {}
                }
            }
            let _ = link_proxy.send_event(Message::Link(LinkMessage::Bye));
        });
    }
    let poll_proxy = proxy.clone();
    std::thread::spawn(move || {
        loop {
            std::thread::sleep(Duration::from_millis(500));
            if poll_proxy.send_event(Message::Poll).is_err() {
                break;
            }
        }
    });
}

/// One `nexus://` request: embedded UI, deck assets, or a vetted remote image.
async fn respond(backend: &Arc<Mutex<Backend>>, uri: &str) -> Response<Vec<u8>> {
    let (remote, thumbnail) = {
        let backend = backend.lock().expect("backend lock");
        (backend.remote_image(uri), backend.thumbnail_job(uri))
    };
    let result = if let Ok((fetcher, src)) = remote {
        remote_bytes(backend, &fetcher, &src).await
    } else if let Ok((files, entry)) = thumbnail {
        thumbnail_bytes(files, entry).await
    } else {
        backend.lock().expect("backend lock").serve(uri)
    };
    let (status, mime, bytes) = match result {
        Ok((mime, bytes)) => (200, mime, bytes),
        Err(_) => (
            404,
            "text/plain; charset=utf-8",
            b"Resource unavailable".to_vec(),
        ),
    };
    let cache_control = if status == 200 {
        backend::cache_control(uri, &bytes)
    } else {
        "no-store"
    };
    Response::builder()
        .status(status)
        .header("Content-Type", mime)
        // WebKit custom schemes can have an opaque origin. Only
        // embedded pages may navigate here; allow their ES modules.
        .header("Access-Control-Allow-Origin", "*")
        .header("Content-Security-Policy", csp())
        .header("X-Content-Type-Options", "nosniff")
        .header("Cache-Control", cache_control)
        .body(bytes)
        .expect("static response headers")
}

fn flush_link(backend: &mut Backend) -> Result<()> {
    let mut stdout = std::io::stdout().lock();
    for message in backend.outgoing.drain(..) {
        stdout.write_all(Line::encode(message)?.as_bytes())?;
    }
    stdout.flush()?;
    Ok(())
}

fn handle_message(
    message: Message,
    backend: &Mutex<Backend>,
    proxy: &EventLoopProxy<Message>,
) -> (String, String) {
    let mut backend = backend.lock().expect("backend lock");
    // A dialog's answer finishes the command that asked, so it is that command's reply.
    let asynchronous = !matches!(message, Message::Command(_) | Message::Dialog(_));
    let result = match message {
        Message::LinkError(message) => Ok(Some(backend::Event::Error { message })),
        Message::Command(source) => match backend.prepare_dialog(&source) {
            // A native dialog is shown by a helper thread, never inside this
            // handler: macOS runs the panel on the main thread (rfd hands it
            // over), and the window's event handler and the backend stay free
            // while it is open. The answer comes back as `Message::Dialog`.
            Some(Ok(job)) => {
                let proxy = proxy.clone();
                std::thread::spawn(move || {
                    let _ = proxy.send_event(Message::Dialog(job.ask()));
                });
                return (format!("nexus studio — {}", backend.title()), String::new());
            }
            Some(Err(error)) => Ok(Some(backend::Event::Error {
                message: error.to_string(),
            })),
            None => Ok(Some(backend.dispatch(&source))),
        },
        Message::Dialog(answer) => Ok(Some(backend.finish_dialog(answer))),
        Message::Link(message) => backend.receive_link(message).map(Some),
        Message::Poll => backend.poll(),
    };
    let event = result.unwrap_or_else(|error| {
        Some(backend::Event::Error {
            message: error.to_string(),
        })
    });
    if let Err(error) = flush_link(&mut backend) {
        eprintln!("studio link: {error}");
        backend.linked = false;
    }
    let script = event.map_or_else(String::new, |event| {
        let script = event.script();
        if asynchronous {
            script.replacen("window.studioEvent(", "window.studioAsyncEvent(", 1)
        } else {
            script
        }
    });
    (format!("nexus studio — {}", backend.title()), script)
}

/// A library thumbnail, drawn off the backend's lock so a slow render never
/// holds up a click, and one at a time so a full library does not start dozens.
async fn thumbnail_bytes(
    files: PathBuf,
    entry: nexus_core::canvas::Entry,
) -> Result<(&'static str, Vec<u8>)> {
    static ONE_AT_A_TIME: Mutex<()> = Mutex::new(());
    tokio::task::spawn_blocking(move || {
        let _turn = ONE_AT_A_TIME
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let path = nexus_core::canvas::ensure_thumbnail(&files, &entry)?;
        Ok(("image/png", std::fs::read(path)?))
    })
    .await?
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

#[cfg(test)]
mod tests {
    /// The page may load only its own origin and `data:` images, and may not connect
    /// anywhere. Doc mode adds no source to it.
    #[test]
    #[cfg(not(target_os = "windows"))]
    fn the_content_security_policy_is_unchanged() {
        assert_eq!(
            super::csp(),
            "default-src 'self' nexus:; img-src 'self' nexus: data:; style-src 'self' 'unsafe-inline'; script-src 'self' nexus:; connect-src 'none'; object-src 'none'; frame-src 'none'; base-uri 'none'; form-action 'none'"
        );
    }
}
