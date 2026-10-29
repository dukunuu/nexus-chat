//! Device-local, bounded image downloads for replies, layouts and search
//! thumbnails. URLs are chosen by a model or a web page, so this is the one
//! place they are fetched, and every hop is vetted:
//!
//! * `https` only, no credentials, no templates (`validate_url`).
//! * Literal IPs are vetted before connecting; hostnames are resolved by
//!   [`VettedResolver`], which hands the HTTP client *only* addresses that
//!   passed the check. The client connects to exactly those addresses, so
//!   there is no second lookup a rebinding DNS server could answer
//!   differently.
//! * Redirects are followed by hand (at most three), each hop re-validated and
//!   re-resolved through the same vetted path.
//! * No proxies, cookies, referer or auth; a plain `User-Agent`.
//! * Six seconds in total, 8 MB, `image/png|jpeg|webp|gif` by content type and
//!   by actually decoding, downscaled to 1600 px, cached as PNG on this
//!   device, failures remembered for ten minutes, four fetches in flight.

use std::collections::HashMap;
use std::fmt::Write as _;
use std::future::Future;
use std::net::{IpAddr, SocketAddr};
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{Result, bail, ensure};
use reqwest::Url;
use reqwest::dns::{Addrs, Name, Resolve, Resolving};
use sha2::{Digest, Sha256};
use tokio::sync::{Semaphore, watch};

const MAX_BYTES: usize = 8 * 1024 * 1024;
const MAX_REDIRECTS: usize = 3;
const MAX_SIDE: u32 = 1600;
const TOTAL_TIMEOUT: Duration = Duration::from_secs(6);
const NEGATIVE_TTL: Duration = Duration::from_secs(600);
const IN_FLIGHT: usize = 4;
/// Site icons are small; anything bigger is not an icon.
const ICON_BYTES: usize = 1024 * 1024;
/// Icons are kept at this size: two terminal cells on a high-density screen.
const ICON_SIDE: u32 = 128;
/// How much of a home page is read looking for `<link rel=icon>`.
const PAGE_BYTES: usize = 512 * 1024;
/// `<link rel=icon>` candidates tried after `/favicon.ico`.
const ICON_LINKS: usize = 4;

/// A nonblocking preview lookup.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Status {
    /// Disabled by the user's preference.
    Off,
    /// Queued or downloading.
    Loading,
    /// Sanitized PNG on this device.
    Ready(PathBuf),
    /// Refused or unavailable (retried after ten minutes).
    Failed,
}

struct Entry {
    reason: Option<String>,
    status: Status,
    since: Instant,
}

type LookupFuture = Pin<Box<dyn Future<Output = std::io::Result<Vec<IpAddr>>> + Send>>;
type Lookup = Arc<dyn Fn(String) -> LookupFuture + Send + Sync>;

fn system_lookup() -> Lookup {
    Arc::new(|host: String| {
        Box::pin(async move {
            Ok(tokio::net::lookup_host((host.as_str(), 0))
                .await?
                .map(|address| address.ip())
                .collect())
        })
    })
}

/// DNS resolver given to the HTTP client. The client can only ever connect to
/// the addresses returned here, and none of them is non-public.
struct VettedResolver {
    lookup: Lookup,
    allow_loopback: bool,
}

impl Resolve for VettedResolver {
    fn resolve(&self, name: Name) -> Resolving {
        let lookup = Arc::clone(&self.lookup);
        let allow_loopback = self.allow_loopback;
        Box::pin(async move {
            let addresses = lookup(name.as_str().to_string()).await?;
            vet_addresses(&addresses, allow_loopback)?;
            let addresses: Addrs = Box::new(
                addresses
                    .into_iter()
                    .map(|ip| SocketAddr::new(ip, 0))
                    .collect::<Vec<_>>()
                    .into_iter(),
            );
            Ok(addresses)
        })
    }
}

/// Shared by reply images, layout images and search thumbnails.
pub struct Fetcher {
    directory: PathBuf,
    enabled: AtomicBool,
    changes: AtomicUsize,
    wake: tokio::sync::Notify,
    entries: Mutex<HashMap<String, Entry>>,
    slots: Semaphore,
    preference: watch::Sender<bool>,
    lookup: Lookup,
    timeout: Duration,
    /// Only ever true in unit tests, so they can reach a 127.0.0.1 mock server.
    allow_loopback: bool,
}

impl Fetcher {
    /// `directory` is a device-local cache root, never a space directory.
    #[must_use]
    pub fn new(directory: impl AsRef<std::path::Path>) -> Arc<Self> {
        Arc::new(Self::build(directory.as_ref(), system_lookup(), false))
    }

    fn build(directory: &std::path::Path, lookup: Lookup, allow_loopback: bool) -> Self {
        Self {
            directory: directory.join("images"),
            enabled: AtomicBool::new(true),
            changes: AtomicUsize::new(0),
            wake: tokio::sync::Notify::new(),
            entries: Mutex::new(HashMap::new()),
            slots: Semaphore::new(IN_FLIGHT),
            preference: watch::channel(true).0,
            lookup,
            timeout: TOTAL_TIMEOUT,
            allow_loopback,
        }
    }

    /// Disabling also cancels queued and active downloads.
    pub fn set_enabled(&self, enabled: bool) {
        if self.enabled.swap(enabled, Ordering::SeqCst) != enabled {
            self.preference.send_replace(enabled);
            self.changes.fetch_add(1, Ordering::Relaxed);
            self.wake.notify_one();
        }
    }

    /// Resolves after a download finishes or the preference flips, so an idle
    /// event loop can redraw. A change that lands before the call is not lost.
    pub async fn changed(&self) {
        self.wake.notified().await;
    }

    /// Whether downloads are currently allowed.
    #[must_use]
    pub fn enabled(&self) -> bool {
        self.enabled.load(Ordering::SeqCst)
    }

    /// Completion/preference generation, for view-side invalidation.
    #[must_use]
    pub fn generation(&self) -> usize {
        self.changes.load(Ordering::Relaxed)
    }

    /// Return only an already vetted cache file, without scheduling network work.
    #[must_use]
    pub fn cached(&self, url: &str) -> Option<PathBuf> {
        if !self.enabled() || validate_url_with(url, self.allow_loopback).is_err() {
            return None;
        }
        let path = self.path(url);
        path.is_file().then_some(path)
    }

    /// Look up a URL and, if needed, enqueue one bounded background download.
    #[must_use]
    pub fn request(self: &Arc<Self>, url: &str) -> Status {
        self.schedule(url, url, false)
    }

    /// The icon of the site at `origin` (`https://host`): its `/favicon.ico`,
    /// else the first usable `<link rel=icon>` on its home page. Only that
    /// site is asked; vetting, caching and failure memory match images.
    #[must_use]
    pub fn favicon(self: &Arc<Self>, origin: &str) -> Status {
        self.schedule(&icon_key(origin), origin, true)
    }

