//! Connector secrets: bearer tokens, `keychain:<name>` environment values
//! and header values. They live in the OS keychain (service `nexus-chat`,
//! account `connector:<name>:<kind>`) through the `keyring` crate. When no
//! keychain is usable they fall back to the connector's own table in
//! `config.toml` (written 0600) and the user is warned once.
//!
//! The store sits behind a trait so every test uses an in-memory one and
//! nothing here ever touches the developer's real keychain.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use serde_json::Value;
use tokio::sync::mpsc::UnboundedSender;

/// The keychain service all connector secrets share.
pub const SERVICE: &str = "nexus-chat";

/// Prefix marking a config value as "look this up in the keychain".
pub const KEYCHAIN_PREFIX: &str = "keychain:";

/// Why a store call failed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SecretError {
    /// No usable keychain on this machine/session (headless Linux, locked,
    /// permission denied). The caller falls back to the config file.
    Unavailable(String),
    /// Anything else.
    Other(String),
}

impl std::fmt::Display for SecretError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Unavailable(why) => write!(f, "no keychain available ({why})"),
            Self::Other(why) => write!(f, "{why}"),
        }
    }
}

/// Mask known credentials before server-controlled text reaches any surface.
pub(super) fn redact(text: &str, secrets: &[String]) -> String {
    let mut text = text.to_string();
    for secret in secrets.iter().filter(|secret| !secret.is_empty()) {
        text = text.replace(secret.as_str(), "…");
    }
    text
}

/// Mask credentials in JSON strings and keys, including RPC errors and tool definitions.
pub(super) fn redact_value(value: Value, secrets: &[String]) -> Value {
    match value {
        Value::String(text) => Value::String(redact(&text, secrets)),
        Value::Array(items) => Value::Array(
            items
                .into_iter()
                .map(|item| redact_value(item, secrets))
                .collect(),
        ),
        Value::Object(object) => Value::Object(
            object
                .into_iter()
                .map(|(key, value)| (redact(&key, secrets), redact_value(value, secrets)))
                .collect(),
        ),
        other => other,
    }
}

/// A place secrets can be kept, addressed by account name.
pub trait SecretStore: Send + Sync {
    /// The secret, or `None` when there is none.
    fn get(&self, account: &str) -> Result<Option<String>, SecretError>;
    /// Create or replace.
    fn set(&self, account: &str, secret: &str) -> Result<(), SecretError>;
    /// Remove; a missing entry is not an error.
    fn delete(&self, account: &str) -> Result<(), SecretError>;
}

/// The real thing: the OS keychain via `keyring`.
#[derive(Debug, Default, Clone, Copy)]
pub struct KeyringStore;

impl KeyringStore {
    fn entry(account: &str) -> Result<keyring::Entry, SecretError> {
        keyring::Entry::new(SERVICE, account).map_err(map_error)
    }
}

fn map_error(error: keyring::Error) -> SecretError {
    match error {
        keyring::Error::NoStorageAccess(why) | keyring::Error::PlatformFailure(why) => {
            SecretError::Unavailable(why.to_string())
        }
        keyring::Error::NoDefaultStore => SecretError::Unavailable("no credential store".into()),
        other => SecretError::Other(other.to_string()),
    }
}

impl SecretStore for KeyringStore {
    fn get(&self, account: &str) -> Result<Option<String>, SecretError> {
        match Self::entry(account)?.get_password() {
            Ok(secret) => Ok(Some(secret)),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(error) => Err(map_error(error)),
        }
    }

    fn set(&self, account: &str, secret: &str) -> Result<(), SecretError> {
        Self::entry(account)?
            .set_password(secret)
            .map_err(map_error)
    }

    fn delete(&self, account: &str) -> Result<(), SecretError> {
        match Self::entry(account)?.delete_credential() {
            Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
            Err(error) => Err(map_error(error)),
        }
    }
}

/// An in-memory store, for tests. `unavailable` makes it behave like a
/// machine without a keychain.
#[derive(Debug, Default)]
pub struct MemoryStore {
    entries: Mutex<HashMap<String, String>>,
    unavailable: AtomicBool,
}

impl MemoryStore {
    /// A store that always reports "no keychain".
    #[must_use]
    pub fn unavailable() -> Self {
        Self {
            entries: Mutex::default(),
            unavailable: AtomicBool::new(true),
        }
    }

    /// How many secrets it holds.
    #[must_use]
    pub fn len(&self) -> usize {
        self.entries.lock().expect("memory store").len()
    }

