//! Xbox Live and XSTS.
//!
//! Two hops. The first trades a Microsoft token for an Xbox Live user token;
//! the second trades that for a security token scoped to Minecraft services.
//! The second hop is where most real-world sign-in failures surface, because it
//! is the point at which Xbox applies account-level rules - child accounts,
//! regional restrictions, accounts that never had an Xbox profile.

use serde::Deserialize;

use crate::endpoints::{Endpoints, MC_RELYING_PARTY, XBL_RELYING_PARTY};
use crate::error::{AuthError, Result, Stage};

/// An Xbox token plus the user hash that must accompany it.
///
/// The two always travel together - Minecraft wants them combined as
/// `XBL3.0 x=<user_hash>;<token>` - so keeping them in one type removes a class
/// of mistake where only the token is passed on.
#[derive(Debug, Clone)]
pub struct XboxToken {
    pub token: String,
    pub user_hash: String,
}

impl XboxToken {
    /// The `Authorization`-style value Minecraft's login endpoint expects.
    pub fn identity_token(&self) -> String {
        format!("XBL3.0 x={};{}", self.user_hash, self.token)
    }
}

#[derive(Deserialize)]
struct XboxResponse {
    #[serde(rename = "Token")]
    token: String,
    #[serde(rename = "DisplayClaims")]
    display_claims: DisplayClaims,
}

#[derive(Deserialize)]
struct DisplayClaims {
    xui: Vec<Xui>,
}

#[derive(Deserialize)]
struct Xui {
    uhs: String,
}

/// The body XSTS returns on a 401 when it refuses.
#[derive(Deserialize)]
struct XstsRefusal {
    #[serde(rename = "XErr")]
    xerr: Option<u64>,
}

impl XboxResponse {
    fn into_token(self, stage: Stage) -> Result<XboxToken> {
        let user_hash = self
            .display_claims
            .xui
            .into_iter()
            .next()
            .map(|x| x.uhs)
            .ok_or_else(|| AuthError::Protocol {
                stage,
                detail: "response contained no DisplayClaims.xui entries".to_owned(),
            })?;

        Ok(XboxToken {
            token: self.token,
            user_hash,
        })
    }
}

/// Exchange a Microsoft access token for an Xbox Live user token.
pub async fn authenticate(
    client: &reqwest::Client,
    endpoints: &Endpoints,
    msa_access_token: &str,
) -> Result<XboxToken> {
    let body = serde_json::json!({
        "Properties": {
            "AuthMethod": "RPS",
            "SiteName": "user.auth.xboxlive.com",
            // The "d=" prefix is required for tokens obtained through the
            // consumers tenant. Without it Xbox rejects the ticket.
            "RpsTicket": format!("d={msa_access_token}"),
        },
        "RelyingParty": XBL_RELYING_PARTY,
        "TokenType": "JWT",
    });

    let response = client
        .post(&endpoints.xbl_authenticate)
        .header("Accept", "application/json")
        .json(&body)
        .send()
        .await
        .map_err(|source| AuthError::Transport {
            stage: Stage::XboxLive,
            source,
        })?;

    let status = response.status();
    if !status.is_success() {
        let body = response.text().await.unwrap_or_default();
        return Err(AuthError::Http {
            stage: Stage::XboxLive,
            status: status.as_u16(),
            body,
        });
    }

    response
        .json::<XboxResponse>()
        .await
        .map_err(|source| AuthError::Transport {
            stage: Stage::XboxLive,
            source,
        })?
        .into_token(Stage::XboxLive)
}

