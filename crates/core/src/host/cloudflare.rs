//! Minimal Cloudflare v4 control-plane calls for `nexus host --setup`.
//!
//! The workspace already carries an async `reqwest` client, so setup uses the
//! documented REST endpoints directly instead of adding a second Cloudflare
//! SDK and HTTP stack. The API token is read from `CF_API_TOKEN` and is never
//! included in returned errors or written to disk.

use std::path::PathBuf;

use anyhow::{Context as _, Result, bail};
use base64::Engine as _;
use serde::Deserialize;
use sha2::{Digest as _, Sha256};

const API: &str = "https://api.cloudflare.com/client/v4";

/// A Cloudflare account selectable during setup.
#[derive(Debug, Clone, Deserialize)]
pub struct Account {
    pub id: String,
    pub name: String,
}

/// A DNS zone selectable during setup.
#[derive(Debug, Clone, Deserialize)]
pub struct Zone {
    pub id: String,
    pub name: String,
}

/// Inputs for creating a named tunnel and its DNS route.
#[derive(Debug, Clone)]
pub struct SetupOptions {
    pub account_id: String,
    pub zone_id: String,
    pub hostname: String,
    pub tunnel_name: String,
    pub port: u16,
}

/// Files and identifiers produced by named-tunnel setup.
#[derive(Debug, Clone)]
pub struct SetupResult {
    pub tunnel_id: String,
    pub hostname: String,
    pub credentials_path: PathBuf,
    pub config_path: PathBuf,
}

#[derive(Debug, Deserialize)]
struct ApiResponse<T> {
    success: bool,
    result: Option<T>,
    #[serde(default)]
    errors: Vec<ApiError>,
}

#[derive(Debug, Deserialize)]
struct ApiError {
    #[serde(default)]
    message: String,
}

/// List accounts visible to `CF_API_TOKEN`.
pub async fn list_accounts() -> Result<Vec<Account>> {
    let client = client()?;
    let response = client.get(format!("{API}/accounts")).send().await?;
    parse_response(response).await
}

/// List DNS zones visible to `CF_API_TOKEN`.
pub async fn list_zones() -> Result<Vec<Zone>> {
    let client = client()?;
    let response = client
        .get(format!("{API}/zones?per_page=100"))
        .send()
        .await?;
    parse_response(response).await
}

/// Create a named tunnel, add a proxied CNAME, fetch its connector token, and
/// write the local credentials/config files used by `cloudflared`.
pub async fn provision_named_tunnel(options: &SetupOptions) -> Result<SetupResult> {
    if options.hostname.trim().is_empty() || options.tunnel_name.trim().is_empty() {
        bail!("tunnel hostname and name must not be empty");
    }
    let client = client()?;
    let secret = tunnel_secret();
    let create = client
        .post(format!("{API}/accounts/{}/cfd_tunnel", options.account_id))
        .json(&serde_json::json!({
            "name": options.tunnel_name,
            "tunnel_secret": secret,
            "config_src": "local",
        }))
        .send()
        .await?;
    let tunnel: TunnelResult = parse_response(create).await?;

    let dns = client
        .post(format!("{API}/zones/{}/dns_records", options.zone_id))
        .json(&serde_json::json!({
            "type": "CNAME",
            "name": options.hostname,
            "content": format!("{}.cfargotunnel.com", tunnel.id),
            "proxied": true,
        }))
        .send()
        .await?;
    let _: serde_json::Value = parse_response(dns).await?;

    // The token endpoint is useful for cloudflared's `tunnel run --token`
    // form, but the credentials JSON is the stable local artifact used by a
    // named config. Fetch it so setup verifies connector authorization too.
    let token_response = client
        .get(format!(
            "{API}/accounts/{}/cfd_tunnel/{}/token",
            options.account_id, tunnel.id
        ))
        .send()
        .await?;
    let _token: String = parse_response(token_response).await?;

    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .context("HOME is not set; cannot locate ~/.cloudflared")?;
    let dir = home.join(".cloudflared");
    std::fs::create_dir_all(&dir).with_context(|| format!("creating {}", dir.display()))?;
    let credentials_path = dir.join(format!("{}.json", tunnel.id));
    let credentials = serde_json::json!({
        "AccountTag": options.account_id,
        "TunnelSecret": secret,
        "TunnelID": tunnel.id,
    });
    write_private(&credentials_path, &serde_json::to_vec_pretty(&credentials)?)?;
    let config_path = dir.join(format!("nexus-{}.yml", tunnel.id));
    let config = format!(
        "tunnel: {}\ncredentials-file: {}\ningress:\n  - hostname: {}\n    service: http://127.0.0.1:{}\n  - service: http_status:404\n",
        tunnel.id,
        yaml_quote(&credentials_path.display().to_string()),
        options.hostname,
        options.port,
    );
    write_private(&config_path, config.as_bytes())?;
    Ok(SetupResult {
        tunnel_id: tunnel.id,
        hostname: options.hostname.clone(),
        credentials_path,
        config_path,
    })
}

#[derive(Debug, Deserialize)]
struct TunnelResult {
    id: String,
}

fn client() -> Result<reqwest::Client> {
    let token = std::env::var("CF_API_TOKEN").context("CF_API_TOKEN is not set")?;
    if token.trim().is_empty() {
        bail!("CF_API_TOKEN is empty");
    }
    let mut headers = reqwest::header::HeaderMap::new();
    headers.insert(
        reqwest::header::AUTHORIZATION,
        reqwest::header::HeaderValue::from_str(&format!("Bearer {token}"))?,
    );
    reqwest::Client::builder()
        .default_headers(headers)
        .user_agent("nexus-chat host setup")
        .build()
        .context("building Cloudflare API client")
}

async fn parse_response<T: for<'de> Deserialize<'de>>(response: reqwest::Response) -> Result<T> {
    let status = response.status();
    let body: ApiResponse<T> = response
        .json()
        .await
        .with_context(|| format!("parsing Cloudflare API response ({status})"))?;
    if !status.is_success() || !body.success {
        let detail = body
            .errors
            .first()
            .map_or("request failed", |error| error.message.as_str());
        bail!("Cloudflare API request failed ({status}): {detail}");
    }
    body.result
        .context("Cloudflare API response omitted result")
}

fn tunnel_secret() -> String {
    let mut bytes = [0u8; 32];
    let first = uuid::Uuid::new_v4().into_bytes();
    let second = uuid::Uuid::new_v4().into_bytes();
    bytes[..16].copy_from_slice(&first);
    bytes[16..].copy_from_slice(&second);
    // Hashing the UUID material makes the value independent of UUID version
    // bits while retaining 256 bits of entropy for Cloudflare's secret.
    let digest = Sha256::digest(bytes);
    base64::engine::general_purpose::STANDARD.encode(digest)
}

fn yaml_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "''"))
}

fn write_private(path: &std::path::Path, bytes: &[u8]) -> Result<()> {
    std::fs::write(path, bytes).with_context(|| format!("writing {}", path.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
            .with_context(|| format!("protecting {}", path.display()))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{tunnel_secret, yaml_quote};

    #[test]
    fn generated_tunnel_secret_is_base64_and_32_bytes() {
        let secret = tunnel_secret();
        let bytes = base64::Engine::decode(&base64::engine::general_purpose::STANDARD, secret)
            .expect("base64 secret");
        assert_eq!(bytes.len(), 32);
    }

    #[test]
    fn yaml_quote_escapes_single_quotes() {
        assert_eq!(yaml_quote("a'b"), "'a''b'");
    }
}
