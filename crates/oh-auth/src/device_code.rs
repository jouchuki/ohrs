//! Generic OAuth2 device-code flow via the `oauth2` crate.
//!
//! Call `DeviceCodeFlow::start()` to initiate; it returns a pending auth with
//! the verification URI and user code. Call `pending.poll()` (or spawn it) to
//! exchange the device code for tokens once the user has authorized.

use std::future::Future;
use std::pin::Pin;
use std::time::Duration;

use oauth2::basic::BasicClient;
use oauth2::{
    AuthUrl, ClientId, DeviceAuthorizationUrl, Scope, StandardDeviceAuthorizationResponse,
    TokenResponse, TokenUrl,
};
use secrecy::SecretString;

use crate::error::AuthError;

/// Static configuration for a provider's OAuth2 endpoints.
#[derive(Debug, Clone)]
pub struct OAuthProviderConfig {
    pub client_id: String,
    pub device_auth_url: String,
    pub token_url: String,
    pub scopes: Vec<String>,
}

impl OAuthProviderConfig {
    /// Pre-baked config for GitHub Copilot device-code flow.
    pub fn github_copilot() -> Self {
        Self {
            client_id: "Iv1.b507a08c87ecfe98".into(),
            device_auth_url: "https://github.com/login/device/code".into(),
            token_url: "https://github.com/login/oauth/access_token".into(),
            scopes: vec!["read:user".into()],
        }
    }
}

/// The result returned by `DeviceCodeFlow::start()`.
pub struct PendingDeviceAuth {
    pub verification_uri: String,
    pub user_code: String,
    /// Call this to block until the user authorizes (or timeout/error).
    poll_fn: Pin<Box<dyn Future<Output = Result<DeviceTokens, AuthError>> + Send>>,
}

impl PendingDeviceAuth {
    /// Consume and await the pending auth, returning the token pair on success.
    pub async fn poll(self) -> Result<DeviceTokens, AuthError> {
        self.poll_fn.await
    }
}

/// Tokens obtained after a successful device-code flow.
#[derive(Debug)]
pub struct DeviceTokens {
    pub access_token: SecretString,
    pub refresh_token: Option<SecretString>,
    pub expires_in: Option<Duration>,
}

/// Stateless helper — call `DeviceCodeFlow::start()` to initiate a flow.
pub struct DeviceCodeFlow;

impl DeviceCodeFlow {
    /// Initiate the device-code flow for the given provider config.
    ///
    /// Returns a `PendingDeviceAuth` that contains the verification URI and
    /// user code to display, plus a future that polls until the user authorizes.
    ///
    /// Note: does not open a browser — the caller handles UX.
    pub async fn start(config: OAuthProviderConfig) -> Result<PendingDeviceAuth, AuthError> {
        let http = reqwest_client()?;

        let device_auth_url = DeviceAuthorizationUrl::new(config.device_auth_url.clone())
            .map_err(|e| AuthError::OAuth(e.to_string()))?;
        let token_url = TokenUrl::new(config.token_url.clone())
            .map_err(|e| AuthError::OAuth(e.to_string()))?;

        let client = BasicClient::new(ClientId::new(config.client_id.clone()))
            .set_device_authorization_url(device_auth_url)
            .set_token_uri(token_url)
            // device-code grant doesn't use an AuthUrl for the redirect, but
            // the oauth2 crate still requires a placeholder for BasicClient.
            .set_auth_uri(
                AuthUrl::new("https://unused.invalid/auth".into())
                    .expect("static placeholder URL"),
            );

        let scopes: Vec<Scope> = config
            .scopes
            .iter()
            .map(|s| Scope::new(s.clone()))
            .collect();

        let details: StandardDeviceAuthorizationResponse = client
            .exchange_device_code()
            .add_scopes(scopes)
            .request_async(&http)
            .await
            .map_err(|e| AuthError::OAuth(e.to_string()))?;

        let verification_uri = details.verification_uri().to_string();
        let user_code = details.user_code().secret().clone();

        // Move ownership into the polling future.
        let poll_fn: Pin<Box<dyn Future<Output = Result<DeviceTokens, AuthError>> + Send>> =
            Box::pin(async move {
                let token = client
                    .exchange_device_access_token(&details)
                    .request_async(
                        &http,
                        tokio::time::sleep,
                        None, // use the server-suggested interval
                    )
                    .await
                    .map_err(|e| AuthError::OAuth(e.to_string()))?;

                Ok(DeviceTokens {
                    access_token: SecretString::from(
                        token.access_token().secret().clone(),
                    ),
                    refresh_token: token
                        .refresh_token()
                        .map(|rt| SecretString::from(rt.secret().clone())),
                    expires_in: token.expires_in(),
                })
            });

        Ok(PendingDeviceAuth {
            verification_uri,
            user_code,
            poll_fn,
        })
    }
}

fn reqwest_client() -> Result<reqwest::Client, AuthError> {
    reqwest::Client::builder()
        .build()
        .map_err(|e| AuthError::OAuth(e.to_string()))
}