    fn schedule(self: &Arc<Self>, key: &str, url: &str, icon: bool) -> Status {
        if !self.enabled() {
            return Status::Off;
        }
        let mut entries = self.entries.lock().expect("image cache lock");
        if let Some(entry) = entries.get(key)
            && (entry.status != Status::Failed || entry.since.elapsed() < NEGATIVE_TTL)
        {
            return entry.status.clone();
        }
        let path = self.path(key);
        let reason = validate_url_with(url, self.allow_loopback)
            .err()
            .map(|error| error.to_string());
        let status = if reason.is_some() {
            Status::Failed
        } else if path.is_file() {
            Status::Ready(path)
        } else {
            Status::Loading
        };
        if status != Status::Loading {
            entries.insert(
                key.into(),
                Entry {
                    reason,
                    status: status.clone(),
                    since: Instant::now(),
                },
            );
            return status;
        }
        let Ok(runtime) = tokio::runtime::Handle::try_current() else {
            // No executor to download on yet; ask again next frame.
            return Status::Loading;
        };
        entries.insert(
            key.into(),
            Entry {
                reason: None,
                status: Status::Loading,
                since: Instant::now(),
            },
        );
        let fetcher = Arc::clone(self);
        let (key, url) = (key.to_string(), url.to_string());
        let mut preference = self.preference.subscribe();
        runtime.spawn(async move {
            let job = async {
                if icon {
                    fetcher.download_favicon(&url).await
                } else {
                    fetcher.download(&url).await
                }
            };
            let result = tokio::select! {
                result = job => Some(result),
                _ = preference.changed() => None,
            };
            let mut entries = fetcher.entries.lock().expect("image cache lock");
            match result {
                // Cancelled by the preference: forget it so turning images back
                // on retries instead of serving a stale failure.
                None => {
                    entries.remove(&key);
                }
                Some(result) => {
                    let reason = result.as_ref().err().map(|error| format!("{error:#}"));
                    let status = result.map_or(Status::Failed, Status::Ready);
                    entries.insert(
                        key,
                        Entry {
                            reason,
                            status,
                            since: Instant::now(),
                        },
                    );
                }
            }
            drop(entries);
            // An icon is drawn over text that is already laid out, so it
            // only needs a redraw, not a new layout generation.
            if !icon {
                fetcher.changes.fetch_add(1, Ordering::Relaxed);
            }
            fetcher.wake.notify_one();
        });
        Status::Loading
    }

    /// Wait for the same vetted, cached download used by nonblocking previews.
    /// # Errors
    /// Returns the download's actual failure reason (including timeouts).
    pub async fn fetch(self: &Arc<Self>, url: &str) -> Result<Status> {
        loop {
            match self.request(url) {
                Status::Loading => tokio::time::sleep(Duration::from_millis(20)).await,
                Status::Failed => {
                    let entries = self.entries.lock().expect("image cache lock");
                    bail!(
                        "{}",
                        entries
                            .get(url)
                            .and_then(|entry| entry.reason.as_deref())
                            .unwrap_or("image unavailable")
                    );
                }
                status => return Ok(status),
            }
        }
    }

    /// Loopback HTTP is available only to hermetic unit tests.
    #[cfg(test)]
    pub(crate) fn for_test(directory: &std::path::Path) -> Arc<Self> {
        Arc::new(Self::build(directory, system_lookup(), true))
    }

    /// Test hook: pretend `url` finished downloading with these PNG bytes.
    /// # Errors
    /// Fails when the cache file cannot be written.
    #[doc(hidden)]
    pub fn seed(&self, url: &str, png: &[u8]) -> Result<()> {
        let path = self.path(url);
        std::fs::create_dir_all(path.parent().expect("image cache parent"))?;
        std::fs::write(&path, png)?;
        self.entries.lock().expect("image cache lock").insert(
            url.into(),
            Entry {
                reason: None,
                status: Status::Ready(path),
                since: Instant::now(),
            },
        );
        self.changes.fetch_add(1, Ordering::Relaxed);
        Ok(())
    }

    /// Test hook: pretend `url` failed to download.
    #[doc(hidden)]
    pub fn seed_failure(&self, url: &str) {
        self.entries.lock().expect("image cache lock").insert(
            url.into(),
            Entry {
                reason: Some("image unavailable".into()),
                status: Status::Failed,
                since: Instant::now(),
            },
        );
        self.changes.fetch_add(1, Ordering::Relaxed);
    }

    fn path(&self, url: &str) -> PathBuf {
        let mut name = String::with_capacity(68);
        for byte in Sha256::digest(url.as_bytes()) {
            let _ = write!(name, "{byte:02x}");
        }
        name.push_str(".png");
        self.directory.join(name)
    }

    /// Download, vet, decode and cache one image. Waits for a free slot first.
    async fn download(&self, source: &str) -> Result<PathBuf> {
        let _slot = self.slots.acquire().await?;
        ensure!(self.enabled(), "images disabled");
        tokio::time::timeout(self.timeout, self.download_inner(source)).await?
    }

    fn client(&self) -> Result<reqwest::Client> {
        Ok(reqwest::Client::builder()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .referer(false)
            .user_agent(concat!("nexus-chat/", env!("CARGO_PKG_VERSION")))
            .dns_resolver(Arc::new(VettedResolver {
                lookup: Arc::clone(&self.lookup),
                allow_loopback: self.allow_loopback,
            }))
            .build()?)
    }

    /// GET `source` through vetted redirects: the final URL, the lowercased
    /// media type (refused unless `accept`ed, before any body is read) and at
    /// most `limit` bytes. Past the limit a `partial` read stops quietly;
    /// otherwise it is an error.
    async fn get(
        &self,
        client: &reqwest::Client,
        source: &str,
        (limit, partial): (usize, bool),
        accept: impl Fn(&str) -> bool,
    ) -> Result<(Url, String, Vec<u8>)> {
        let mut url = validate_url_with(source, self.allow_loopback)?;
        let mut hops = 0;
        let mut response = loop {
            // Hostnames are vetted by the resolver at connect time; a literal IP
            // never reaches a resolver, so it is vetted here.
            if let Some(ip) = literal_ip(&url) {
                vet_addresses(&[ip], self.allow_loopback)?;
            }
            let response = client.get(url.clone()).send().await?;
            if !response.status().is_redirection() {
                break response;
            }
            ensure!(hops < MAX_REDIRECTS, "too many image redirects");
            hops += 1;
            let location = response
                .headers()
                .get(reqwest::header::LOCATION)
                .ok_or_else(|| anyhow::anyhow!("missing redirect location"))?
                .to_str()?;
            url = validate_url_with(url.join(location)?.as_str(), self.allow_loopback)?;
        };
        response.error_for_status_ref()?;
        let mime = response
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .unwrap_or("")
            .split(';')
            .next()
            .unwrap_or("")
            .trim()
            .to_ascii_lowercase();
        ensure!(accept(&mime), "unsupported content type: {mime}");
        ensure!(
            partial
                || response
                    .content_length()
                    .is_none_or(|length| length <= limit as u64),
            "image too large"
        );
        let mut bytes = Vec::new();
        while let Some(chunk) = response.chunk().await? {
            if bytes.len() + chunk.len() > limit {
                ensure!(partial, "image too large");
                bytes.extend_from_slice(&chunk[..limit - bytes.len()]);
                break;
            }
            bytes.extend_from_slice(&chunk);
        }
        Ok((url, mime, bytes))
    }

