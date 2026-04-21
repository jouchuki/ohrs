//! Credential storage backends.
//!
//! `FileBackend` — always available, writes `~/.config/ohrs/credentials.json`
//! with mode 0600, using an atomic rename.
//!
//! `KeyringBackend` — optional OS keyring (macOS Keychain, Linux Secret Service,
//! Windows Credential Manager). Degrades gracefully if the keyring is
//! unavailable by returning `AuthError::Keyring`.

use std::collections::HashMap;
use std::path::PathBuf;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use async_trait::async_trait;
use secrecy::{ExposeSecret, SecretString};
use serde::{Deserialize, Serialize};

use crate::error::AuthError;
use crate::providers::Provider;

/// A stored credential for a single provider+label combination.
#[derive(Clone, Serialize, Deserialize, Debug)]
pub struct Credential {
    pub provider: Provider,
    /// Optional label (e.g. "work", "personal"). `None` → default slot.
    pub label: Option<String>,
    /// The secret (API key or access token).
    #[serde(
        serialize_with = "serialize_secret",
        deserialize_with = "deserialize_secret"
    )]
    pub secret: SecretString,
    /// Refresh token for OAuth flows.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        serialize_with = "serialize_secret_opt",
        deserialize_with = "deserialize_secret_opt"
    )]
    pub refresh_token: Option<SecretString>,
    /// Unix-seconds expiry timestamp, if known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_at_secs: Option<u64>,
    /// Arbitrary metadata (non-secret).
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub metadata: HashMap<String, String>,
}

impl Credential {
    pub fn new(provider: Provider, secret: impl Into<String>) -> Self {
        Self {
            provider,
            label: None,
            secret: SecretString::from(secret.into()),
            refresh_token: None,
            expires_at_secs: None,
            metadata: HashMap::new(),
        }
    }

    pub fn with_label(mut self, label: impl Into<String>) -> Self {
        self.label = Some(label.into());
        self
    }

    pub fn with_refresh_token(mut self, rt: impl Into<String>) -> Self {
        self.refresh_token = Some(SecretString::from(rt.into()));
        self
    }

    pub fn with_expiry(mut self, t: SystemTime) -> Self {
        self.expires_at_secs = Some(
            t.duration_since(UNIX_EPOCH)
                .unwrap_or(Duration::ZERO)
                .as_secs(),
        );
        self
    }

    /// Storage key: `"<provider>:<label>"` or `"<provider>"` if no label.
    pub fn storage_key(&self) -> String {
        make_key(&self.provider, self.label.as_deref())
    }

    /// `true` if the credential has expired (best-effort, ignores leeway).
    pub fn is_expired(&self) -> bool {
        match self.expires_at_secs {
            Some(exp) => {
                let now = SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap_or(Duration::ZERO)
                    .as_secs();
                now >= exp
            }
            None => false,
        }
    }
}

/// Public helper so `manager.rs` can build keys without going through `Credential`.
pub fn credential_key(provider: &Provider, label: Option<&str>) -> String {
    make_key(provider, label)
}

fn make_key(provider: &Provider, label: Option<&str>) -> String {
    match label {
        Some(l) if !l.is_empty() => format!("{}:{}", provider.as_key(), l),
        _ => provider.as_key(),
    }
}

// serde helpers for SecretString

fn serialize_secret<S>(s: &SecretString, ser: S) -> Result<S::Ok, S::Error>
where
    S: serde::Serializer,
{
    ser.serialize_str(s.expose_secret())
}

fn deserialize_secret<'de, D>(de: D) -> Result<SecretString, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let s = String::deserialize(de)?;
    Ok(SecretString::from(s))
}

fn serialize_secret_opt<S>(opt: &Option<SecretString>, ser: S) -> Result<S::Ok, S::Error>
where
    S: serde::Serializer,
{
    match opt {
        Some(s) => ser.serialize_some(s.expose_secret()),
        None => ser.serialize_none(),
    }
}

