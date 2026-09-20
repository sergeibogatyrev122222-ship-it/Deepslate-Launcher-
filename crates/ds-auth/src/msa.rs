//! Microsoft OAuth 2.0, authorization code flow with PKCE.
//!
//! Public client: no secret is shipped, because a program running on someone
//! else's computer cannot keep one. PKCE takes its place.

use serde::Deserialize;

use crate::endpoints::{Endpoints, SCOPE};
use crate::error::{AuthError, Result, Stage};
use crate::pkce::{Pkce, State};

/// Tokens from Microsoft, before any Xbox or Minecraft exchange.
#[derive(Debug, Clone)]
pub struct MsaTokens {
    pub access_token: String,
    /// Absent if the tenant declined to issue one - the caller must then treat
    /// the session as non-renewable rather than assume it can refresh later.
    pub refresh_token: Option<String>,
    pub expires_in_secs: u64,
}

#[derive(Deserialize)]
struct TokenResponse {
    access_token: String,
    #[serde(default)]
    refresh_token: Option<String>,
    #[serde(default)]
    expires_in: u64,
}

#[derive(Deserialize)]
struct TokenError {
    error: String,
    #[serde(default)]
    error_description: Option<String>,
}

/// Build the URL to open in the user's browser.
///
/// Pure, so the exact query string is testable without a browser or a network.
pub fn authorize_url(
    endpoints: &Endpoints,
    client_id: &str,
    redirect_uri: &str,
    pkce: &Pkce,
    state: &State,
) -> String {
    // `url` does the percent-encoding; hand-rolling it is how redirect URIs end
    // up subtly wrong.
    let mut url = String::from(&endpoints.msa_authorize);
    url.push('?');

    let pairs = [
        ("client_id", client_id),
        ("response_type", "code"),
        ("redirect_uri", redirect_uri),
        ("response_mode", "query"),
        ("scope", SCOPE),
        ("state", state.as_str()),
        ("code_challenge", pkce.challenge()),
        ("code_challenge_method", pkce.method()),
        // Always show the picker: without it a machine with one signed-in
        // account silently reuses it, which makes adding a second account
        // impossible.
        ("prompt", "select_account"),
    ];

    let query: String = pairs
        .iter()
        .map(|(k, v)| {
            format!(
                "{}={}",
                k,
                url::form_urlencoded::byte_serialize(v.as_bytes()).collect::<String>()
            )
        })
        .collect::<Vec<_>>()
        .join("&");

    url.push_str(&query);
    url
}

/// Redeem the authorization code returned through the redirect.
pub async fn exchange_code(
    client: &reqwest::Client,
    endpoints: &Endpoints,
    client_id: &str,
    redirect_uri: &str,
    code: &str,
    pkce: &Pkce,
) -> Result<MsaTokens> {
    let form = [
        ("client_id", client_id),
        ("grant_type", "authorization_code"),
        ("code", code),
        ("redirect_uri", redirect_uri),
        ("code_verifier", pkce.verifier()),
        ("scope", SCOPE),
    ];
    post_token(client, endpoints, &form).await
}

/// Renew an expired access token without user interaction.
pub async fn refresh(
    client: &reqwest::Client,
    endpoints: &Endpoints,
    client_id: &str,
    refresh_token: &str,
) -> Result<MsaTokens> {
    let form = [
        ("client_id", client_id),
        ("grant_type", "refresh_token"),
        ("refresh_token", refresh_token),
        ("scope", SCOPE),
    ];
    post_token(client, endpoints, &form).await
}