    async fn download_inner(&self, source: &str) -> Result<PathBuf> {
        let client = self.client()?;
        let (_, mime, bytes) = self
            .get(&client, source, (MAX_BYTES, false), |mime| {
                image::ImageFormat::from_mime_type(mime).is_some_and(|format| {
                    matches!(
                        format,
                        image::ImageFormat::Png
                            | image::ImageFormat::Jpeg
                            | image::ImageFormat::WebP
                            | image::ImageFormat::Gif
                    )
                })
            })
            .await?;
        let format = image::ImageFormat::from_mime_type(&mime)
            .ok_or_else(|| anyhow::anyhow!("unsupported content type: {mime}"))?;
        self.save(source, bytes, format, MAX_SIDE).await
    }

    async fn save(
        &self,
        key: &str,
        bytes: Vec<u8>,
        format: image::ImageFormat,
        side: u32,
    ) -> Result<PathBuf> {
        let target = self.path(key);
        let saved = target.clone();
        tokio::task::spawn_blocking(move || sanitize_to_png(bytes, format, &saved, side)).await??;
        Ok(target)
    }

    async fn download_favicon(&self, origin: &str) -> Result<PathBuf> {
        let _slot = self.slots.acquire().await?;
        ensure!(self.enabled(), "images disabled");
        tokio::time::timeout(self.timeout, self.favicon_inner(origin)).await?
    }

    async fn favicon_inner(&self, origin: &str) -> Result<PathBuf> {
        let client = self.client()?;
        let key = icon_key(origin);
        let site = validate_url_with(origin, self.allow_loopback)?;
        // The page's own icons first: they are usually larger than the
        // 16–32 px `/favicon.ico`, which looks blocky drawn two cells wide.
        let page = self
            .get(
                &client,
                site.join("/")?.as_str(),
                (PAGE_BYTES, true),
                |mime| matches!(mime, "text/html" | "application/xhtml+xml"),
            )
            .await;
        if let Ok((page, _, html)) = &page {
            for href in icon_links(&String::from_utf8_lossy(html))
                .into_iter()
                .take(ICON_LINKS)
            {
                let Ok(url) = page.join(&href) else {
                    continue;
                };
                if let Ok((bytes, format)) = self.icon(&client, url.as_str()).await {
                    return self.save(&key, bytes, format, ICON_SIDE).await;
                }
            }
        }
        match self
            .icon(&client, site.join("/favicon.ico")?.as_str())
            .await
        {
            Ok((bytes, format)) => self.save(&key, bytes, format, ICON_SIDE).await,
            Err(error) => bail!("no site icon: {error:#}"),
        }
    }

    /// An icon's bytes and format. ICO is common and often mislabelled, so an
    /// unknown media type is sniffed, still only among raster formats.
    async fn icon(
        &self,
        client: &reqwest::Client,
        url: &str,
    ) -> Result<(Vec<u8>, image::ImageFormat)> {
        use image::ImageFormat;
        let (_, mime, bytes) = self
            .get(client, url, (ICON_BYTES, false), |mime| {
                matches!(
                    mime,
                    "image/png"
                        | "image/jpeg"
                        | "image/webp"
                        | "image/x-icon"
                        | "image/vnd.microsoft.icon"
                        | "image/ico"
                        | ""
                        | "application/octet-stream"
                        | "text/plain"
                )
            })
            .await?;
        let format = match mime.as_str() {
            "image/png" => ImageFormat::Png,
            "image/jpeg" => ImageFormat::Jpeg,
            "image/webp" => ImageFormat::WebP,
            "image/x-icon" | "image/vnd.microsoft.icon" | "image/ico" => ImageFormat::Ico,
            "" | "application/octet-stream" | "text/plain" => image::guess_format(&bytes)?,
            _ => bail!("unsupported icon content type"),
        };
        ensure!(
            matches!(
                format,
                ImageFormat::Png | ImageFormat::Jpeg | ImageFormat::WebP | ImageFormat::Ico
            ),
            "unsupported icon format"
        );
        Ok((bytes, format))
    }
}

/// Cache key of a site's icon; the version retires icons fetched smallest-first.
fn icon_key(origin: &str) -> String {
    format!("favicon2 {origin}")
}

/// `href`s of a page's `<link rel="… icon …">` tags, largest declared size
/// first (an Apple touch icon counts as 180 px, an icon without sizes as 0),
/// without vector icons (they need a renderer) and Safari's mask icons.
fn icon_links(html: &str) -> Vec<String> {
    let lower = html.to_ascii_lowercase();
    let end = lower.find("</head").unwrap_or(lower.len());
    let mut links = Vec::new();
    let mut at = 0;
    while let Some(start) = lower[at..end].find("<link") {
        let start = at + start;
        let close = lower[start..end]
            .find('>')
            .map_or(end, |close| start + close);
        let attributes = tag_attributes(&html[start + 5..close]);
        at = close;
        let get = |name: &str| {
            attributes
                .iter()
                .find(|(key, _)| key == name)
                .map(|(_, value)| value.as_str())
        };
        let rel = get("rel").unwrap_or_default().to_ascii_lowercase();
        let Some(href) = get("href").filter(|href| !href.is_empty()) else {
            continue;
        };
        let vector = get("type").is_some_and(|kind| kind.contains("svg"))
            || href
                .split(['?', '#'])
                .next()
                .is_some_and(|path| path.to_ascii_lowercase().ends_with(".svg"));
        if rel.split_whitespace().any(|word| word.ends_with("icon"))
            && !rel.contains("mask-icon")
            && !vector
        {
            let declared = get("sizes")
                .unwrap_or_default()
                .split_whitespace()
                .filter_map(|size| size.split_once(['x', 'X']))
                .filter_map(|(width, _)| width.parse::<u32>().ok())
                .max();
            let size = declared.unwrap_or(if rel.contains("apple-touch-icon") {
                180
            } else {
                0
            });
            links.push((size, href.replace("&amp;", "&")));
        }
    }
    links.sort_by_key(|(size, _)| std::cmp::Reverse(*size));
    links.into_iter().map(|(_, href)| href).collect()
}

