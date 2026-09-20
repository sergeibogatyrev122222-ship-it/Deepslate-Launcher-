//! The sign-in flow, assembled.
//!
//! The individual legs live in [`crate::msa`], [`crate::xbox`] and
//! [`crate::minecraft`]. This module is only the ordering and the plumbing
//! between them, which is why it is short: the interesting logic and all the
//! failure handling belong to the legs.

use std::time::Duration;

use crate::endpoints::Endpoints;
use crate::error::Result;
use crate::loopback::Loopback;
use crate::minecraft::{McSession, Profile};
use crate::msa::{self, MsaTokens};
use crate::pkce::{Pkce, State};
use crate::store::{now_unix, Account};
use crate::{app_id, minecraft, xbox};

/// How long to wait for the user to finish in the browser before giving up.
///
/// Long enough to find a password manager, read a consent screen and deal with
/// two-factor; short enough that an abandoned attempt does not leave a socket
/// listening forever.
const BROWSER_TIMEOUT: Duration = Duration::from_secs(300);

/// A completed sign-in.
#[derive(Debug, Clone)]
pub struct SignedIn {
    pub profile: Profile,
    pub session: McSession,
    /// Absent when Microsoft declined to issue one, in which case this session
    /// cannot be renewed silently later.
    pub refresh_token: Option<String>,
}

impl SignedIn {
    /// The non-secret record to keep on disk.
    pub fn account(&self) -> Account {
        let now = now_unix();
        Account {
            id: self.profile.id.clone(),
            name: self.profile.name.clone(),
            added_at: now,
            last_used: now,
        }
    }
}

/// Runs the sign-in chain.
pub struct Flow {
    client: reqwest::Client,
    endpoints: Endpoints,
    client_id: String,
}

impl Default for Flow {
    fn default() -> Self {
        Self::new()
    }
}

impl Flow {
    pub fn new() -> Self {
        Self {
            client: reqwest::Client::new(),
            endpoints: Endpoints::production(),
            client_id: app_id::client_id(),
        }
    }

    /// Point the flow at a mock server.
    #[cfg(test)]
    pub fn mocked(base: &str) -> Self {
        Self {
            client: reqwest::Client::new(),
            endpoints: Endpoints::mocked(base),
            client_id: "test-client-id".to_owned(),
        }
    }

    /// Full interactive sign-in through the system browser.
    ///
    /// The listener is bound *before* the browser opens, so the redirect can
    /// never arrive at a port nobody is listening on.
    pub async fn sign_in(&self) -> Result<SignedIn> {
        let loopback = Loopback::bind().await?;
        let redirect_uri = loopback.redirect_uri();

        let pkce = Pkce::generate();
        let state = State::generate();
        let url = msa::authorize_url(
            &self.endpoints,
            &self.client_id,
            &redirect_uri,
            &pkce,
            &state,
        );

        // The system browser, not an embedded webview: the user sees a real
        // address bar they can verify, and their password manager works.
        // `open` rather than a hand-built `cmd /c start`, because an
        // authorization URL is full of `&` and quoting that through a shell
        // correctly on three platforms is a command-injection bug waiting to
        // happen.
        open::that(&url).map_err(|source| crate::AuthError::Loopback { source })?;

        let code = loopback.wait_for_code(&state, BROWSER_TIMEOUT).await?;

        let tokens = msa::exchange_code(
            &self.client,
            &self.endpoints,
            &self.client_id,
            &redirect_uri,
            &code,
            &pkce,
        )
        .await?;

        self.finish(tokens).await
    }

    /// Silent sign-in using a stored refresh token.
    ///
    /// The common path: the launcher does this at startup so the user is
    /// already signed in by the time they look at it.
    pub async fn resume(&self, refresh_token: &str) -> Result<SignedIn> {
        let tokens = msa::refresh(
            &self.client,
            &self.endpoints,
            &self.client_id,
            refresh_token,
        )
        .await?;
        self.finish(tokens).await
    }

    /// Everything downstream of holding Microsoft tokens.
    ///
    /// Shared by both entry points so the interactive and silent paths cannot
    /// drift apart - a bug fixed in one would otherwise survive in the other.
    async fn finish(&self, tokens: MsaTokens) -> Result<SignedIn> {
        let xbl = xbox::authenticate(&self.client, &self.endpoints, &tokens.access_token).await?;
        let xsts = xbox::authorize(&self.client, &self.endpoints, &xbl).await?;
        let session = minecraft::login_with_xbox(&self.client, &self.endpoints, &xsts).await?;
        let profile = minecraft::profile(&self.client, &self.endpoints, &session).await?;

        Ok(SignedIn {
            profile,
            session,
            refresh_token: tokens.refresh_token,
        })
    }

