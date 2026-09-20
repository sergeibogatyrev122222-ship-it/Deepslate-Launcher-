//! The URLs the sign-in chain talks to.
//!
//! Collected in one injectable struct for a single reason: tests point them at
//! a local mock server. Without this the chain could only be exercised against
//! Microsoft's real infrastructure, which means it could not be tested at all.

/// Endpoints for every leg of the chain.
#[derive(Debug, Clone)]
pub struct Endpoints {
    pub msa_authorize: String,
    pub msa_token: String,
    pub xbl_authenticate: String,
    pub xsts_authorize: String,
    pub mc_login: String,
    pub mc_profile: String,
    pub mc_entitlements: String,
}

impl Default for Endpoints {
    fn default() -> Self {
        Self::production()
    }
}

impl Endpoints {
    /// The real Microsoft and Mojang endpoints.
    ///
    /// `consumers` rather than `common` is required: Minecraft accounts are
    /// personal Microsoft accounts, and the `XboxLive.signin` scope is only
    /// granted through the consumers tenant.
    pub fn production() -> Self {
        Self {
            msa_authorize: "https://login.microsoftonline.com/consumers/oauth2/v2.0/authorize"
                .to_owned(),
            msa_token: "https://login.microsoftonline.com/consumers/oauth2/v2.0/token".to_owned(),
            xbl_authenticate: "https://user.auth.xboxlive.com/user/authenticate".to_owned(),
            xsts_authorize: "https://xsts.auth.xboxlive.com/xsts/authorize".to_owned(),
            mc_login: "https://api.minecraftservices.com/authentication/login_with_xbox".to_owned(),
            mc_profile: "https://api.minecraftservices.com/minecraft/profile".to_owned(),
            mc_entitlements: "https://api.minecraftservices.com/entitlements/mcstore".to_owned(),
        }
    }

    /// Point every endpoint at one mock server root.
    #[cfg(test)]
    pub fn mocked(base: &str) -> Self {
        let b = base.trim_end_matches('/');
        Self {
            msa_authorize: format!("{b}/oauth2/authorize"),
            msa_token: format!("{b}/oauth2/token"),
            xbl_authenticate: format!("{b}/user/authenticate"),
            xsts_authorize: format!("{b}/xsts/authorize"),
            mc_login: format!("{b}/authentication/login_with_xbox"),
            mc_profile: format!("{b}/minecraft/profile"),
            mc_entitlements: format!("{b}/entitlements/mcstore"),
        }
    }
}

/// The only OAuth scope this launcher requests.
///
/// `XboxLive.signin` is what Minecraft sign-in requires; `offline_access` is
/// what makes a refresh token come back, so the user is not asked to sign in on
/// every launch. Nothing else is requested - a launcher has no business holding
/// broader consent than it needs.
pub const SCOPE: &str = "XboxLive.signin offline_access";

/// The relying party for the XSTS exchange. Anything else yields a token
/// Minecraft will not accept.
pub const MC_RELYING_PARTY: &str = "rp://api.minecraftservices.com/";

/// The relying party for the initial Xbox Live user token.
pub const XBL_RELYING_PARTY: &str = "http://auth.xboxlive.com";
