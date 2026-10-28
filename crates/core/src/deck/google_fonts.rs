//! Google Fonts on demand. A family a deck uses that this device does not have
//! is downloaded once per weight into the deck folder's `assets/fonts/` and
//! listed in the theme's `font_files`, so renders, studio and exports all draw
//! it, and a PPTX opens with the same family in Google Slides.
//!
//! Only two hosts are contacted: `fonts.googleapis.com` for the stylesheet
//! (which, asked by a plain client, names one static TTF per weight) and
//! `fonts.gstatic.com` for the files. Family names are plain words, files are
//! bounded and checked as fonts before they are kept, and families Google does
//! not have are remembered for ten minutes.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::future::Future;
use std::path::Path;
use std::pin::Pin;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use anyhow::{Context as _, Result, ensure};

use super::{Deck, Font, FontFile, Kind};

const STYLESHEET: &str = "https://fonts.googleapis.com/css2";
const FILES_HOST: &str = "fonts.gstatic.com";
const MAX_FONT_BYTES: usize = 4 * 1024 * 1024;
const MAX_FAMILIES: usize = 4;
const MAX_WEIGHTS: usize = 4;
const MISSING_TTL: Duration = Duration::from_secs(600);

/// Families every renderer already maps to something sensible.
const GENERIC: &[&str] = &[
    "serif",
    "sans-serif",
    "monospace",
    "system-ui",
    "ui-serif",
    "ui-sans-serif",
    "ui-monospace",
    "cursive",
    "fantasy",
];

pub type Fetched<'a> = Pin<Box<dyn Future<Output = Result<Vec<u8>>> + Send + 'a>>;

/// Where the bytes come from: the network, or a fixture in tests.
pub trait Source: Send + Sync {
    /// The body of an https GET, bounded to `limit` bytes; an error for any
    /// status other than 200.
    fn get<'a>(&'a self, url: &'a str, limit: usize) -> Fetched<'a>;
}

/// The real source: rustls, no cookies or proxies, ten seconds per request.
pub struct Http(reqwest::Client);

impl Http {
    /// # Errors
    /// When the HTTP client cannot be built.
    pub fn new() -> Result<Self> {
        Ok(Self(
            reqwest::Client::builder()
                .user_agent("nexus-studio")
                .timeout(Duration::from_secs(10))
                .no_proxy()
                .redirect(reqwest::redirect::Policy::none())
                .build()?,
        ))
    }
}

impl Source for Http {
    fn get<'a>(&'a self, url: &'a str, limit: usize) -> Fetched<'a> {
        Box::pin(async move {
            let parsed = reqwest::Url::parse(url)?;
            ensure!(
                parsed.scheme() == "https"
                    && matches!(parsed.host_str(), Some("fonts.googleapis.com" | FILES_HOST)),
                "not a Google Fonts address"
            );
            let mut response = self.0.get(parsed).send().await?;
            ensure!(response.status() == 200, "HTTP {}", response.status());
            let mut body = Vec::new();
            while let Some(chunk) = response.chunk().await? {
                ensure!(
                    body.len() + chunk.len() <= limit,
                    "larger than {limit} bytes"
                );
                body.extend_from_slice(&chunk);
            }
            Ok(body)
        })
    }
}

/// A family name Google Fonts could have: letters, digits and spaces.
#[must_use]
pub fn plausible(family: &str) -> bool {
    !family.is_empty()
        && family.len() <= 48
        && family
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == ' ')
        && !GENERIC.contains(&family.to_ascii_lowercase().as_str())
}

/// The stylesheet that names `family`'s file for one weight.
#[must_use]
pub fn stylesheet_url(family: &str, weight: u16) -> String {
    format!(
        "{STYLESHEET}?family={}:wght@{weight}",
        family.replace(' ', "+")
    )
}

/// The upright TTF a stylesheet names, if it names one on the files host.
#[must_use]
pub fn ttf_url(css: &str) -> Option<String> {
    css.split("@font-face")
        .filter(|face| !face.contains("font-style: italic"))
        .find_map(|face| {
            let start = face.find("url(")? + 4;
            let url = &face[start..start + face[start..].find(')')?];
            (url.starts_with(&format!("https://{FILES_HOST}/"))
                && Path::new(url)
                    .extension()
                    .is_some_and(|e| e.eq_ignore_ascii_case("ttf")))
            .then(|| url.to_string())
        })
}

