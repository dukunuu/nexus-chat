//! Standalone playback, bundling studio's actual renderer and animation modules.

use super::super::{Deck, Kind, layout::image_path, text::escape};
use anyhow::Result;
use base64::Engine as _;
use std::path::Path;

const CSS: &str = include_str!("../../../../studio/ui/studio.css");
const INDEX: &str = include_str!("../../../../studio/ui/index.html");
const MODULES: [&str; 4] = [
    include_str!("../../../../studio/ui/helpers.js"),
    include_str!("../../../../studio/ui/render.js"),
    include_str!("../../../../studio/ui/animations.js"),
    include_str!("../../../../studio/ui/playback.js"),
];

fn data(mime: &str, bytes: &[u8]) -> String {
    format!(
        "data:{mime};base64,{}",
        base64::engine::general_purpose::STANDARD.encode(bytes)
    )
}

fn inline_nodes(
    nodes: &mut [super::super::Node],
    assets: Option<&Path>,
    warnings: &mut Vec<String>,
) {
    for node in nodes {
        match &mut node.kind {
            Kind::Frame { children, .. } => inline_nodes(children, assets, warnings),
            Kind::Image { src, .. } => {
                let png = image_path(src, assets)
                    .and_then(|p| std::fs::read(p).ok())
                    .and_then(|b| image::load_from_memory(&b).ok())
                    .and_then(|image| {
                        let mut png = std::io::Cursor::new(Vec::new());
                        image.write_to(&mut png, image::ImageFormat::Png).ok()?;
                        Some(png.into_inner())
                    });
                *src = if let Some(png) = png {
                    data("image/png", &png)
                } else {
                    warnings.push(format!(
                        "{}: image unavailable; exported a labelled placeholder ({src}).",
                        node.id
                    ));
                    data("image/svg+xml", b"<svg xmlns='http://www.w3.org/2000/svg' width='640' height='360'><rect width='640' height='360' fill='#dddcd5'/><text x='20' y='50' font-size='28'>Image not available</text></svg>")
                };
            }
            _ => {}
        }
    }
}

/// Self-contained HTML with hidden notes and the same keyboard/build playback as studio.
pub fn export_html(deck: &Deck, assets: Option<&Path>) -> Result<String> {
    with_warnings(deck, assets, &mut Vec::new())
}

pub(super) fn with_warnings(
    deck: &Deck,
    assets: Option<&Path>,
    warnings: &mut Vec<String>,
) -> Result<String> {
    let mut deck = deck.clone();
    for slide in &mut deck.slides {
        slide.notes.clear();
        if let Some(background) = &mut slide.background {
            let mut node = super::super::Node::new(
                "background",
                Kind::Image {
                    src: background.image.clone(),
                    fit: background.fit,
                },
            );
            inline_nodes(std::slice::from_mut(&mut node), assets, warnings);
            if let Kind::Image { src, .. } = node.kind {
                background.image = src;
            }
        }
        inline_nodes(&mut slide.children, assets, warnings);
    }
    for font in &mut deck.theme.font_files {
        if let Some(bytes) = image_path(&font.file, assets).and_then(|p| std::fs::read(p).ok()) {
            let mime = match Path::new(&font.file).extension().and_then(|e| e.to_str()) {
                Some("woff2") => "font/woff2",
                Some("otf") => "font/otf",
                _ => "font/ttf",
            };
            font.file = data(mime, &bytes);
        } else {
            warnings.push(format!(
                "{}: font file unavailable; HTML uses installed/fallback fonts.",
                font.family
            ));
            font.file.clear();
        }
    }
    deck.theme.font_files.retain(|f| !f.file.is_empty());
    // These modules use one-line relative imports and named exports only.
    // Concatenate their original source into a single lexical scope, so file://
    // playback needs neither module fetches, a server, nor a bundler dependency.
    let source = MODULES
        .iter()
        .flat_map(|s| s.lines())
        .filter(|line| !line.starts_with("import "))
        .map(|line| line.strip_prefix("export ").unwrap_or(line))
        .collect::<Vec<_>>()
        .join("\n");
    let json = serde_json::to_string(&deck)?
        .replace('<', "\\u003c")
        .replace('>', "\\u003e")
        .replace('&', "\\u0026");
    let presentation = INDEX
        .split_once("<div id=\"presentation\"")
        .expect("studio presentation markup")
        .1
        .split_once("</body>")
        .expect("studio body")
        .0;
    let boot = format!(
        r"
const state = {{ deck:{json}, images:{{}}, slideId:null, playStep:0 }};
const api = {{ state, toast:() => {{}}, render:() => document.querySelector('#present').hidden = false }};
const playback = playbackController(api);
document.addEventListener('keydown', event => {{
  if (!playback.key(event) && ['Enter',' ','F5'].includes(event.key)) {{ event.preventDefault(); playback.start(); }}
}});
Promise.all([document.fonts.ready, ...(state.deck.theme.font_files || []).map(file => {{
  const face = new FontFace(file.family, `url(${{file.file}})`, {{weight:String(file.weight),style:file.italic?'italic':'normal'}});
  document.fonts.add(face); return face.load().catch(() => {{}});
}})]).then(() => playback.start());
"
    );
    Ok(format!(
        "<!doctype html><html lang=\"en\"><head><meta charset=\"utf-8\"><meta name=\"viewport\" content=\"width=device-width,initial-scale=1\"><meta http-equiv=\"Content-Security-Policy\" content=\"default-src 'none'; script-src 'unsafe-inline'; style-src 'unsafe-inline'; img-src data:; font-src data:\"><title>{}</title><style>{CSS}\n#presenter,#presenter-toggle,#play-from-here{{display:none!important}}.presenter-view{{padding:0}}.presenter-view #present-stage{{width:min(100vw,calc((100vh - 64px) * {} / {}))}}#present-stage{{aspect-ratio:{} / {};width:min(100vw,calc((100vh - 64px) * {} / {}))}}#present{{position:fixed;inset:40%;z-index:101}}body:has(#presentation:not([hidden])) > #present{{display:none}}</style></head><body><button id=\"present\">Start presentation · Enter</button><button id=\"play-from-here\" hidden></button><div id=\"presentation\"{presentation}<script>(()=>{{\n{}\n{boot}\n}})();</script></body></html>",
        escape(&deck.title),
        deck.theme.size.w,
        deck.theme.size.h,
        deck.theme.size.w,
        deck.theme.size.h,
        deck.theme.size.w,
        deck.theme.size.h,
        source.replace("</script", "<\\/script")
    ))
}
