//! Microsoft account authentication for Deepslate.
//!
//! Official Microsoft sign-in only. Nothing in this crate supports
//! unauthenticated accounts or bypasses any entitlement check.
//!
//! The chain is Microsoft OAuth -> Xbox Live -> XSTS -> Minecraft services.
//! Each leg lives in its own module and has its own typed failures; see
//! [`error`] for the full taxonomy.
//!
//! Endpoints are injected ([`endpoints::Endpoints`]) so the whole chain can be
//! exercised against a mock server rather than only against Microsoft.

// Tests assert on exact outcomes, where unwrapping IS the assertion. The
// workspace denies these everywhere else.
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used, clippy::panic))]

pub mod app_id;
pub mod endpoints;
pub mod error;
pub mod loopback;
pub mod minecraft;
pub mod msa;
pub mod pkce;
pub mod store;
pub mod xbox;

pub use endpoints::Endpoints;
pub use error::{AuthError, Result, Stage};
pub use minecraft::{McSession, Profile};
pub use xbox::XboxToken;