async fn post_token(
    client: &reqwest::Client,
    endpoints: &Endpoints,
    form: &[(&str, &str)],
) -> Result<MsaTokens> {
    let response = client
        .post(&endpoints.msa_token)
        .form(form)
        .send()
        .await
        .map_err(|source| AuthError::Transport {
            stage: Stage::Microsoft,
            source,
        })?;

    let status = response.status();
    let body = response
        .text()
        .await
        .map_err(|source| AuthError::Transport {
            stage: Stage::Microsoft,
            source,
        })?;

    if !status.is_success() {
        // `invalid_grant` is the one that matters: the refresh token is spent,
        // revoked or too old. It is a normal part of the lifecycle, not a
        // malfunction, and the remedy is to sign in again - so it gets its own
        // variant rather than a generic HTTP error.
        if let Ok(parsed) = serde_json::from_str::<TokenError>(&body) {
            if parsed.error == "invalid_grant" {
                return Err(AuthError::RefreshExpired);
            }
            return Err(AuthError::Protocol {
                stage: Stage::Microsoft,
                detail: parsed.error_description.unwrap_or(parsed.error),
            });
        }

        return Err(AuthError::Http {
            stage: Stage::Microsoft,
            status: status.as_u16(),
            body,
        });
    }

    let parsed =
        serde_json::from_str::<TokenResponse>(&body).map_err(|error| AuthError::Protocol {
            stage: Stage::Microsoft,
            detail: format!("token response was not the documented shape: {error}"),
        })?;

    Ok(MsaTokens {
        access_token: parsed.access_token,
        refresh_token: parsed.refresh_token,
        expires_in_secs: parsed.expires_in,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::{body_string_contains, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn endpoints() -> Endpoints {
        Endpoints::production()
    }

    #[test]
    fn authorize_url_carries_every_required_parameter() {
        let pkce = Pkce::generate();
        let state = State::generate();
        let url = authorize_url(
            &endpoints(),
            "the-client-id",
            "http://127.0.0.1:41573/callback",
            &pkce,
            &state,
        );

        assert!(url.starts_with("https://login.microsoftonline.com/consumers/"));
        assert!(url.contains("client_id=the-client-id"));
        assert!(url.contains("response_type=code"));
        assert!(url.contains("code_challenge_method=S256"));
        assert!(url.contains(&format!("code_challenge={}", pkce.challenge())));
        assert!(url.contains(&format!("state={}", state.as_str())));
        assert!(url.contains("prompt=select_account"));
    }

    /// The verifier is the secret half of PKCE. Sending it in the browser URL
    /// would defeat the entire mechanism.
    #[test]
    fn authorize_url_never_contains_the_verifier() {
        let pkce = Pkce::generate();
        let url = authorize_url(
            &endpoints(),
            "id",
            "http://127.0.0.1:1/callback",
            &pkce,
            &State::generate(),
        );
        assert!(!url.contains(pkce.verifier()));
    }

    #[test]
    fn authorize_url_percent_encodes_the_redirect() {
        let url = authorize_url(
            &endpoints(),
            "id",
            "http://127.0.0.1:41573/callback",
            &Pkce::generate(),
            &State::generate(),
        );
        assert!(
            url.contains("redirect_uri=http%3A%2F%2F127.0.0.1%3A41573%2Fcallback"),
            "redirect was not encoded: {url}"
        );
    }

    #[test]
    fn scope_requests_only_what_is_needed() {
        assert!(SCOPE.contains("XboxLive.signin"));
        assert!(SCOPE.contains("offline_access"));
        // A launcher has no business holding broader consent.
        assert!(!SCOPE.contains("Mail"));
        assert!(!SCOPE.contains("User.Read"));
    }

    #[tokio::test]
    async fn code_exchange_sends_the_verifier_and_returns_tokens() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/oauth2/token"))
            .and(body_string_contains("grant_type=authorization_code"))
            .and(body_string_contains("code_verifier="))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "token_type": "Bearer",
                "expires_in": 3600,
                "scope": SCOPE,
                "access_token": "msa-access",
                "refresh_token": "msa-refresh"
            })))
            .expect(1)
            .mount(&server)
            .await;

        let tokens = exchange_code(
            &reqwest::Client::new(),
            &Endpoints::mocked(&server.uri()),
            "cid",
            "http://127.0.0.1:1/callback",
            "the-code",
            &Pkce::generate(),
        )
        .await
        .expect("exchange should succeed");

        assert_eq!(tokens.access_token, "msa-access");
        assert_eq!(tokens.refresh_token.as_deref(), Some("msa-refresh"));
        assert_eq!(tokens.expires_in_secs, 3600);
    }

    /// A spent refresh token is a routine lifecycle event, and must be
    /// reported as "sign in again", not as an opaque HTTP failure.
    #[tokio::test]
    async fn invalid_grant_is_reported_as_an_expired_session() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/oauth2/token"))
            .respond_with(ResponseTemplate::new(400).set_body_json(serde_json::json!({
                "error": "invalid_grant",
                "error_description": "AADSTS70000: The refresh token has expired."
            })))
            .mount(&server)
            .await;

        let err = refresh(
            &reqwest::Client::new(),
            &Endpoints::mocked(&server.uri()),
            "cid",
            "stale-token",
        )
        .await
        .expect_err("invalid_grant must be an error");

        assert!(matches!(err, AuthError::RefreshExpired), "got {err:?}");
        assert!(err.needs_reauth());
        assert!(!err.is_retryable());
    }

    #[tokio::test]
    async fn other_oauth_errors_keep_their_description() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/oauth2/token"))
            .respond_with(ResponseTemplate::new(400).set_body_json(serde_json::json!({
                "error": "unauthorized_client",
                "error_description": "AADSTS700016: Application not found in directory."
            })))
            .mount(&server)
            .await;

        let err = refresh(
            &reqwest::Client::new(),
            &Endpoints::mocked(&server.uri()),
            "cid",
            "tok",
        )
        .await
        .expect_err("should error");

        match err {
            AuthError::Protocol { detail, .. } => {
                assert!(detail.contains("AADSTS700016"), "got {detail}");
            }
            other => panic!("expected Protocol, got {other:?}"),
        }
    }

    /// A tenant that issues no refresh token must not be silently treated as if
    /// it had - that would produce a session that cannot be renewed and a
    /// confusing failure much later.
    #[tokio::test]
    async fn missing_refresh_token_is_represented_honestly() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/oauth2/token"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "token_type": "Bearer",
                "expires_in": 3600,
                "access_token": "only-access"
            })))
            .mount(&server)
            .await;

        let tokens = refresh(
            &reqwest::Client::new(),
            &Endpoints::mocked(&server.uri()),
            "cid",
            "tok",
        )
        .await
        .expect("should succeed");

        assert!(tokens.refresh_token.is_none());
    }
}
