//! Proof Key for Code Exchange (RFC 7636).
//!
//! A desktop app cannot keep a client secret - anyone can read it out of the
//! binary - so the authorization code flow is secured with PKCE instead. We
//! send a hash of a random secret up front, then the secret itself when
//! redeeming the code. An attacker who intercepts the redirect gets a code they
//! cannot spend.
//!
//! Pure: no I/O, no clock, so every property below is testable directly.

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine as _;
use rand::Rng as _;
use sha2::{Digest as _, Sha256};

/// A verifier/challenge pair for one sign-in attempt. Never reused.
#[derive(Debug, Clone)]
pub struct Pkce {
    verifier: String,
    challenge: String,
}

impl Pkce {
    /// Generate a fresh pair from 32 bytes of OS entropy.
    ///
    /// 32 bytes base64url-encodes to 43 characters, which is the minimum length
    /// RFC 7636 permits and comfortably above the 256 bits of entropy it asks
    /// for.
    pub fn generate() -> Self {
        let mut bytes = [0u8; 32];
        rand::rng().fill_bytes(&mut bytes);
        Self::from_entropy(&bytes)
    }

    /// Build from caller-supplied entropy.
    ///
    /// Exists so tests can pin the input and assert the exact challenge, rather
    /// than only checking shape.
    fn from_entropy(bytes: &[u8]) -> Self {
        let verifier = URL_SAFE_NO_PAD.encode(bytes);
        let digest = Sha256::digest(verifier.as_bytes());
        let challenge = URL_SAFE_NO_PAD.encode(digest);
        Self {
            verifier,
            challenge,
        }
    }

    /// Sent only when redeeming the authorization code.
    pub fn verifier(&self) -> &str {
        &self.verifier
    }

    /// Sent in the authorization request, as `code_challenge`.
    pub fn challenge(&self) -> &str {
        &self.challenge
    }

    /// Always `S256`. The spec also allows `plain`, which defeats the purpose.
    pub const fn method(&self) -> &'static str {
        "S256"
    }
}

/// An opaque value echoed back through the redirect to prove the response
/// belongs to the request we started. Guards against a forged callback.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct State(String);

impl State {
    pub fn generate() -> Self {
        let mut bytes = [0u8; 16];
        rand::rng().fill_bytes(&mut bytes);
        Self(URL_SAFE_NO_PAD.encode(bytes))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Compare against a value that came back from the browser.
    pub fn matches(&self, candidate: &str) -> bool {
        // Not constant-time on purpose: `state` is a public CSRF nonce, not a
        // secret, and a timing side channel on it reveals nothing useful.
        self.0 == candidate
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The worked example from RFC 7636 appendix B. If this passes, our
    /// encoding and hashing match what the spec - and therefore Microsoft -
    /// expects.
    #[test]
    fn matches_rfc7636_appendix_b_vector() {
        let entropy: [u8; 32] = [
            116, 24, 223, 180, 151, 153, 224, 37, 79, 250, 96, 125, 216, 173, 187, 186, 22, 212,
            37, 77, 105, 214, 191, 240, 91, 88, 5, 88, 83, 132, 141, 121,
        ];
        let pkce = Pkce::from_entropy(&entropy);

        assert_eq!(
            pkce.verifier(),
            "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk"
        );
        assert_eq!(
            pkce.challenge(),
            "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM"
        );
    }

    #[test]
    fn verifier_length_is_within_rfc_bounds() {
        let pkce = Pkce::generate();
        let len = pkce.verifier().len();
        assert!((43..=128).contains(&len), "verifier was {len} chars");
    }

    #[test]
    fn verifier_uses_only_unreserved_characters() {
        let pkce = Pkce::generate();
        assert!(
            pkce.verifier()
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '.' | '_' | '~')),
            "verifier contained a character outside the RFC 7636 unreserved set"
        );
    }

    #[test]
    fn each_attempt_gets_fresh_values() {
        let a = Pkce::generate();
        let b = Pkce::generate();
        assert_ne!(a.verifier(), b.verifier());
        assert_ne!(a.challenge(), b.challenge());
        assert_ne!(State::generate(), State::generate());
    }

    #[test]
    fn challenge_never_leaks_the_verifier() {
        let pkce = Pkce::generate();
        assert_ne!(pkce.verifier(), pkce.challenge());
        assert!(!pkce.challenge().contains(pkce.verifier()));
    }

    #[test]
    fn state_rejects_anything_it_did_not_issue() {
        let state = State::generate();
        assert!(state.matches(state.as_str()));
        assert!(!state.matches("forged"));
        assert!(!state.matches(""));
        assert!(!state.matches(&format!("{} ", state.as_str())));
    }
}