fn deserialize_secret_opt<'de, D>(de: D) -> Result<Option<SecretString>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let opt: Option<String> = Option::deserialize(de)?;
    Ok(opt.map(SecretString::from))
}

// ---------------------------------------------------------------------------
// Backend trait
// ---------------------------------------------------------------------------

#[async_trait]
pub trait Backend: Send + Sync {
    /// Return all stored credentials.
    async fn list(&self) -> Result<Vec<Credential>, AuthError>;
    /// Retrieve a single credential by storage key.
    async fn get(&self, key: &str) -> Result<Option<Credential>, AuthError>;
    /// Persist a credential (insert or replace).
    async fn put(&self, cred: &Credential) -> Result<(), AuthError>;
    /// Delete the credential with the given key.
    async fn remove(&self, key: &str) -> Result<(), AuthError>;
}

// ---------------------------------------------------------------------------
// FileBackend
// ---------------------------------------------------------------------------

/// File-based backend: `~/.config/ohrs/credentials.json` (mode 0600).
///
/// Uses an atomic `rename` to avoid partial writes.
pub struct FileBackend {
    path: PathBuf,
}

impl FileBackend {
    /// Use the default path (`~/.config/ohrs/credentials.json`).
    pub fn default_path() -> Result<PathBuf, AuthError> {
        let base = dirs::config_dir()
            .ok_or_else(|| AuthError::Other("cannot determine config dir".into()))?;
        Ok(base.join("ohrs").join("credentials.json"))
    }

    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    fn load_map(&self) -> Result<HashMap<String, Credential>, AuthError> {
        if !self.path.exists() {
            return Ok(HashMap::new());
        }
        let text = std::fs::read_to_string(&self.path)?;
        let map: HashMap<String, Credential> = serde_json::from_str(&text)?;
        Ok(map)
    }

    fn save_map(&self, map: &HashMap<String, Credential>) -> Result<(), AuthError> {
        // Ensure parent directory exists.
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent)?;
        }

        let json = serde_json::to_string_pretty(map)?;
        // Write to a temp file in the same directory, then rename atomically.
        let tmp_path = self.path.with_extension("json.tmp");
        std::fs::write(&tmp_path, json.as_bytes())?;

        // Set mode 0600 on Unix before rename.
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let perms = std::fs::Permissions::from_mode(0o600);
            std::fs::set_permissions(&tmp_path, perms)?;
        }

        std::fs::rename(&tmp_path, &self.path)?;
        Ok(())
    }
}

#[async_trait]
impl Backend for FileBackend {
    async fn list(&self) -> Result<Vec<Credential>, AuthError> {
        Ok(self.load_map()?.into_values().collect())
    }

    async fn get(&self, key: &str) -> Result<Option<Credential>, AuthError> {
        Ok(self.load_map()?.remove(key))
    }

    async fn put(&self, cred: &Credential) -> Result<(), AuthError> {
        let mut map = self.load_map()?;
        map.insert(cred.storage_key(), cred.clone());
        self.save_map(&map)
    }

    async fn remove(&self, key: &str) -> Result<(), AuthError> {
        let mut map = self.load_map()?;
        map.remove(key);
        self.save_map(&map)
    }
}

// ---------------------------------------------------------------------------
// KeyringBackend
// ---------------------------------------------------------------------------

/// OS keyring backend.  One keyring entry per `(provider, label)`.
///
/// The service name is `"ohrs"`.  On headless systems (containers, CI, WSL)
/// where no keyring daemon is available this will return `AuthError::Keyring`
/// — callers should catch this and fall back to `FileBackend`.
#[cfg(feature = "keyring-backend")]
pub struct KeyringBackend {
    service: String,
    /// Index file that tracks which keys exist (keyring has no "list all").
    index_path: PathBuf,
}

#[cfg(feature = "keyring-backend")]
impl KeyringBackend {
    const SERVICE: &'static str = "ohrs";