/// Exchange an Xbox Live user token for a Minecraft-scoped XSTS token.
///
/// A 401 here is not a transport failure - it is Xbox telling us precisely why
/// this account may not sign in, via an `XErr` code. That is the most
/// user-actionable signal in the whole chain, so it is decoded rather than
/// reported as "unauthorized".
pub async fn authorize(
    client: &reqwest::Client,
    endpoints: &Endpoints,
    xbl: &XboxToken,
) -> Result<XboxToken> {
    let body = serde_json::json!({
        "Properties": {
            "SandboxId": "RETAIL",
            "UserTokens": [xbl.token],
        },
        "RelyingParty": MC_RELYING_PARTY,
        "TokenType": "JWT",
    });

    let response = client
        .post(&endpoints.xsts_authorize)
        .header("Accept", "application/json")
        .json(&body)
        .send()
        .await
        .map_err(|source| AuthError::Transport {
            stage: Stage::Xsts,
            source,
        })?;

    let status = response.status();

    if status == reqwest::StatusCode::UNAUTHORIZED {
        let raw = response.text().await.unwrap_or_default();
        // A 401 without a parseable XErr is still a refusal, just an opaque
        // one; surface the body rather than inventing a reason.
        return Err(match serde_json::from_str::<XstsRefusal>(&raw) {
            Ok(XstsRefusal { xerr: Some(xerr) }) => AuthError::from_xerr(xerr),
            _ => AuthError::Http {
                stage: Stage::Xsts,
                status: 401,
                body: raw,
            },
        });
    }

    if !status.is_success() {
        let body = response.text().await.unwrap_or_default();
        return Err(AuthError::Http {
            stage: Stage::Xsts,
            status: status.as_u16(),
            body,
        });
    }

    response
        .json::<XboxResponse>()
        .await
        .map_err(|source| AuthError::Transport {
            stage: Stage::Xsts,
            source,
        })?
        .into_token(Stage::Xsts)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::xerr;
    use wiremock::matchers::{body_json_string, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn xbox_ok_body(token: &str, uhs: &str) -> serde_json::Value {
        serde_json::json!({
            "IssueInstant": "2026-09-20T00:00:00.0000000Z",
            "NotAfter": "2026-09-21T00:00:00.0000000Z",
            "Token": token,
            "DisplayClaims": { "xui": [ { "uhs": uhs } ] }
        })
    }

    #[tokio::test]
    async fn authenticate_extracts_token_and_user_hash() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/user/authenticate"))
            .respond_with(ResponseTemplate::new(200).set_body_json(xbox_ok_body("xbl-tok", "uhs1")))
            .mount(&server)
            .await;

        let token = authenticate(
            &reqwest::Client::new(),
            &Endpoints::mocked(&server.uri()),
            "msa-token",
        )
        .await
        .expect("authenticate should succeed");

        assert_eq!(token.token, "xbl-tok");
        assert_eq!(token.user_hash, "uhs1");
    }

    /// Xbox rejects a ticket that is not prefixed with `d=`, so this is a
    /// contract worth pinning rather than trusting to memory.
    #[tokio::test]
    async fn authenticate_prefixes_the_rps_ticket() {
        let server = MockServer::start().await;
        let expected = serde_json::json!({
            "Properties": {
                "AuthMethod": "RPS",
                "SiteName": "user.auth.xboxlive.com",
                "RpsTicket": "d=msa-token"
            },
            "RelyingParty": XBL_RELYING_PARTY,
            "TokenType": "JWT"
        });

        Mock::given(method("POST"))
            .and(path("/user/authenticate"))
            .and(body_json_string(expected.to_string()))
            .respond_with(ResponseTemplate::new(200).set_body_json(xbox_ok_body("t", "u")))
            .expect(1)
            .mount(&server)
            .await;

        authenticate(
            &reqwest::Client::new(),
            &Endpoints::mocked(&server.uri()),
            "msa-token",
        )
        .await
        .expect("body should match the documented shape");
    }

    #[tokio::test]
    async fn identity_token_uses_the_documented_format() {
        let token = XboxToken {
            token: "abc".to_owned(),
            user_hash: "123".to_owned(),
        };
        assert_eq!(token.identity_token(), "XBL3.0 x=123;abc");
    }

    #[tokio::test]
    async fn missing_user_hash_is_a_protocol_error_not_a_panic() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/user/authenticate"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "Token": "t",
                "DisplayClaims": { "xui": [] }
            })))
            .mount(&server)
            .await;

        let err = authenticate(
            &reqwest::Client::new(),
            &Endpoints::mocked(&server.uri()),
            "msa",
        )
        .await
        .expect_err("empty xui must be rejected");

        assert!(matches!(err, AuthError::Protocol { .. }), "got {err:?}");
    }

    async fn xsts_refusal_yields(xerr: u64) -> AuthError {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/xsts/authorize"))
            .respond_with(ResponseTemplate::new(401).set_body_json(serde_json::json!({
                "Identity": "0",
                "XErr": xerr,
                "Message": "",
                "Redirect": "https://start.ui.xboxlive.com/"
            })))
            .mount(&server)
            .await;

        let xbl = XboxToken {
            token: "t".to_owned(),
            user_hash: "u".to_owned(),
        };
        authorize(
            &reqwest::Client::new(),
            &Endpoints::mocked(&server.uri()),
            &xbl,
        )
        .await
        .expect_err("a 401 with an XErr must be an error")
    }

    #[tokio::test]
    async fn no_xbox_account_is_reported_specifically() {
        assert!(matches!(
            xsts_refusal_yields(xerr::NO_XBOX_ACCOUNT).await,
            AuthError::NoXboxAccount
        ));
    }

    #[tokio::test]
    async fn child_account_is_reported_specifically() {
        assert!(matches!(
            xsts_refusal_yields(xerr::CHILD_ACCOUNT).await,
            AuthError::ChildAccount
        ));
    }

    #[tokio::test]
    async fn region_restriction_is_reported_specifically() {
        assert!(matches!(
            xsts_refusal_yields(xerr::REGION_UNAVAILABLE).await,
            AuthError::RegionUnavailable
        ));
    }

    /// The reason the catch-all exists: a code Microsoft has not documented
    /// must still reach the user intact.
    #[tokio::test]
    async fn undocumented_xerr_survives_with_its_code() {
        match xsts_refusal_yields(2_148_916_999).await {
            AuthError::XstsUnknown { xerr } => assert_eq!(xerr, 2_148_916_999),
            other => panic!("expected XstsUnknown, got {other:?}"),
        }
    }

    /// A 401 that is not shaped like a refusal must not be silently turned into
    /// a wrong diagnosis.
    #[tokio::test]
    async fn unparseable_401_reports_the_raw_body() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/xsts/authorize"))
            .respond_with(ResponseTemplate::new(401).set_body_string("<html>gateway</html>"))
            .mount(&server)
            .await;

        let xbl = XboxToken {
            token: "t".to_owned(),
            user_hash: "u".to_owned(),
        };
        let err = authorize(
            &reqwest::Client::new(),
            &Endpoints::mocked(&server.uri()),
            &xbl,
        )
        .await
        .expect_err("should error");

        match err {
            AuthError::Http { status, body, .. } => {
                assert_eq!(status, 401);
                assert!(body.contains("gateway"));
            }
            other => panic!("expected Http, got {other:?}"),
        }
    }
}
