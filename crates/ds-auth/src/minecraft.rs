//! Minecraft services: sign-in, ownership, profile.

use serde::Deserialize;

use crate::endpoints::Endpoints;
use crate::error::{AuthError, Result, Stage};
use crate::xbox::XboxToken;

/// A Minecraft session token and how long it lasts.
#[derive(Debug, Clone)]
pub struct McSession {
    pub access_token: String,
    pub expires_in_secs: u64,
}

/// The player, as Minecraft services reports them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Profile {
    /// Dashless UUID, exactly as returned. The game wants it in this form.
    pub id: String,
    pub name: String,
}

#[derive(Deserialize)]
struct LoginResponse {
    access_token: String,
    #[serde(default)]
    expires_in: u64,
}

#[derive(Deserialize)]
struct ProfileResponse {
    id: String,
    name: String,
}

/// Trade an XSTS token for a Minecraft session.
pub async fn login_with_xbox(
    client: &reqwest::Client,
    endpoints: &Endpoints,
    xsts: &XboxToken,
) -> Result<McSession> {
    let body = serde_json::json!({ "identityToken": xsts.identity_token() });

    let response = client
        .post(&endpoints.mc_login)
        .header("Accept", "application/json")
        .json(&body)
        .send()
        .await
        .map_err(|source| AuthError::Transport {
            stage: Stage::MinecraftLogin,
            source,
        })?;

    let status = response.status();

    // 403 here is almost always "Invalid app registration": the Azure client ID
    // has not been allow-listed for the Minecraft API. That is a property of
    // the build, not of the user's account, so it gets a distinct variant
    // rather than looking like a permissions problem they caused.
    if status == reqwest::StatusCode::FORBIDDEN {
        return Err(AuthError::AppNotApproved);
    }

    if !status.is_success() {
        let body = response.text().await.unwrap_or_default();
        return Err(AuthError::Http {
            stage: Stage::MinecraftLogin,
            status: status.as_u16(),
            body,
        });
    }

    let parsed = response
        .json::<LoginResponse>()
        .await
        .map_err(|source| AuthError::Transport {
            stage: Stage::MinecraftLogin,
            source,
        })?;

    Ok(McSession {
        access_token: parsed.access_token,
        expires_in_secs: parsed.expires_in,
    })
}

/// Fetch the player's profile.
///
/// **This is the authoritative ownership check**, not the entitlements
/// endpoint. `/entitlements/mcstore` can come back empty for a Game Pass
/// subscriber who genuinely owns and can play the game, so gating on it tells a
/// legitimate player they do not own Minecraft. A 200 here is the signal that
/// matters.
///
/// The two failure shapes are deliberately distinguished:
/// - **404** — signed in fine, but no Minecraft profile exists yet (the account
///   has never picked a username). Actionable: go and choose one.
/// - **401/403** — no entitlement. Enriched with the entitlements body when one
///   can be fetched, purely to make the message more specific.
pub async fn profile(
    client: &reqwest::Client,
    endpoints: &Endpoints,
    session: &McSession,
) -> Result<Profile> {
    let response = client
        .get(&endpoints.mc_profile)
        .bearer_auth(&session.access_token)
        .header("Accept", "application/json")
        .send()
        .await
        .map_err(|source| AuthError::Transport {
            stage: Stage::MinecraftProfile,
            source,
        })?;

    let status = response.status();

    if status == reqwest::StatusCode::NOT_FOUND {
        return Err(AuthError::NoProfile);
    }

    if status == reqwest::StatusCode::UNAUTHORIZED || status == reqwest::StatusCode::FORBIDDEN {
        let detail = entitlements_detail(client, endpoints, session).await;
        return Err(AuthError::NotEntitled { detail });
    }

    if !status.is_success() {
        let body = response.text().await.unwrap_or_default();
        return Err(AuthError::Http {
            stage: Stage::MinecraftProfile,
            status: status.as_u16(),
            body,
        });
    }

    let parsed =
        response
            .json::<ProfileResponse>()
            .await
            .map_err(|source| AuthError::Transport {
                stage: Stage::MinecraftProfile,
                source,
            })?;

    Ok(Profile {
        id: parsed.id,
        name: parsed.name,
    })
}