    /// Construct and probe the OS keyring.
    ///
    /// Returns `Err(AuthError::Keyring(_))` immediately if no usable keyring
    /// daemon is available (containers, CI, WSL), so callers can fall back
    /// to `FileBackend` before any credential operations are attempted.
    pub fn new() -> Result<Self, AuthError> {
        let base = dirs::config_dir()
            .ok_or_else(|| AuthError::Other("cannot determine config dir".into()))?;
        let index_path = base.join("ohrs").join("keyring_index.json");
        let kb = Self {
            service: Self::SERVICE.into(),
            index_path,
        };
        // Probe: a get on a non-existent key exercises the daemon path and
        // will return NoEntry (Ok) on a working keyring, or an error on a
        // broken / absent one.
        let probe = keyring::Entry::new(&kb.service, "__ohrs_probe__")
            .map_err(|e| AuthError::Keyring(format!("keyring init failed: {e}")))?;
        match probe.get_password() {
            Ok(_) | Err(keyring::Error::NoEntry) => Ok(kb),
            Err(e) => Err(AuthError::Keyring(format!("keyring unavailable: {e}"))),
        }
    }

    fn load_index(&self) -> Vec<String> {
        if !self.index_path.exists() {
            return vec![];
        }
        std::fs::read_to_string(&self.index_path)
            .ok()
            .and_then(|t| serde_json::from_str(&t).ok())
            .unwrap_or_default()
    }

    fn save_index(&self, keys: &[String]) -> Result<(), AuthError> {
        if let Some(parent) = self.index_path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let json = serde_json::to_string(keys)?;
        std::fs::write(&self.index_path, json.as_bytes())?;
        Ok(())
    }

    fn kr_get(&self, key: &str) -> Result<Option<String>, AuthError> {
        let entry = keyring::Entry::new(&self.service, key)
            .map_err(|e| AuthError::Keyring(e.to_string()))?;
        match entry.get_password() {
            Ok(val) => Ok(Some(val)),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(e) => Err(AuthError::Keyring(e.to_string())),
        }
    }

    fn kr_set(&self, key: &str, value: &str) -> Result<(), AuthError> {
        let entry = keyring::Entry::new(&self.service, key)
            .map_err(|e| AuthError::Keyring(e.to_string()))?;
        entry
            .set_password(value)
            .map_err(|e| AuthError::Keyring(e.to_string()))
    }

    fn kr_delete(&self, key: &str) -> Result<(), AuthError> {
        let entry = keyring::Entry::new(&self.service, key)
            .map_err(|e| AuthError::Keyring(e.to_string()))?;
        match entry.delete_credential() {
            Ok(()) => Ok(()),
            Err(keyring::Error::NoEntry) => Ok(()),
            Err(e) => Err(AuthError::Keyring(e.to_string())),
        }
    }
}

#[cfg(feature = "keyring-backend")]
#[async_trait]
impl Backend for KeyringBackend {
    async fn list(&self) -> Result<Vec<Credential>, AuthError> {
        let keys = self.load_index();
        let mut out = Vec::new();
        for key in &keys {
            if let Some(json) = self.kr_get(key)? {
                match serde_json::from_str::<Credential>(&json) {
                    Ok(c) => out.push(c),
                    Err(e) => tracing::warn!("keyring: corrupt entry {key}: {e}"),
                }
            }
        }
        Ok(out)
    }

    async fn get(&self, key: &str) -> Result<Option<Credential>, AuthError> {
        match self.kr_get(key)? {
            Some(json) => {
                let c: Credential = serde_json::from_str(&json)?;
                Ok(Some(c))
            }
            None => Ok(None),
        }
    }

    async fn put(&self, cred: &Credential) -> Result<(), AuthError> {
        let key = cred.storage_key();
        let json = serde_json::to_string(cred)?;
        self.kr_set(&key, &json)?;

        // Update index.
        let mut idx = self.load_index();
        if !idx.contains(&key) {
            idx.push(key);
            self.save_index(&idx)?;
        }
        Ok(())
    }