/// `Space Grotesk`, 700 → `space-grotesk-700.ttf`.
#[must_use]
pub fn file_name(family: &str, weight: u16) -> String {
    format!(
        "{}-{weight}.ttf",
        family.to_ascii_lowercase().replace(' ', "-")
    )
}

/// The families and weights a deck draws with, by family name: theme roles
/// resolved, fallback lists cut to their first name, weights snapped to the
/// hundreds Google serves. Regular is always included.
#[must_use]
pub fn wanted(deck: &Deck) -> BTreeMap<String, BTreeSet<u16>> {
    let roles = &deck.theme.fonts;
    let family_of = |name: &str| {
        let first = name
            .split(',')
            .next()
            .unwrap_or(name)
            .trim()
            .trim_matches(['\'', '"']);
        roles
            .get(first)
            .cloned()
            .unwrap_or_else(|| first.to_string())
    };
    let mut out: BTreeMap<String, BTreeSet<u16>> = BTreeMap::new();
    let mut add = |font: &Font, fallback_role: &str| {
        let family = family_of(font.family.as_deref().unwrap_or(fallback_role));
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)] // 100..=900
        let weight = ((f64::from(font.weight.unwrap_or(400)) / 100.0).round() * 100.0)
            .clamp(100.0, 900.0) as u16;
        let weights = out.entry(family).or_default();
        weights.insert(400);
        weights.insert(weight);
    };
    for family in roles.values() {
        add(
            &Font {
                family: Some(family.clone()),
                ..Font::default()
            },
            "body",
        );
    }
    for font in deck.theme.text_styles.values() {
        add(font, "body");
    }
    for slide in &deck.slides {
        for node in slide.nodes() {
            if let Kind::Text { style, font, .. }
            | Kind::Table { style, font, .. }
            | Kind::Code { style, font, .. } = &node.kind
            {
                let base = deck
                    .theme
                    .text_styles
                    .get(style)
                    .cloned()
                    .unwrap_or_default();
                add(&base.merged(font), "body");
            }
        }
    }
    out
}

fn missing() -> &'static Mutex<HashMap<String, Instant>> {
    static MISSING: OnceLock<Mutex<HashMap<String, Instant>>> = OnceLock::new();
    MISSING.get_or_init(Mutex::default)
}

fn known_missing(family: &str) -> bool {
    let mut map = missing().lock().expect("missing fonts");
    map.retain(|_, since| since.elapsed() < MISSING_TTL);
    map.contains_key(&family.to_ascii_lowercase())
}

/// Download one weight into `fonts_dir`, unless it is already there.
async fn fetch_weight(
    source: &dyn Source,
    fonts_dir: &Path,
    family: &str,
    weight: u16,
) -> Result<()> {
    let path = fonts_dir.join(file_name(family, weight));
    if path.exists() {
        return Ok(());
    }
    let css = source
        .get(&stylesheet_url(family, weight), 64 * 1024)
        .await?;
    let url = ttf_url(&String::from_utf8_lossy(&css)).context("no TTF in the stylesheet")?;
    let bytes = source.get(&url, MAX_FONT_BYTES).await?;
    let mut db = resvg::usvg::fontdb::Database::new();
    db.load_font_data(bytes.clone());
    ensure!(db.faces().next().is_some(), "not a font file");
    std::fs::create_dir_all(fonts_dir)?;
    super::write_atomic(&path, &bytes)
}