    /// Whether it holds none.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// The accounts it holds, sorted.
    #[must_use]
    pub fn accounts(&self) -> Vec<String> {
        let mut accounts: Vec<String> = self
            .entries
            .lock()
            .expect("memory store")
            .keys()
            .cloned()
            .collect();
        accounts.sort();
        accounts
    }

    fn check(&self) -> Result<(), SecretError> {
        if self.unavailable.load(Ordering::Relaxed) {
            Err(SecretError::Unavailable("memory store disabled".into()))
        } else {
            Ok(())
        }
    }
}

impl SecretStore for MemoryStore {
    fn get(&self, account: &str) -> Result<Option<String>, SecretError> {
        self.check()?;
        Ok(self
            .entries
            .lock()
            .expect("memory store")
            .get(account)
            .cloned())
    }

    fn set(&self, account: &str, secret: &str) -> Result<(), SecretError> {
        self.check()?;
        self.entries
            .lock()
            .expect("memory store")
            .insert(account.to_string(), secret.to_string());
        Ok(())
    }

    fn delete(&self, account: &str) -> Result<(), SecretError> {
        self.check()?;
        self.entries.lock().expect("memory store").remove(account);
        Ok(())
    }
}

/// Where a stored secret ended up.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Placed {
    Keychain,
    /// The connector's table in `config.toml`.
    ConfigFile,
}

/// The keychain with the config-file fallback in front of callers.
pub struct Vault {
    store: Arc<dyn SecretStore>,
    config_path: Option<PathBuf>,
    warned: AtomicBool,
    notes: Option<UnboundedSender<String>>,
}

impl Vault {
    /// A vault over `store`. `config_path` enables the fallback; `notes`
    /// receives the one-time warning when it is used.
    #[must_use]
    pub fn new(
        store: Arc<dyn SecretStore>,
        config_path: Option<PathBuf>,
        notes: Option<UnboundedSender<String>>,
    ) -> Self {
        Self {
            store,
            config_path,
            warned: AtomicBool::new(false),
            notes,
        }
    }

    /// The real keychain, falling back to `config_path`.
    #[must_use]
    pub fn system(config_path: Option<PathBuf>, notes: Option<UnboundedSender<String>>) -> Self {
        Self::new(Arc::new(KeyringStore), config_path, notes)
    }

    /// A vault that keeps everything in memory (no persistence at all).
    #[must_use]
    pub fn in_memory() -> Self {
        Self::new(Arc::new(MemoryStore::default()), None, None)
    }

    /// `connector:<name>:<kind>`.
    #[must_use]
    pub fn account(connector: &str, kind: &str) -> String {
        format!("connector:{connector}:{kind}")
    }

    /// Look a secret up: keychain first, then the config fallback.
    #[must_use]
    pub fn get(&self, connector: &str, kind: &str) -> Option<String> {
        if let Ok(Some(secret)) = self.store.get(&Self::account(connector, kind)) {
            return Some(secret);
        }
        let path = self.config_path.as_ref()?;
        let loaded = super::config::load_at(path).ok()?;
        let secret = loaded
            .connectors
            .get(connector)?
            .secrets
            .get(kind)
            .cloned()?;
        self.warn_fallback();
        Some(secret)
    }

    fn warn_fallback(&self) {
        if !self.warned.swap(true, Ordering::Relaxed) {
            let warning =
                "no keychain available · connector secrets are stored in config.toml (owner-only)";
            if let Some(notes) = &self.notes {
                let _ = notes.send(warning.to_string());
            } else {
                eprintln!("{warning}");
            }
        }
    }

    /// Store a secret. The keychain is preferred; when it is unavailable the
    /// connector's config table holds it and a one-time warning is sent.
    ///
    /// # Errors
    /// Neither the keychain nor the config file could take it.
    pub fn set(&self, connector: &str, kind: &str, secret: &str) -> Result<Placed, String> {
        match self.store.set(&Self::account(connector, kind), secret) {
            Ok(()) => {
                // A stale fallback copy must not outlive a good keychain write.
                self.clear_fallback(connector, kind);
                Ok(Placed::Keychain)
            }
            Err(SecretError::Unavailable(_)) if self.config_path.is_some() => {
                let path = self.config_path.as_ref().expect("checked");
                super::config::set_secret_at(path, connector, kind, Some(secret))
                    .map_err(|error| format!("could not store the secret: {error:#}"))?;
                self.warn_fallback();
                Ok(Placed::ConfigFile)
            }
            Err(error) => Err(redact(
                &format!("could not store the secret: {error}"),
                &[secret.to_string()],
            )),
        }
    }