    async fn remove(&self, key: &str) -> Result<(), AuthError> {
        self.kr_delete(key)?;
        let mut idx = self.load_index();
        idx.retain(|k| k != key);
        self.save_index(&idx)?;
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn tmp_backend(dir: &std::path::Path) -> FileBackend {
        FileBackend::new(dir.join("credentials.json"))
    }

    fn mk_cred(provider: Provider, secret: &str) -> Credential {
        Credential::new(provider, secret)
    }

    #[tokio::test]
    async fn file_backend_add_list_get_remove() {
        let dir = TempDir::new().unwrap();
        let b = tmp_backend(dir.path());

        let cred = mk_cred(Provider::Anthropic, "sk-ant-test");
        b.put(&cred).await.unwrap();

        let list = b.list().await.unwrap();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].secret.expose_secret(), "sk-ant-test");

        let key = cred.storage_key();
        let got = b.get(&key).await.unwrap().unwrap();
        assert_eq!(got.secret.expose_secret(), "sk-ant-test");

        b.remove(&key).await.unwrap();
        assert!(b.get(&key).await.unwrap().is_none());
        assert_eq!(b.list().await.unwrap().len(), 0);
    }

    #[tokio::test]
    async fn file_backend_multiple_providers() {
        let dir = TempDir::new().unwrap();
        let b = tmp_backend(dir.path());

        b.put(&mk_cred(Provider::Anthropic, "ant-key")).await.unwrap();
        b.put(&mk_cred(Provider::OpenAi, "oai-key")).await.unwrap();

        let list = b.list().await.unwrap();
        assert_eq!(list.len(), 2);
    }

    #[tokio::test]
    async fn file_backend_label_isolation() {
        let dir = TempDir::new().unwrap();
        let b = tmp_backend(dir.path());

        let work = mk_cred(Provider::Anthropic, "work-key").with_label("work");
        let personal = mk_cred(Provider::Anthropic, "personal-key").with_label("personal");
        b.put(&work).await.unwrap();
        b.put(&personal).await.unwrap();

        let list = b.list().await.unwrap();
        assert_eq!(list.len(), 2);

        let got = b.get("anthropic:work").await.unwrap().unwrap();
        assert_eq!(got.secret.expose_secret(), "work-key");
    }

    #[tokio::test]
    async fn file_mode_0600() {
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let dir = TempDir::new().unwrap();
            let b = tmp_backend(dir.path());
            b.put(&mk_cred(Provider::Anthropic, "secret")).await.unwrap();
            let meta = std::fs::metadata(dir.path().join("credentials.json")).unwrap();
            let mode = meta.permissions().mode() & 0o777;
            assert_eq!(mode, 0o600, "expected mode 0600, got {mode:o}");
        }
    }

    #[test]
    fn credential_serde_round_trip_full() {
        let cred = mk_cred(Provider::OpenAi, "sk-test")
            .with_label("ci")
            .with_refresh_token("rt-abc");
        let json = serde_json::to_string(&cred).unwrap();
        let back: Credential = serde_json::from_str(&json).unwrap();
        assert_eq!(back.secret.expose_secret(), "sk-test");
        assert_eq!(
            back.refresh_token.unwrap().expose_secret(),
            "rt-abc"
        );
        assert_eq!(back.label.as_deref(), Some("ci"));
    }

    #[test]
    fn credential_serde_minimal() {
        let cred = mk_cred(Provider::Gemini, "gem-key");
        let json = serde_json::to_string(&cred).unwrap();
        let back: Credential = serde_json::from_str(&json).unwrap();
        assert_eq!(back.secret.expose_secret(), "gem-key");
        assert!(back.refresh_token.is_none());
        assert!(back.expires_at_secs.is_none());
        assert!(back.metadata.is_empty());
    }

    #[test]
    fn storage_key_no_label() {
        let cred = mk_cred(Provider::Moonshot, "ms-key");
        assert_eq!(cred.storage_key(), "moonshot");
    }

    #[test]
    fn storage_key_with_label() {
        let cred = mk_cred(Provider::OpenAi, "key").with_label("work");
        assert_eq!(cred.storage_key(), "openai:work");
    }
}
