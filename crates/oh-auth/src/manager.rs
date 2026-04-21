//! `AuthManager` — unified credential CRUD over a pluggable `Backend`.

use crate::error::AuthError;
use crate::providers::Provider;
use crate::storage::{Backend, Credential, FileBackend};

/// Central credential manager.  Wraps a `Backend` (file or keyring).
pub struct AuthManager {
    backend: Box<dyn Backend>,
}

impl AuthManager {
    /// Build with a `KeyringBackend`; fall back to `FileBackend` if unavailable.
    pub fn with_default_backend() -> Result<Self, AuthError> {
        #[cfg(feature = "keyring-backend")]
        {
            match crate::storage::KeyringBackend::new() {
                Ok(kb) => {
                    // Probe: attempt a harmless get to verify the keyring works.
                    // (We do this synchronously because we're not in an async ctx.)
                    // If the probe is not possible here, just trust the constructor.
                    tracing::debug!("oh-auth: using keyring backend");
                    return Ok(Self {
                        backend: Box::new(kb),
                    });
                }
                Err(e) => {
                    tracing::info!("oh-auth: keyring unavailable ({e}), falling back to file");
                }
            }
        }

        let path = FileBackend::default_path()?;
        Ok(Self {
            backend: Box::new(FileBackend::new(path)),
        })
    }

    /// Use an explicit backend (useful for tests and custom setups).
    pub fn with_backend(backend: Box<dyn Backend>) -> Self {
        Self { backend }
    }

    /// List all stored credentials.
    pub async fn list(&self) -> Result<Vec<Credential>, AuthError> {
        self.backend.list().await
    }

    /// List credentials for a specific provider.
    pub async fn list_for(&self, p: &Provider) -> Result<Vec<Credential>, AuthError> {
        let all = self.backend.list().await?;
        Ok(all.into_iter().filter(|c| &c.provider == p).collect())
    }

    /// Retrieve a single credential. `label = None` returns the default slot.
    pub async fn get(
        &self,
        p: &Provider,
        label: Option<&str>,
    ) -> Result<Option<Credential>, AuthError> {
        let key = crate::storage::credential_key(p, label);
        self.backend.get(&key).await
    }

    /// Persist a credential (insert or replace).
    pub async fn add(&self, cred: Credential) -> Result<(), AuthError> {
        self.backend.put(&cred).await
    }

    /// Remove a credential. Silent if not found.
    pub async fn remove(&self, p: &Provider, label: Option<&str>) -> Result<(), AuthError> {
        let key = crate::storage::credential_key(p, label);
        self.backend.remove(&key).await
    }

    /// Return the credential, refreshing it first if it is expired.
    ///
    /// Currently only signals an error when the credential is expired and no
    /// refresh token is available; actual OAuth refresh is left to callers so
    /// we avoid hard-coding provider-specific token endpoints here.
    pub async fn refresh_if_needed(
        &self,
        p: &Provider,
        label: Option<&str>,
    ) -> Result<Credential, AuthError> {
        let cred = self
            .get(p, label)
            .await?
            .ok_or_else(|| AuthError::NotFound {
                provider: p.to_string(),
                label: label.map(str::to_owned),
            })?;

        if cred.is_expired() {
            match &cred.refresh_token {
                None => {
                    return Err(AuthError::OAuth(format!(
                        "credential for {p} is expired and no refresh token is available"
                    )));
                }
                Some(_rt) => {
                    // Placeholder: a real implementation would call the provider's
                    // token endpoint here. Return the stale credential for now so
                    // callers can decide what to do.
                    tracing::warn!(
                        "oh-auth: credential for {} is expired; refresh not yet implemented",
                        p
                    );
                }
            }
        }

        Ok(cred)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::FileBackend;
    use secrecy::ExposeSecret;
    use std::time::SystemTime;
    use tempfile::TempDir;

    fn make_manager(dir: &std::path::Path) -> AuthManager {
        AuthManager::with_backend(Box::new(FileBackend::new(
            dir.join("credentials.json"),
        )))
    }

    #[tokio::test]
    async fn manager_add_get_remove() {
        let dir = TempDir::new().unwrap();
        let m = make_manager(dir.path());

        let cred = Credential::new(Provider::Anthropic, "sk-test");
        m.add(cred).await.unwrap();

        let got = m.get(&Provider::Anthropic, None).await.unwrap().unwrap();
        assert_eq!(got.secret.expose_secret(), "sk-test");

        m.remove(&Provider::Anthropic, None).await.unwrap();
        assert!(m.get(&Provider::Anthropic, None).await.unwrap().is_none());
    }

    #[tokio::test]
    async fn manager_list_for() {
        let dir = TempDir::new().unwrap();
        let m = make_manager(dir.path());

        m.add(Credential::new(Provider::Anthropic, "ant1")).await.unwrap();
        m.add(
            Credential::new(Provider::Anthropic, "ant2").with_label("work"),
        )
        .await
        .unwrap();
        m.add(Credential::new(Provider::OpenAi, "oai1")).await.unwrap();

        let ant = m.list_for(&Provider::Anthropic).await.unwrap();
        assert_eq!(ant.len(), 2);

        let oai = m.list_for(&Provider::OpenAi).await.unwrap();
        assert_eq!(oai.len(), 1);
    }

    #[tokio::test]
    async fn manager_refresh_not_found() {
        let dir = TempDir::new().unwrap();
        let m = make_manager(dir.path());

        let err = m
            .refresh_if_needed(&Provider::Moonshot, None)
            .await
            .unwrap_err();
        assert!(matches!(err, AuthError::NotFound { .. }));
    }

    #[tokio::test]
    async fn manager_refresh_valid_cred_returns_it() {
        let dir = TempDir::new().unwrap();
        let m = make_manager(dir.path());

        let future_exp = SystemTime::now() + std::time::Duration::from_secs(3600);
        let cred = Credential::new(Provider::Gemini, "gem-key").with_expiry(future_exp);
        m.add(cred).await.unwrap();

        let got = m
            .refresh_if_needed(&Provider::Gemini, None)
            .await
            .unwrap();
        assert_eq!(got.secret.expose_secret(), "gem-key");
    }
}