/// Make every Google family `deck` uses drawable here: download what is
/// missing into `assets/fonts/` beside the deck and add it to the theme.
/// Returns a line per family that could not be fetched; the deck then draws
/// it with the fallback. Never fails the edit.
pub async fn provide(
    source: &dyn Source,
    assets: &Path,
    deck: &mut Deck,
    installed: &[String],
) -> Vec<String> {
    let installed: BTreeSet<String> = installed.iter().map(|f| f.to_ascii_lowercase()).collect();
    let mut problems = Vec::new();
    let fonts_dir = assets.join("fonts");
    let wanted = wanted(deck)
        .into_iter()
        .filter(|(family, _)| {
            plausible(family) && !installed.contains(&family.to_ascii_lowercase())
        })
        .take(MAX_FAMILIES);
    for (family, weights) in wanted {
        if known_missing(&family) {
            problems.push(format!(
                "font {family:?} is not installed or on Google Fonts; it draws with the fallback"
            ));
            continue;
        }
        for weight in weights.into_iter().take(MAX_WEIGHTS) {
            let present =
                deck.theme.font_files.iter().any(|f| {
                    f.family.eq_ignore_ascii_case(&family) && f.weight == weight && !f.italic
                });
            if present {
                continue;
            }
            match fetch_weight(source, &fonts_dir, &family, weight).await {
                Ok(()) => deck.theme.font_files.push(FontFile {
                    family: family.clone(),
                    file: format!("assets/fonts/{}", file_name(&family, weight)),
                    weight,
                    italic: false,
                    fallback: Vec::new(),
                }),
                // A weight the family lacks is drawn from the nearest one.
                Err(_) if weight != 400 => {}
                Err(error) => {
                    // Google answers 400 for a family it does not have.
                    if error.to_string() == "HTTP 400 Bad Request" {
                        missing()
                            .lock()
                            .expect("missing fonts")
                            .insert(family.to_ascii_lowercase(), Instant::now());
                    }
                    problems.push(format!("font {family:?} is not installed and could not be fetched from Google Fonts ({error:#}); it draws with the fallback"));
                    break;
                }
            }
        }
    }
    problems
}

/// [`provide`] for synchronous callers such as studio's backend: it runs on a
/// thread with its own runtime, so it works inside or outside Tokio.
pub fn provide_blocking(
    source: &dyn Source,
    assets: &Path,
    deck: &mut Deck,
    installed: &[String],
) -> Vec<String> {
    std::thread::scope(|scope| {
        scope
            .spawn(|| {
                match tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                {
                    Ok(runtime) => runtime.block_on(provide(source, assets, deck, installed)),
                    Err(error) => vec![format!("fonts were not fetched: {error}")],
                }
            })
            .join()
            .unwrap_or_else(|_| vec!["fonts were not fetched".into()])
    })
}

/// A source answering from a table of URLs (anything else is Google's "no
/// such family"), for tests anywhere in the crate.
#[cfg(test)]
pub(crate) struct Table(pub HashMap<String, Vec<u8>>);

#[cfg(test)]
impl Table {
    /// A table serving `family` at `weights`, every file being `ttf`.
    pub(crate) fn family(family: &str, weights: &[u16], ttf: &[u8]) -> Self {
        let mut table = HashMap::new();
        for weight in weights {
            let file = format!("https://{FILES_HOST}/s/test/{}", file_name(family, *weight));
            let css = format!(
                "@font-face {{ font-style: normal; src: url({file}) format('truetype'); }}"
            );
            table.insert(stylesheet_url(family, *weight), css.into_bytes());
            table.insert(file, ttf.to_vec());
        }
        Self(table)
    }
}

#[cfg(test)]
impl Source for Table {
    fn get<'a>(&'a self, url: &'a str, limit: usize) -> Fetched<'a> {
        let answer = self.0.get(url).cloned();
        Box::pin(async move {
            let bytes = answer.ok_or_else(|| anyhow::anyhow!("HTTP 400 Bad Request"))?;
            ensure!(bytes.len() <= limit, "larger than {limit} bytes");
            Ok(bytes)
        })
    }
}

/// Any installed TTF, standing in for a downloaded one in tests.
#[cfg(test)]
pub(crate) fn installed_ttf() -> Option<Vec<u8>> {
    super::text::system_fonts()
        .faces()
        .find_map(|face| match &face.source {
            resvg::usvg::fontdb::Source::File(path)
                if path.extension().is_some_and(|e| e == "ttf")
                    && std::fs::metadata(path).is_ok_and(|m| m.len() < 2_000_000) =>
            {
                std::fs::read(path).ok()
            }
            _ => None,
        })
}

#[cfg(test)]
mod tests;