/// `name=value` pairs of one tag, names lowercased; values may be quoted.
fn tag_attributes(tag: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let mut rest = tag.trim_start_matches(['/', ' ']);
    while let Some(eq) = rest.find('=') {
        let name = rest[..eq]
            .rsplit(|ch: char| ch.is_whitespace())
            .next()
            .unwrap_or_default()
            .to_ascii_lowercase();
        let value = rest[eq + 1..].trim_start();
        let (value, tail) = if let Some(quote @ ('"' | '\'')) = value.chars().next() {
            let body = &value[1..];
            let close = body.find(quote).unwrap_or(body.len());
            (&body[..close], &body[(close + 1).min(body.len())..])
        } else {
            // Unquoted values run to whitespace; a closing `/>` is not part of one.
            let close = value.find(char::is_whitespace).unwrap_or(value.len());
            let token = &value[..close];
            let tail = &value[close..];
            (
                if tail.trim().is_empty() {
                    token.strip_suffix('/').unwrap_or(token)
                } else {
                    token
                },
                tail,
            )
        };
        out.push((name, value.to_string()));
        rest = tail;
    }
    out
}

/// Decode (first frame for animations), downscale, and write a PNG atomically.
fn sanitize_to_png(
    bytes: Vec<u8>,
    format: image::ImageFormat,
    target: &std::path::Path,
    side: u32,
) -> Result<()> {
    let mut reader = image::ImageReader::with_format(std::io::Cursor::new(bytes), format);
    let mut limits = image::Limits::default();
    limits.max_alloc = Some(128 * 1024 * 1024);
    limits.max_image_width = Some(16_384);
    limits.max_image_height = Some(16_384);
    reader.limits(limits);
    let image = reader.decode()?;
    let image = if image.width().max(image.height()) > side {
        image.resize(side, side, image::imageops::FilterType::Triangle)
    } else {
        image
    };
    let parent = target.parent().expect("image cache parent");
    std::fs::create_dir_all(parent)?;
    let temporary = target.with_extension(format!("tmp{}", uuid::Uuid::new_v4().simple()));
    let written = image.save_with_format(&temporary, image::ImageFormat::Png);
    if let Err(error) = written {
        let _ = std::fs::remove_file(&temporary);
        return Err(error.into());
    }
    std::fs::rename(&temporary, target).inspect_err(|_| {
        let _ = std::fs::remove_file(&temporary);
    })?;
    Ok(())
}

/// Validate untrusted image syntax without DNS or I/O. Layouts use this too.
/// # Errors
/// Rejects insecure schemes, credentials, missing hosts and templates.
pub fn validate_url(source: &str) -> Result<Url> {
    validate_url_with(source, false)
}

fn validate_url_with(source: &str, allow_test_http: bool) -> Result<Url> {
    ensure!(
        !source.contains(['{', '}']),
        "image URLs cannot contain templates"
    );
    ensure!(
        !source.chars().any(|c| c.is_control() || c.is_whitespace()),
        "image URLs cannot contain whitespace or control characters"
    );
    let url = Url::parse(source)?;
    ensure!(
        url.scheme() == "https" || (allow_test_http && url.scheme() == "http"),
        "image URL must use https"
    );
    ensure!(
        url.username().is_empty() && url.password().is_none(),
        "image URL credentials forbidden"
    );
    ensure!(url.host_str().is_some(), "missing image host");
    Ok(url)
}

/// `Some` when the URL's host is an IP literal (the URL parser has already
/// normalised decimal/hex/octal spellings of IPv4 into dotted form).
fn literal_ip(url: &Url) -> Option<IpAddr> {
    url.host_str()?.trim_matches(['[', ']']).parse().ok()
}

/// Refuse the whole set unless every address is publicly routable.
fn vet_addresses(addresses: &[IpAddr], allow_loopback: bool) -> Result<()> {
    ensure!(!addresses.is_empty(), "no image address");
    for address in addresses {
        let test_exempt = allow_loopback && *address == IpAddr::from([127, 0, 0, 1]);
        ensure!(
            test_exempt || public_address(*address),
            "non-public image address"
        );
    }
    Ok(())
}