    /// Remove a secret from both places.
    pub fn delete(&self, connector: &str, kind: &str) {
        let _ = self.store.delete(&Self::account(connector, kind));
        self.clear_fallback(connector, kind);
    }

    fn clear_fallback(&self, connector: &str, kind: &str) {
        if let Some(path) = &self.config_path {
            let _ = super::config::set_secret_at(path, connector, kind, None);
        }
    }

    /// Resolve a config value: `keychain:<kind>` is looked up, anything else
    /// is returned as written.
    #[must_use]
    pub fn resolve(&self, connector: &str, value: &str) -> Option<String> {
        match value.strip_prefix(KEYCHAIN_PREFIX) {
            Some(kind) => self.get(connector, kind),
            None => Some(value.to_string()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accounts_follow_the_documented_shape() {
        assert_eq!(
            Vault::account("linear", "bearer"),
            "connector:linear:bearer"
        );
    }

    #[test]
    fn the_keychain_is_preferred_and_resolve_handles_prefixes() {
        let store = Arc::new(MemoryStore::default());
        let vault = Vault::new(store.clone(), None, None);
        assert_eq!(vault.set("a", "bearer", "tok").unwrap(), Placed::Keychain);
        assert_eq!(store.accounts(), ["connector:a:bearer"]);
        assert_eq!(vault.get("a", "bearer").as_deref(), Some("tok"));
        assert_eq!(
            vault.resolve("a", "keychain:bearer").as_deref(),
            Some("tok")
        );
        assert_eq!(vault.resolve("a", "plain").as_deref(), Some("plain"));
        assert_eq!(vault.resolve("a", "keychain:missing"), None);
        vault.delete("a", "bearer");
        assert!(store.is_empty());
        assert_eq!(vault.get("a", "bearer"), None);
    }

    #[test]
    fn reading_previously_saved_fallback_secrets_warns_once_without_echoing_them() {
        let dir = std::env::temp_dir().join(format!("nexus-vault-read-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("config.toml");
        super::super::config::add_at(
            &path,
            "a",
            &super::super::ConnectorConfig::remote("https://a.example/mcp"),
        )
        .unwrap();
        super::super::config::set_secret_at(&path, "a", "bearer", Some("fake-saved-secret"))
            .unwrap();
        let (sender, mut receiver) = tokio::sync::mpsc::unbounded_channel();
        let vault = Vault::new(
            Arc::new(MemoryStore::unavailable()),
            Some(path),
            Some(sender),
        );
        for _ in 0..2 {
            assert_eq!(
                vault.get("a", "bearer").as_deref(),
                Some("fake-saved-secret")
            );
        }
        let warning = receiver.try_recv().unwrap();
        assert!(warning.contains("owner-only"));
        assert!(!warning.contains("fake-saved-secret"));
        assert!(receiver.try_recv().is_err());
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn without_a_keychain_secrets_fall_back_to_config_with_one_warning() {
        let dir = std::env::temp_dir().join(format!("nexus-vault-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("config.toml");
        std::fs::write(
            &path,
            "[connectors.a]\nurl = \"https://a.example/mcp\"\n[connectors.b]\nurl = \"https://b.example/mcp\"\n",
        )
        .unwrap();
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let vault = Vault::new(
            Arc::new(MemoryStore::unavailable()),
            Some(path.clone()),
            Some(tx),
        );
        assert_eq!(
            vault.set("a", "bearer", "tok-a").unwrap(),
            Placed::ConfigFile
        );
        assert_eq!(
            vault.set("b", "bearer", "tok-b").unwrap(),
            Placed::ConfigFile
        );
        assert_eq!(vault.get("a", "bearer").as_deref(), Some("tok-a"));
        let warning = rx.try_recv().unwrap();
        assert!(warning.contains("config.toml"));
        assert!(!warning.contains("tok-"), "{warning}");
        assert!(rx.try_recv().is_err(), "the warning is sent only once");
        vault.delete("a", "bearer");
        assert_eq!(vault.get("a", "bearer"), None);
        assert_eq!(vault.get("b", "bearer").as_deref(), Some("tok-b"));
        // No fallback path and no keychain: the error says so, without the secret.
        let bare = Vault::new(Arc::new(MemoryStore::unavailable()), None, None);
        let error = bare.set("a", "bearer", "tok-zzz").unwrap_err();
        assert!(!error.contains("tok-zzz"));
        std::fs::remove_dir_all(dir).unwrap();
    }
}