/// Best-effort entitlements fetch, used only to enrich an error message.
///
/// Never fails the sign-in: this is diagnostic colour, and a launcher that
/// turned a failed diagnostic lookup into a failed login would be worse than
/// one that said nothing.
async fn entitlements_detail(
    client: &reqwest::Client,
    endpoints: &Endpoints,
    session: &McSession,
) -> Option<String> {
    let response = client
        .get(&endpoints.mc_entitlements)
        .bearer_auth(&session.access_token)
        .header("Accept", "application/json")
        .send()
        .await
        .ok()?;

    if !response.status().is_success() {
        return None;
    }

    let body = response.text().await.ok()?;
    // Cap it: this ends up in an error message and a log line, not a data store.
    Some(body.chars().take(500).collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::{body_json_string, header, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn xsts() -> XboxToken {
        XboxToken {
            token: "xsts-token".to_owned(),
            user_hash: "user-hash".to_owned(),
        }
    }

    fn session() -> McSession {
        McSession {
            access_token: "mc-token".to_owned(),
            expires_in_secs: 86_400,
        }
    }

    #[tokio::test]
    async fn login_sends_the_documented_identity_token_format() {
        let server = MockServer::start().await;
        let expected = serde_json::json!({ "identityToken": "XBL3.0 x=user-hash;xsts-token" });

        Mock::given(method("POST"))
            .and(path("/authentication/login_with_xbox"))
            .and(body_json_string(expected.to_string()))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "username": "whatever",
                "roles": [],
                "access_token": "mc-token",
                "token_type": "Bearer",
                "expires_in": 86400
            })))
            .expect(1)
            .mount(&server)
            .await;

        let got = login_with_xbox(
            &reqwest::Client::new(),
            &Endpoints::mocked(&server.uri()),
            &xsts(),
        )
        .await
        .expect("login should succeed");

        assert_eq!(got.access_token, "mc-token");
        assert_eq!(got.expires_in_secs, 86_400);
    }

    /// The state this build is in until Mojang approves the app registration.
    /// It must not be reported as the user's fault.
    #[tokio::test]
    async fn forbidden_login_reports_app_not_approved() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/authentication/login_with_xbox"))
            .respond_with(
                ResponseTemplate::new(403)
                    .set_body_string("{\"error\":\"invalid app registration\"}"),
            )
            .mount(&server)
            .await;

        let err = login_with_xbox(
            &reqwest::Client::new(),
            &Endpoints::mocked(&server.uri()),
            &xsts(),
        )
        .await
        .expect_err("403 must be an error");

        assert!(matches!(err, AuthError::AppNotApproved), "got {err:?}");
        assert!(err.to_string().contains("aka.ms/mce-reviewappid"));
        assert!(!err.is_retryable());
    }

    #[tokio::test]
    async fn profile_is_returned_with_a_dashless_uuid() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/minecraft/profile"))
            .and(header("authorization", "Bearer mc-token"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "id": "069a79f444e94726a5befca90e38aaf5",
                "name": "Notch"
            })))
            .mount(&server)
            .await;

        let got = profile(
            &reqwest::Client::new(),
            &Endpoints::mocked(&server.uri()),
            &session(),
        )
        .await
        .expect("profile should succeed");

        assert_eq!(
            got,
            Profile {
                id: "069a79f444e94726a5befca90e38aaf5".to_owned(),
                name: "Notch".to_owned(),
            }
        );
    }

    #[tokio::test]
    async fn missing_profile_is_distinct_from_not_owning_the_game() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/minecraft/profile"))
            .respond_with(ResponseTemplate::new(404))
            .mount(&server)
            .await;

        let err = profile(
            &reqwest::Client::new(),
            &Endpoints::mocked(&server.uri()),
            &session(),
        )
        .await
        .expect_err("404 must be an error");

        assert!(matches!(err, AuthError::NoProfile), "got {err:?}");
        assert!(err.to_string().contains("minecraft.net"));
    }

    #[tokio::test]
    async fn unentitled_account_is_enriched_with_the_entitlements_body() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/minecraft/profile"))
            .respond_with(ResponseTemplate::new(401))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/entitlements/mcstore"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(serde_json::json!({ "items": [] })),
            )
            .mount(&server)
            .await;

        let err = profile(
            &reqwest::Client::new(),
            &Endpoints::mocked(&server.uri()),
            &session(),
        )
        .await
        .expect_err("401 must be an error");

        match err {
            AuthError::NotEntitled { detail } => {
                let detail = detail.expect("entitlements body should have been attached");
                assert!(detail.contains("items"), "got {detail}");
            }
            other => panic!("expected NotEntitled, got {other:?}"),
        }
    }

    /// The diagnostic lookup is best-effort. If it fails, sign-in must still
    /// report the real error rather than the lookup's.
    #[tokio::test]
    async fn entitlements_lookup_failure_does_not_mask_the_real_error() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/minecraft/profile"))
            .respond_with(ResponseTemplate::new(401))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/entitlements/mcstore"))
            .respond_with(ResponseTemplate::new(500))
            .mount(&server)
            .await;

        let err = profile(
            &reqwest::Client::new(),
            &Endpoints::mocked(&server.uri()),
            &session(),
        )
        .await
        .expect_err("401 must still be an error");

        assert!(
            matches!(err, AuthError::NotEntitled { detail: None }),
            "got {err:?}"
        );
    }
}
