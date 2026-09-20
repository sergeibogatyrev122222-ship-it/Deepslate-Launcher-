//! Typed failures for the Microsoft sign-in chain.
//!
//! Every variant carries a message a user can act on. "Authentication failed"
//! is not an acceptable outcome anywhere in this crate: the whole point of the
//! taxonomy is that a person reading the message knows what to do next.

use std::fmt;

/// Which leg of the chain a transport failure happened on.
///
/// Carried on [`AuthError::Transport`] so a network blip reports *where* it
/// broke rather than just "network error", which matters because the remedies
/// differ - an Xbox outage is not the same as Minecraft services being down.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stage {
    /// Exchanging the authorization code, or refreshing, at login.live.com.
    Microsoft,
    /// `user.authenticate` at user.auth.xboxlive.com.
    XboxLive,
    /// `xsts.authorize` at xsts.auth.xboxlive.com.
    Xsts,
    /// `login_with_xbox` at api.minecraftservices.com.
    MinecraftLogin,
    /// The entitlement or profile lookup.
    MinecraftProfile,
}

impl fmt::Display for Stage {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let name = match self {
            Self::Microsoft => "Microsoft sign-in",
            Self::XboxLive => "Xbox Live authentication",
            Self::Xsts => "Xbox security token exchange",
            Self::MinecraftLogin => "Minecraft sign-in",
            Self::MinecraftProfile => "Minecraft profile lookup",
        };
        f.write_str(name)
    }
}

/// XSTS reports why it refused through an `XErr` number in the response body.
///
/// Only a handful are documented. The rest are deliberately *not* collapsed
/// into a generic failure - see [`AuthError::XstsUnknown`].
pub mod xerr {
    /// The Microsoft account has no associated Xbox profile.
    pub const NO_XBOX_ACCOUNT: u64 = 2_148_916_233;
    /// Region does not permit Xbox Live.
    pub const REGION_UNAVAILABLE: u64 = 2_148_916_235;
    /// Adult verification required (South Korea).
    pub const ADULT_VERIFICATION_REQUIRED: u64 = 2_148_916_236;
    /// Adult verification required (South Korea), alternate code.
    pub const ADULT_VERIFICATION_REQUIRED_ALT: u64 = 2_148_916_237;
    /// Account is a child and must be added to a Microsoft Family by an adult.
    pub const CHILD_ACCOUNT: u64 = 2_148_916_238;
}

#[derive(Debug, thiserror::Error)]
pub enum AuthError {
    // ---- Xbox Live / XSTS refusals -------------------------------------
    #[error(
        "this Microsoft account has no Xbox profile; create one at xbox.com and sign in again"
    )]
    NoXboxAccount,

    #[error(
        "this is a child account; an adult must add it to a Microsoft Family before it can sign in"
    )]
    ChildAccount,

    #[error("Xbox Live is not available in this account's country or region")]
    RegionUnavailable,

    #[error("this account needs adult verification before it can sign in")]
    AdultVerificationRequired,

    /// Anything XSTS returned that is not in [`xerr`].
    ///
    /// Deliberately not folded into a generic error. The documented codes came
    /// from documentation rather than from responses observed in the wild, so a
    /// closed set would silently swallow whatever Microsoft returns next.
    /// Surfacing the raw number keeps it actionable - it can be searched, and it
    /// tells us which variant to add.
    #[error("Xbox sign-in was refused with code {xerr} (this code is not one we recognise)")]
    XstsUnknown { xerr: u64 },

    // ---- Minecraft services --------------------------------------------
    /// HTTP 403 from `login_with_xbox`, i.e. "Invalid app registration".
    ///
    /// This is the expected state for a launcher whose Azure application has not
    /// yet been approved for the Minecraft API, so it gets its own variant and a
    /// message that says so rather than looking like a user problem.
    #[error(
        "this build is not yet approved to use Minecraft sign-in \
         (Microsoft returned 'invalid app registration'); approval is requested at \
         https://aka.ms/mce-reviewappid"
    )]
    AppNotApproved,

    #[error("this account does not own Minecraft: Java Edition")]
    NotEntitled {
        /// Entitlement response, when one was available, to distinguish a genuine
        /// non-owner from a Game Pass subscriber whose entitlements read empty.
        detail: Option<String>,
    },

    #[error(
        "this account owns Minecraft but has not chosen a username yet; \
         set one up at minecraft.net and sign in again"
    )]
    NoProfile,

    // ---- Session lifecycle ---------------------------------------------
    #[error("the saved sign-in has expired; signing in again is required")]
    RefreshExpired,

    #[error("sign-in was cancelled")]
    Cancelled,

    #[error("sign-in timed out waiting for the browser")]
    TimedOut,

    // ---- Transport and protocol ----------------------------------------
    #[error("network error during {stage}")]
    Transport {
        stage: Stage,
        #[source]
        source: reqwest::Error,
    },

    /// A response arrived but did not look like what the endpoint documents.
    #[error("unexpected response during {stage}: {detail}")]
    Protocol { stage: Stage, detail: String },

    #[error("{stage} returned HTTP {status}")]
    Http {
        stage: Stage,
        status: u16,
        body: String,
    },

    // ---- Local -----------------------------------------------------------
    #[error("could not store the sign-in securely in the system keychain")]
    Keychain {
        #[source]
        source: keyring::Error,
    },

    #[error("could not start a local listener for the sign-in redirect")]
    Loopback {
        #[source]
        source: std::io::Error,
    },
}