    /// The authorization URL, exposed for tests and diagnostics.
    pub fn authorize_url_for(&self, redirect_uri: &str, pkce: &Pkce, state: &State) -> String {
        msa::authorize_url(&self.endpoints, &self.client_id, redirect_uri, pkce, state)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::{xerr, AuthError};
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    /// Stand up every endpoint the post-Microsoft part of the chain needs.
    async fn happy_chain() -> MockServer {
        let server = MockServer::start().await;

        Mock::given(method("POST"))
            .and(path("/oauth2/token"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "token_type": "Bearer", "expires_in": 3600,
                "access_token": "msa-access", "refresh_token": "msa-refresh"
            })))
            .mount(&server)
            .await;

        Mock::given(method("POST"))
            .and(path("/user/authenticate"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "Token": "xbl", "DisplayClaims": { "xui": [ { "uhs": "hash" } ] }
            })))
            .mount(&server)
            .await;

        Mock::given(method("POST"))
            .and(path("/xsts/authorize"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "Token": "xsts", "DisplayClaims": { "xui": [ { "uhs": "hash" } ] }
            })))
            .mount(&server)
            .await;

        Mock::given(method("POST"))
            .and(path("/authentication/login_with_xbox"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "access_token": "mc-token", "expires_in": 86400
            })))
            .mount(&server)
            .await;

        Mock::given(method("GET"))
            .and(path("/minecraft/profile"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "id": "069a79f444e94726a5befca90e38aaf5", "name": "Notch"
            })))
            .mount(&server)
            .await;

        server
    }

    #[tokio::test]
    async fn resume_walks_the_whole_chain_and_returns_a_profile() {
        let server = happy_chain().await;
        let signed_in = Flow::mocked(&server.uri())
            .resume("stored-refresh")
            .await
            .expect("resume should succeed");

        assert_eq!(signed_in.profile.name, "Notch");
        assert_eq!(signed_in.profile.id, "069a79f444e94726a5befca90e38aaf5");
        assert_eq!(signed_in.session.access_token, "mc-token");
        assert_eq!(signed_in.refresh_token.as_deref(), Some("msa-refresh"));
    }

    #[tokio::test]
    async fn the_account_record_carries_no_secrets() {
        let server = happy_chain().await;
        let signed_in = Flow::mocked(&server.uri())
            .resume("stored-refresh")
            .await
            .expect("resume should succeed");

        let account = signed_in.account();
        let encoded = serde_json::to_string(&account).expect("account should serialise");

        for secret in ["msa-refresh", "mc-token", "stored-refresh", "xsts", "xbl"] {
            assert!(
                !encoded.contains(secret),
                "account record leaked {secret}: {encoded}"
            );
        }
        assert_eq!(account.name, "Notch");
    }

    /// A refusal partway along must surface as that refusal, not as a generic
    /// failure from whichever call happened to come next.
    #[tokio::test]
    async fn a_mid_chain_refusal_is_reported_precisely() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/oauth2/token"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "access_token": "a", "refresh_token": "r", "expires_in": 3600
            })))
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/user/authenticate"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "Token": "xbl", "DisplayClaims": { "xui": [ { "uhs": "hash" } ] }
            })))
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/xsts/authorize"))
            .respond_with(ResponseTemplate::new(401).set_body_json(serde_json::json!({
                "XErr": xerr::CHILD_ACCOUNT
            })))
            .mount(&server)
            .await;

        let err = Flow::mocked(&server.uri())
            .resume("r")
            .await
            .expect_err("a child account must fail the chain");

        assert!(matches!(err, AuthError::ChildAccount), "got {err:?}");
    }

    /// The state this project is in until Mojang approves the registration -
    /// it must be named, not mistaken for the user's problem.
    #[tokio::test]
    async fn an_unapproved_app_is_named_as_such() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/oauth2/token"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "access_token": "a", "refresh_token": "r", "expires_in": 3600
            })))
            .mount(&server)
            .await;
        for p in ["/user/authenticate", "/xsts/authorize"] {
            Mock::given(method("POST"))
                .and(path(p))
                .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                    "Token": "t", "DisplayClaims": { "xui": [ { "uhs": "h" } ] }
                })))
                .mount(&server)
                .await;
        }
        Mock::given(method("POST"))
            .and(path("/authentication/login_with_xbox"))
            .respond_with(ResponseTemplate::new(403))
            .mount(&server)
            .await;

        let err = Flow::mocked(&server.uri())
            .resume("r")
            .await
            .expect_err("403 must fail");

        assert!(matches!(err, AuthError::AppNotApproved), "got {err:?}");
    }

    /// An expired refresh token must ask for interactive sign-in rather than
    /// looking like a server fault.
    #[tokio::test]
    async fn an_expired_refresh_token_asks_for_a_new_sign_in() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/oauth2/token"))
            .respond_with(ResponseTemplate::new(400).set_body_json(serde_json::json!({
                "error": "invalid_grant"
            })))
            .mount(&server)
            .await;

        let err = Flow::mocked(&server.uri())
            .resume("stale")
            .await
            .expect_err("expired refresh must fail");

        assert!(err.needs_reauth(), "got {err:?}");
    }
}
