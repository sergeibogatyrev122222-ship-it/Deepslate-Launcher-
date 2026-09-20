//! The Azure application identity this launcher signs in as.
//!
//! **None of this is secret.** A public client ships no password at all - it
//! cannot, because anyone can read the binary. The security comes from PKCE
//! (see [`crate::pkce`]): a per-attempt secret that never leaves the machine
//! and is never sent to the browser. So the client ID is committed
//! deliberately, exactly as every other open-source launcher does.
//!
//! Overridable at runtime purely so a contributor can point a build at their
//! own registration without editing source.

/// Application (client) ID of the "DeepSlate Launcher" Azure registration.
pub const DEFAULT_CLIENT_ID: &str = "046bf82e-b3bf-4a42-866d-4c3a772de68a";

/// Directory (tenant) ID the registration lives in.
///
/// Not used when signing in - sign-in goes through the `consumers` tenant,
/// because Minecraft accounts are personal Microsoft accounts. Recorded here
/// because Mojang's app review form asks for it.
pub const TENANT_ID: &str = "43686ab0-0da4-421a-a22d-9d60e22ea89c";

/// Environment variable that overrides the built-in client ID.
pub const CLIENT_ID_ENV: &str = "DEEPSLATE_CLIENT_ID";

/// The client ID to sign in with.
///
/// Falls back to the built-in registration. An empty override is ignored
/// rather than honoured, since an empty client ID produces a confusing failure
/// deep inside the OAuth exchange rather than at the point of the mistake.
pub fn client_id() -> String {
    match std::env::var(CLIENT_ID_ENV) {
        Ok(value) if !value.trim().is_empty() => value.trim().to_owned(),
        _ => DEFAULT_CLIENT_ID.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn is_guid(value: &str) -> bool {
        let parts: Vec<&str> = value.split('-').collect();
        parts.len() == 5
            && [8, 4, 4, 4, 12]
                .iter()
                .zip(&parts)
                .all(|(len, part)| part.len() == *len)
            && value.chars().all(|c| c.is_ascii_hexdigit() || c == '-')
    }

    #[test]
    fn built_in_ids_are_well_formed_guids() {
        assert!(is_guid(DEFAULT_CLIENT_ID), "{DEFAULT_CLIENT_ID}");
        assert!(is_guid(TENANT_ID), "{TENANT_ID}");
    }

    /// A blank override is a mistake, not an instruction. Honouring it would
    /// surface as an opaque OAuth error much later.
    #[test]
    fn blank_override_falls_back_to_the_built_in_id() {
        // SAFETY-ish: this test owns the variable and restores nothing, so it
        // must not run in parallel with a test that reads a different value.
        // There is only one such test, and it asserts the same fallback.
        std::env::set_var(CLIENT_ID_ENV, "   ");
        assert_eq!(client_id(), DEFAULT_CLIENT_ID);
        std::env::remove_var(CLIENT_ID_ENV);
    }
}