impl AuthError {
    /// Map an XSTS `XErr` to its variant, preserving unknown codes verbatim.
    pub fn from_xerr(xerr: u64) -> Self {
        match xerr {
            xerr::NO_XBOX_ACCOUNT => Self::NoXboxAccount,
            xerr::CHILD_ACCOUNT => Self::ChildAccount,
            xerr::REGION_UNAVAILABLE => Self::RegionUnavailable,
            xerr::ADULT_VERIFICATION_REQUIRED | xerr::ADULT_VERIFICATION_REQUIRED_ALT => {
                Self::AdultVerificationRequired
            }
            other => Self::XstsUnknown { xerr: other },
        }
    }

    /// Whether retrying the same operation could plausibly succeed.
    ///
    /// Drives whether the UI offers a retry button. Deliberately conservative:
    /// offering retry on something permanent trains people to mash it.
    pub fn is_retryable(&self) -> bool {
        match self {
            Self::Transport { .. } | Self::TimedOut => true,
            Self::Http { status, .. } => *status >= 500 || *status == 429,

            Self::NoXboxAccount
            | Self::ChildAccount
            | Self::RegionUnavailable
            | Self::AdultVerificationRequired
            | Self::XstsUnknown { .. }
            | Self::AppNotApproved
            | Self::NotEntitled { .. }
            | Self::NoProfile
            | Self::RefreshExpired
            | Self::Cancelled
            | Self::Protocol { .. }
            | Self::Keychain { .. }
            | Self::Loopback { .. } => false,
        }
    }

    /// Whether the user must go through interactive sign-in again.
    pub fn needs_reauth(&self) -> bool {
        matches!(self, Self::RefreshExpired)
    }
}

pub type Result<T> = std::result::Result<T, AuthError>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn documented_xerrs_map_to_named_variants() {
        assert!(matches!(
            AuthError::from_xerr(xerr::NO_XBOX_ACCOUNT),
            AuthError::NoXboxAccount
        ));
        assert!(matches!(
            AuthError::from_xerr(xerr::CHILD_ACCOUNT),
            AuthError::ChildAccount
        ));
        assert!(matches!(
            AuthError::from_xerr(xerr::REGION_UNAVAILABLE),
            AuthError::RegionUnavailable
        ));
        assert!(matches!(
            AuthError::from_xerr(xerr::ADULT_VERIFICATION_REQUIRED),
            AuthError::AdultVerificationRequired
        ));
        assert!(matches!(
            AuthError::from_xerr(xerr::ADULT_VERIFICATION_REQUIRED_ALT),
            AuthError::AdultVerificationRequired
        ));
    }

    /// The whole point of the catch-all: an unrecognised code must survive
    /// intact so it can be searched and reported, not be flattened away.
    #[test]
    fn unknown_xerr_is_preserved_verbatim() {
        let err = AuthError::from_xerr(9_999_999);
        match err {
            AuthError::XstsUnknown { xerr } => assert_eq!(xerr, 9_999_999),
            other => panic!("expected XstsUnknown, got {other:?}"),
        }
        assert!(AuthError::from_xerr(9_999_999)
            .to_string()
            .contains("9999999"));
    }

    #[test]
    fn permanent_failures_are_not_offered_a_retry() {
        for err in [
            AuthError::NoXboxAccount,
            AuthError::ChildAccount,
            AuthError::AppNotApproved,
            AuthError::NoProfile,
            AuthError::NotEntitled { detail: None },
            AuthError::RefreshExpired,
        ] {
            assert!(!err.is_retryable(), "{err} should not be retryable");
        }
    }

    #[test]
    fn server_errors_and_rate_limits_are_retryable() {
        for status in [500, 502, 503, 429] {
            let err = AuthError::Http {
                stage: Stage::Xsts,
                status,
                body: String::new(),
            };
            assert!(err.is_retryable(), "HTTP {status} should be retryable");
        }
        let err = AuthError::Http {
            stage: Stage::Xsts,
            status: 400,
            body: String::new(),
        };
        assert!(!err.is_retryable(), "HTTP 400 should not be retryable");
    }

    #[test]
    fn only_expired_refresh_demands_interactive_signin() {
        assert!(AuthError::RefreshExpired.needs_reauth());
        assert!(!AuthError::NoProfile.needs_reauth());
        assert!(!AuthError::AppNotApproved.needs_reauth());
    }
}