fn public_address(address: IpAddr) -> bool {
    match address {
        IpAddr::V4(ip) => {
            let [a, b, c, _] = ip.octets();
            !(ip.is_private()
                || ip.is_loopback()
                || ip.is_link_local()
                || ip.is_multicast()
                || ip.is_unspecified()
                || ip.is_broadcast()
                || ip.is_documentation()
                || a == 0
                || a >= 240
                || (a == 100 && (64..=127).contains(&b))
                || (a == 192 && b == 0 && c == 0)
                || (a == 198 && (b == 18 || b == 19)))
        }
        IpAddr::V6(ip) => {
            if let Some(mapped) = ip.to_ipv4_mapped() {
                return public_address(IpAddr::V4(mapped));
            }
            let segments = ip.segments();
            // 64:ff9b::/96 (NAT64) embeds an IPv4 address in the last 32 bits.
            if segments[..6] == [0x64, 0xff9b, 0, 0, 0, 0] {
                let [a, b] = segments[6].to_be_bytes();
                let [c, d] = segments[7].to_be_bytes();
                return public_address(IpAddr::from([a, b, c, d]));
            }
            // Deprecated IPv4-compatible ::a.b.c.d (not ::1 or ::) embeds IPv4 too.
            if segments[..6] == [0; 6] && !ip.is_loopback() && !ip.is_unspecified() {
                let [a, b] = segments[6].to_be_bytes();
                let [c, d] = segments[7].to_be_bytes();
                return public_address(IpAddr::from([a, b, c, d]));
            }
            let first = segments[0];
            !(ip.is_loopback()
                || ip.is_unspecified()
                || ip.is_multicast()
                || first & 0xfe00 == 0xfc00
                || first & 0xffc0 == 0xfe80
                || (first == 0x2001 && segments[1] == 0x0db8))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicUsize;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    struct Reply {
        status: u16,
        headers: Vec<(&'static str, String)>,
        body: Vec<u8>,
    }

    impl Reply {
        fn ok(content_type: &str, body: Vec<u8>) -> Self {
            Self {
                status: 200,
                headers: vec![("Content-Type", content_type.into())],
                body,
            }
        }
        fn redirect(location: &str) -> Self {
            Self {
                status: 302,
                headers: vec![("Location", location.into())],
                body: Vec::new(),
            }
        }
    }

    struct Mock {
        addr: SocketAddr,
        hits: Arc<Mutex<Vec<String>>>,
        peak: Arc<AtomicUsize>,
    }

    impl Mock {
        fn hits(&self) -> Vec<String> {
            self.hits.lock().unwrap().clone()
        }
        fn url(&self, path: &str) -> String {
            format!("http://127.0.0.1:{}{path}", self.addr.port())
        }
    }

    async fn serve(
        delay: Duration,
        handler: impl Fn(&str) -> Reply + Send + Sync + 'static,
    ) -> Mock {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let hits = Arc::new(Mutex::new(Vec::new()));
        let peak = Arc::new(AtomicUsize::new(0));
        let live = Arc::new(AtomicUsize::new(0));
        let handler = Arc::new(handler);
        let (hits_task, peak_task) = (Arc::clone(&hits), Arc::clone(&peak));
        tokio::spawn(async move {
            loop {
                let Ok((mut stream, _)) = listener.accept().await else {
                    return;
                };
                let (handler, hits, peak, live) = (
                    Arc::clone(&handler),
                    Arc::clone(&hits_task),
                    Arc::clone(&peak_task),
                    Arc::clone(&live),
                );
                tokio::spawn(async move {
                    let mut request = Vec::new();
                    let mut chunk = [0u8; 1024];
                    while !request.windows(4).any(|window| window == b"\r\n\r\n") {
                        match stream.read(&mut chunk).await {
                            Ok(0) | Err(_) => return,
                            Ok(n) => request.extend_from_slice(&chunk[..n]),
                        }
                    }
                    let text = String::from_utf8_lossy(&request).to_string();
                    let path = text
                        .lines()
                        .next()
                        .and_then(|line| line.split(' ').nth(1))
                        .unwrap_or("/")
                        .to_string();
                    hits.lock().unwrap().push(path.clone());
                    let now = live.fetch_add(1, Ordering::SeqCst) + 1;
                    peak.fetch_max(now, Ordering::SeqCst);
                    tokio::time::sleep(delay).await;
                    let reply = handler(&path);
                    let mut head = format!("HTTP/1.1 {} X\r\nConnection: close\r\n", reply.status);
                    for (name, value) in &reply.headers {
                        let _ = write!(head, "{name}: {value}\r\n");
                    }
                    let _ = write!(head, "\r\n");
                    let _ = stream.write_all(head.as_bytes()).await;
                    let _ = stream.write_all(&reply.body).await;
                    let _ = stream.shutdown().await;
                    live.fetch_sub(1, Ordering::SeqCst);
                });
            }
        });
        Mock { addr, hits, peak }
    }

    fn png(width: u32, height: u32) -> Vec<u8> {
        let image = image::RgbImage::from_fn(width, height, |x, y| {
            image::Rgb([(x % 251) as u8, (y % 241) as u8, 90])
        });
        let mut out = Vec::new();
        image
            .write_to(&mut std::io::Cursor::new(&mut out), image::ImageFormat::Png)
            .unwrap();
        out
    }

    fn jpeg() -> Vec<u8> {
        let image = image::RgbImage::from_pixel(20, 10, image::Rgb([200, 30, 30]));
        let mut out = Vec::new();
        image
            .write_to(
                &mut std::io::Cursor::new(&mut out),
                image::ImageFormat::Jpeg,
            )
            .unwrap();
        out
    }

    fn dir() -> PathBuf {
        std::env::temp_dir().join(format!("nexus-image-test-{}", uuid::Uuid::new_v4()))
    }

    fn test_fetcher() -> Arc<Fetcher> {
        Arc::new(Fetcher::build(&dir(), system_lookup(), true))
    }

    /// A fetcher whose DNS answers come from `table` (name to addresses).
    fn fetcher_with_dns(table: &[(&'static str, &'static str)]) -> Arc<Fetcher> {
        let table: Vec<_> = table.to_vec();
        let lookup: Lookup = Arc::new(move |host: String| {
            let answer: Vec<IpAddr> = table
                .iter()
                .filter(|(name, _)| *name == host)
                .map(|(_, ip)| ip.parse().unwrap())
                .collect();
            Box::pin(async move { Ok(answer) })
        });
        Arc::new(Fetcher::build(&dir(), lookup, true))
    }

    async fn settled(fetcher: &Arc<Fetcher>, url: &str) -> Status {
        for _ in 0..200 {
            let status = fetcher.request(url);
            if status != Status::Loading {
                return status;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
        panic!("image never settled");
    }

    fn error_text(result: Result<PathBuf>) -> String {
        format!("{:#}", result.expect_err("download should be refused"))
    }

    #[test]
    fn syntax_rules_hold_outside_tests() {
        assert!(validate_url("https://example.com/a.png").is_ok());
        assert!(validate_url("http://example.com/a.png").is_err());
        assert!(validate_url("http://127.0.0.1/a.png").is_err());
        assert!(validate_url("file:///etc/passwd").is_err());
        assert!(validate_url("data:image/png;base64,AAAA").is_err());
        assert!(validate_url("https://user:pw@example.com/a.png").is_err());
        assert!(validate_url("https://user@example.com/a.png").is_err());
        assert!(validate_url("https://example.com/{{state.x}}.png").is_err());
        assert!(validate_url("not a url").is_err());
        assert!(validate_url("https://example.com/a b.png").is_err());
        assert!(validate_url("https://example.com/a\n.png").is_err());
        assert!(validate_url(" https://example.com/a.png").is_err());
        assert!(validate_url("https://").is_err());
    }

    #[test]
    fn non_public_addresses_are_refused() {
        let refused = [
            "10.0.0.1",
            "172.16.5.4",
            "192.168.1.1",
            "127.0.0.1",
            "127.1.2.3",
            "169.254.169.254",
            "0.0.0.0",
            "224.0.0.1",
            "255.255.255.255",
            "100.64.0.1",
            "::1",
            "::",
            "fe80::1",
            "fc00::1",
            "fd12:3456::1",
            "ff02::1",
            "::ffff:127.0.0.1",
            "::ffff:10.0.0.1",
            "::ffff:192.168.0.1",
            "::ffff:169.254.169.254",
            "64:ff9b::7f00:1",
            "64:ff9b::a00:1",
        ];
        for text in refused {
            let ip: IpAddr = text.parse().unwrap();
            assert!(!public_address(ip), "{text} must be refused");
            assert!(vet_addresses(&[ip], false).is_err(), "{text}");
        }
        for text in ["8.8.8.8", "93.184.216.34", "2606:4700:4700::1111"] {
            let ip: IpAddr = text.parse().unwrap();
            assert!(public_address(ip), "{text} is public");
        }
        // One bad record poisons the whole answer.
        let mixed: Vec<IpAddr> = vec!["8.8.8.8".parse().unwrap(), "10.0.0.1".parse().unwrap()];
        assert!(vet_addresses(&mixed, false).is_err());
        assert!(vet_addresses(&[], false).is_err());
        // The loopback exemption exists only when a test asks for it.
        let loopback: IpAddr = "127.0.0.1".parse().unwrap();
        assert!(vet_addresses(&[loopback], true).is_ok());
        for embedded in [
            "::7f00:1",
            "::a00:1",
            "::c0a8:101",
            "64:ff9b::a00:1",
            "::ffff:169.254.169.254",
        ] {
            let address: IpAddr = embedded.parse().unwrap();
            assert!(!public_address(address), "{embedded} embeds a private IPv4");
        }
        assert!(
            public_address("::808:808".parse().unwrap()),
            "::8.8.8.8 is public"
        );
        assert!(vet_addresses(&["10.0.0.1".parse().unwrap()], true).is_err());
    }

    #[test]
    fn obfuscated_ipv4_spellings_normalise_to_literals() {
        for text in [
            "https://2130706433/a.png",
            "https://0x7f.1/a.png",
            "https://0177.0.0.1/a.png",
            "https://127.1/a.png",
        ] {
            let url = validate_url(text).unwrap();
            let ip = literal_ip(&url).unwrap_or_else(|| panic!("{text} is a literal"));
            assert!(!public_address(ip), "{text}");
        }
        let url = validate_url("https://[::ffff:7f00:1]/a.png").unwrap();
        assert!(!public_address(literal_ip(&url).unwrap()));
        assert!(literal_ip(&validate_url("https://example.com/").unwrap()).is_none());
    }

    #[tokio::test]
    async fn literal_private_hosts_are_refused_without_connecting() {
        // A strict fetcher (no loopback exemption) must refuse even a live local server.
        let mock = serve(Duration::ZERO, |_| Reply::ok("image/png", png(8, 8))).await;
        let fetcher = Fetcher::new(dir());
        for url in [
            format!("https://127.0.0.1:{}/a.png", mock.addr.port()),
            "https://10.1.2.3/a.png".into(),
            "https://[::1]/a.png".into(),
            "https://[::ffff:127.0.0.1]/a.png".into(),
            "https://169.254.169.254/latest/meta-data".into(),
            "https://[fd00::1]/a.png".into(),
            "https://0.0.0.0/a.png".into(),
        ] {
            let text = error_text(fetcher.download(&url).await);
            assert!(text.contains("non-public"), "{url}: {text}");
        }
        // And plain http is refused before any lookup.
        let text = error_text(fetcher.download(&mock.url("/a.png")).await);
        assert!(text.contains("https"), "{text}");
        assert!(mock.hits().is_empty(), "nothing may reach the server");
    }

    #[tokio::test]
    async fn hostnames_resolving_to_private_addresses_are_refused_at_connect() {
        // Real system resolver: `localhost` is local without touching the network.
        let mock = serve(Duration::ZERO, |_| Reply::ok("image/png", png(8, 8))).await;
        let fetcher = Fetcher::new(dir());
        let text = error_text(
            fetcher
                .download(&format!("https://localhost:{}/a.png", mock.addr.port()))
                .await,
        );
        assert!(
            text.contains("non-public") || text.contains("error"),
            "{text}"
        );
        assert!(mock.hits().is_empty());

        // Fake DNS: each private flavour behind an ordinary-looking name.
        let fetcher = fetcher_with_dns(&[
            ("lan.example", "192.168.1.20"),
            ("meta.example", "169.254.169.254"),
            ("v6.example", "fd00::5"),
            ("mapped.example", "::ffff:10.0.0.9"),
            ("mixed.example", "8.8.8.8"),
            ("mixed.example", "10.0.0.1"),
        ]);
        for name in [
            "lan.example",
            "meta.example",
            "v6.example",
            "mapped.example",
            "mixed.example",
        ] {
            let url = format!("http://{name}:{}/a.png", mock.addr.port());
            let text = error_text(fetcher.download(&url).await);
            assert!(text.contains("non-public"), "{name}: {text}");
        }
        assert!(mock.hits().is_empty(), "no private address was dialled");
    }

    #[tokio::test]
    async fn resolver_returns_only_vetted_addresses_for_every_lookup() {
        // A rebinding server answers differently on each lookup. The resolver
        // vets every answer it hands out, so a later private answer is refused.
        let answers = Arc::new(Mutex::new(vec!["10.0.0.5", "8.8.8.8"]));
        let lookup: Lookup = {
            let answers = Arc::clone(&answers);
            Arc::new(move |_| {
                let next = answers.lock().unwrap().pop().unwrap();
                Box::pin(async move { Ok(vec![next.parse().unwrap()]) })
            })
        };
        let resolver = VettedResolver {
            lookup,
            allow_loopback: false,
        };
        let name = || "rebind.example".parse::<Name>().unwrap();
        let first: Vec<_> = resolver.resolve(name()).await.unwrap().collect();
        assert_eq!(first, vec![SocketAddr::new("8.8.8.8".parse().unwrap(), 0)]);
        assert!(resolver.resolve(name()).await.is_err());
    }

    #[tokio::test]
    async fn dns_pinned_connection_uses_the_vetted_answer() {
        // The name only resolves (via the injected lookup) to the mock server,
        // so a successful fetch proves the client dialled the vetted address.
        let mock = serve(Duration::ZERO, |_| Reply::ok("image/png", png(40, 30))).await;
        let fetcher = fetcher_with_dns(&[("cdn.example", "127.0.0.1")]);
        let url = format!("http://cdn.example:{}/a.png", mock.addr.port());
        let path = fetcher.download(&url).await.unwrap();
        assert!(path.is_file());
        assert_eq!(mock.hits(), vec!["/a.png"]);
    }

    #[tokio::test]
    async fn redirects_are_revetted_on_every_hop() {
        let mock = serve(Duration::ZERO, |path| match path {
            "/to-private-ip" => Reply::redirect("https://10.0.0.1/a.png"),
            "/to-loopback-v6" => Reply::redirect("https://[::1]/a.png"),
            "/to-mapped" => Reply::redirect("https://[::ffff:127.0.0.1]/a.png"),
            "/to-metadata" => Reply::redirect("https://169.254.169.254/x"),
            "/to-private-name" => Reply::redirect("http://evil.example:1/a.png"),
            "/to-file" => Reply::redirect("file:///etc/passwd"),
            "/to-creds" => Reply::redirect("https://u:p@example.com/a.png"),
            "/relative" => Reply::redirect("/a.png"),
            "/loop" => Reply::redirect("/loop"),
            "/a.png" => Reply::ok("image/png", png(16, 16)),
            _ => Reply::ok("text/plain", Vec::new()),
        })
        .await;
        let fetcher = fetcher_with_dns(&[("evil.example", "192.168.5.5")]);
        // A fetcher that allows plain http in tests but not the private targets.
        for hop in [
            "/to-private-ip",
            "/to-loopback-v6",
            "/to-mapped",
            "/to-metadata",
            "/to-file",
            "/to-creds",
        ] {
            let text = error_text(fetcher.download(&mock.url(hop)).await);
            assert!(
                text.contains("non-public")
                    || text.contains("https")
                    || text.contains("credentials"),
                "{hop}: {text}"
            );
        }
        let text = error_text(fetcher.download(&mock.url("/to-private-name")).await);
        assert!(text.contains("non-public"), "{text}");
        // Only the redirecting requests themselves reached the server.
        assert!(!mock.hits().contains(&"/a.png".to_string()));
        // A relative redirect to a public resource is followed.
        assert!(fetcher.download(&mock.url("/relative")).await.is_ok());
        // Redirect loops stop after three hops.
        let before = mock.hits().len();
        let text = error_text(fetcher.download(&mock.url("/loop")).await);
        assert!(text.contains("too many"), "{text}");
        assert_eq!(mock.hits().len() - before, MAX_REDIRECTS + 1);
    }

    #[tokio::test]
    async fn fetch_decodes_downscales_and_reuses_the_cache() {
        // The big picture is encoded once, not per request, and this fetcher
        // gets a generous deadline: a debug build decoding and resizing it on
        // a machine busy with the whole suite can overrun the real 6 s.
        let big = png(3200, 800);
        let mock = serve(Duration::ZERO, move |path| match path {
            "/big.png" => Reply::ok("image/png; charset=binary", big.clone()),
            "/photo.jpg" => Reply::ok("IMAGE/JPEG", jpeg()),
            _ => Reply::ok("text/plain", Vec::new()),
        })
        .await;
        let root = dir();
        let patient = || {
            let mut fetcher = Fetcher::build(&root, system_lookup(), true);
            fetcher.timeout = Duration::from_secs(120);
            Arc::new(fetcher)
        };
        let fetcher = patient();
        let path = fetcher.download(&mock.url("/big.png")).await.unwrap();
        assert!(path.starts_with(root.join("images")));
        let decoded = image::open(&path).unwrap();
        assert_eq!(decoded.width(), 1600);
        assert_eq!(decoded.height(), 400);
        let jpeg_path = fetcher.download(&mock.url("/photo.jpg")).await.unwrap();
        assert_eq!(image::open(jpeg_path).unwrap().width(), 20);

        // Through `request`: first Loading, then Ready; a second session reuses the file.
        let url = mock.url("/big.png");
        assert_eq!(settled(&fetcher, &url).await, Status::Ready(path.clone()));
        let hits = mock.hits().len();
        let second = patient();
        assert_eq!(second.request(&url), Status::Ready(path));
        assert_eq!(mock.hits().len(), hits, "cache hit must not refetch");
    }

    #[tokio::test]
    async fn bad_payloads_are_rejected() {
        let mock = serve(Duration::ZERO, |path| match path {
            "/html" => Reply::ok("text/html", b"<html></html>".to_vec()),
            "/svg" => Reply::ok("image/svg+xml", b"<svg/>".to_vec()),
            "/none" => Reply {
                status: 200,
                headers: Vec::new(),
                body: png(8, 8),
            },
            "/garbage" => Reply::ok("image/png", b"definitely not a png".to_vec()),
            "/lies" => Reply::ok("image/png", b"GIF89a....".to_vec()),
            "/declared" => Reply {
                status: 200,
                headers: vec![
                    ("Content-Type", "image/png".into()),
                    ("Content-Length", (MAX_BYTES + 1).to_string()),
                ],
                body: Vec::new(),
            },
            "/streamed" => Reply::ok("image/png", vec![0u8; MAX_BYTES + 4096]),
            "/missing" => Reply {
                status: 404,
                headers: vec![("Content-Type", "image/png".into())],
                body: png(8, 8),
            },
            _ => Reply::ok("text/plain", Vec::new()),
        })
        .await;
        let fetcher = test_fetcher();
        for (path, needle) in [
            ("/html", "content type"),
            ("/svg", "content type"),
            ("/none", "content type"),
            ("/garbage", ""),
            ("/lies", ""),
            ("/declared", "too large"),
            ("/streamed", "too large"),
            ("/missing", "404"),
        ] {
            let text = error_text(fetcher.download(&mock.url(path)).await);
            assert!(text.contains(needle), "{path}: {text}");
        }
        // Nothing unverified was left in the cache.
        let leftovers = std::fs::read_dir(&fetcher.directory)
            .map(|entries| entries.count())
            .unwrap_or(0);
        assert_eq!(leftovers, 0);
    }

    #[test]
    fn icon_links_keep_raster_icons_largest_first() {
        let html = r#"<html><head>
            <link rel="stylesheet" href="/a.css">
            <link rel="icon" type="image/svg+xml" href="/icon.svg">
            <LINK REL='shortcut icon' HREF='/static/icon.png?v=2&amp;x=1'>
            <link rel=mask-icon href=/mask.png>
            <link href="https://cdn.example/touch.png" rel="apple-touch-icon" sizes="180x180"/>
            <link rel="icon" href="">
        </head><body><link rel="icon" href="/late.png"></body></html>"#;
        assert_eq!(
            icon_links(html),
            ["https://cdn.example/touch.png", "/static/icon.png?v=2&x=1"]
        );
        let sized = r#"<link rel=icon href=/16.png sizes=16x16><link rel=icon href=/96.png sizes="96x96"><link rel=apple-touch-icon href=/touch.png>"#;
        assert_eq!(icon_links(sized), ["/touch.png", "/96.png", "/16.png"]);
        assert!(icon_links("<link rel=icon").is_empty());
    }

    fn ico() -> Vec<u8> {
        let image = image::RgbaImage::from_pixel(32, 32, image::Rgba([10, 120, 240, 255]));
        let mut out = Vec::new();
        image
            .write_to(&mut std::io::Cursor::new(&mut out), image::ImageFormat::Ico)
            .unwrap();
        out
    }

    #[tokio::test]
    async fn favicons_come_from_favicon_ico_or_the_pages_icon_links() {
        let direct = serve(Duration::ZERO, |path| match path {
            "/favicon.ico" => Reply::ok("image/vnd.microsoft.icon", ico()),
            _ => Reply::ok("text/plain", Vec::new()),
        })
        .await;
        let fetcher = test_fetcher();
        let origin = direct.url("");
        let path = fetcher.download_favicon(&origin).await.unwrap();
        assert_eq!(image::open(&path).unwrap().width(), 32);
        assert_eq!(
            direct.hits(),
            ["/", "/favicon.ico"],
            "no page icons, then the ico"
        );

        let linked = serve(Duration::ZERO, |path| match path {
            "/" => Reply::ok(
                "text/html; charset=utf-8",
                br#"<head><link rel="icon" href="/missing.png"><link rel="icon" href="/static/i.png?v=2&amp;x=1"></head>"#.to_vec(),
            ),
            "/static/i.png?v=2&x=1" => Reply::ok("application/octet-stream", png(200, 100)),
            _ => Reply {
                status: 404,
                headers: Vec::new(),
                body: Vec::new(),
            },
        })
        .await;
        let origin = linked.url("");
        let generation = fetcher.generation();
        assert!(matches!(
            settled_icon(&fetcher, &origin).await,
            Status::Ready(_)
        ));
        assert_eq!(fetcher.generation(), generation, "icons never re-layout");
        let Status::Ready(path) = fetcher.favicon(&origin) else {
            unreachable!()
        };
        let icon = image::open(&path).unwrap();
        assert_eq!(
            (icon.width(), icon.height()),
            (128, 64),
            "icons are kept small"
        );
        assert_eq!(
            linked.hits(),
            ["/", "/missing.png", "/static/i.png?v=2&x=1"],
            "page icons come before the usually tiny favicon.ico"
        );
        assert_ne!(
            Some(path),
            fetcher.cached(&origin),
            "icons and images are cached apart"
        );

        let bare = serve(Duration::ZERO, |path| match path {
            "/" => Reply::ok("text/html", b"<head></head>".to_vec()),
            _ => Reply::ok("image/svg+xml", b"<svg/>".to_vec()),
        })
        .await;
        assert_eq!(settled_icon(&fetcher, &bare.url("")).await, Status::Failed);
        let text = error_text(fetcher.download_favicon(&bare.url("")).await);
        assert!(text.contains("no site icon"), "{text}");
    }

    async fn settled_icon(fetcher: &Arc<Fetcher>, origin: &str) -> Status {
        for _ in 0..200 {
            let status = fetcher.favicon(origin);
            if status != Status::Loading {
                return status;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
        panic!("icon never settled");
    }

    #[tokio::test]
    async fn slow_responses_time_out() {
        let mock = serve(Duration::from_secs(30), |_| {
            Reply::ok("image/png", png(8, 8))
        })
        .await;
        let mut fetcher = Fetcher::build(&dir(), system_lookup(), true);
        fetcher.timeout = Duration::from_millis(300);
        let text = error_text(fetcher.download(&mock.url("/slow.png")).await);
        assert!(
            text.contains("elapsed") || text.contains("deadline"),
            "{text}"
        );
    }

    #[tokio::test]
    async fn failures_are_remembered_then_retried_after_ten_minutes() {
        let mock = serve(Duration::ZERO, |_| Reply {
            status: 500,
            headers: Vec::new(),
            body: Vec::new(),
        })
        .await;
        let fetcher = test_fetcher();
        let url = mock.url("/broken.png");
        assert_eq!(settled(&fetcher, &url).await, Status::Failed);
        assert_eq!(mock.hits().len(), 1);
        for _ in 0..5 {
            assert_eq!(fetcher.request(&url), Status::Failed);
        }
        assert_eq!(mock.hits().len(), 1, "negative cache stops the retries");
        // Age the failure past the TTL: the next request tries again.
        fetcher.entries.lock().unwrap().get_mut(&url).unwrap().since = Instant::now()
            .checked_sub(NEGATIVE_TTL + Duration::from_secs(1))
            .unwrap();
        assert_eq!(fetcher.request(&url), Status::Loading);
        assert_eq!(settled(&fetcher, &url).await, Status::Failed);
        assert_eq!(mock.hits().len(), 2);
        // A URL that fails validation is a cached failure without any network.
        assert_eq!(fetcher.request("ftp://example.com/a.png"), Status::Failed);
    }

    #[tokio::test]
    async fn at_most_four_downloads_run_at_once() {
        let mock = serve(Duration::from_millis(150), |_| {
            Reply::ok("image/png", png(8, 8))
        })
        .await;
        let fetcher = test_fetcher();
        let urls: Vec<_> = (0..10).map(|n| mock.url(&format!("/{n}.png"))).collect();
        let jobs = urls.iter().map(|url| fetcher.download(url));
        let results = futures_util::future::join_all(jobs).await;
        assert!(results.iter().all(Result::is_ok));
        assert_eq!(mock.hits().len(), 10);
        let peak = mock.peak.load(Ordering::SeqCst);
        assert!(peak <= IN_FLIGHT, "peak concurrency {peak}");
        assert!(peak >= 2, "downloads should overlap, peak {peak}");
    }

    #[tokio::test]
    async fn disabled_fetcher_fetches_nothing_and_cancels_pending() {
        let mock = serve(Duration::from_millis(300), |_| {
            Reply::ok("image/png", png(8, 8))
        })
        .await;
        let fetcher = test_fetcher();
        fetcher.set_enabled(false);
        assert_eq!(fetcher.request(&mock.url("/a.png")), Status::Off);
        assert!(error_text(fetcher.download(&mock.url("/a.png")).await).contains("disabled"));
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert!(mock.hits().is_empty());

        // Turning images off mid-download cancels it without a failure entry.
        fetcher.set_enabled(true);
        let url = mock.url("/b.png");
        assert_eq!(fetcher.request(&url), Status::Loading);
        tokio::time::sleep(Duration::from_millis(50)).await;
        fetcher.set_enabled(false);
        assert_eq!(fetcher.request(&url), Status::Off);
        tokio::time::sleep(Duration::from_millis(500)).await;
        fetcher.set_enabled(true);
        assert!(!fetcher.entries.lock().unwrap().contains_key(&url));
        assert!(matches!(settled(&fetcher, &url).await, Status::Ready(_)));
    }

    #[tokio::test]
    async fn completion_wakes_an_idle_event_loop() {
        let mock = serve(Duration::ZERO, |_| Reply::ok("image/png", png(8, 8))).await;
        let fetcher = test_fetcher();
        let url = mock.url("/a.png");
        assert_eq!(fetcher.request(&url), Status::Loading);
        tokio::time::timeout(Duration::from_secs(3), fetcher.changed())
            .await
            .expect("completion must notify");
        assert!(matches!(fetcher.request(&url), Status::Ready(_)));
        assert!(fetcher.generation() >= 1);
    }

    #[test]
    fn generation_advances_on_preference_changes() {
        let fetcher = Fetcher::new(dir());
        let start = fetcher.generation();
        fetcher.set_enabled(true);
        assert_eq!(fetcher.generation(), start, "no change, no invalidation");
        fetcher.set_enabled(false);
        assert_eq!(fetcher.generation(), start + 1);
    }

    #[test]
    fn cache_names_are_hex_digests() {
        let fetcher = Fetcher::new(dir());
        let name = fetcher.path("https://example.com/a.png");
        let stem = name.file_stem().unwrap().to_str().unwrap();
        assert_eq!(stem.len(), 64);
        assert!(stem.chars().all(|c| c.is_ascii_hexdigit()));
        assert_eq!(name.extension().unwrap(), "png");
        assert_ne!(name, fetcher.path("https://example.com/b.png"));
    }
}
