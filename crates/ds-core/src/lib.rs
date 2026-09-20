//! Pure Minecraft domain logic.
//!
//! **Zero I/O.** Nothing here touches the network, the filesystem or the clock.
//! That is the point: the logic most likely to be subtly wrong - which
//! libraries a version needs, what order the classpath goes in, what the
//! command line expands to - is testable directly, with no mock server and no
//! fixtures on disk.
//!
//! Fetching and caching manifests belongs to `ds-mc`.

// Tests assert on exact outcomes, where unwrapping IS the assertion.
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used, clippy::panic))]

pub mod args;
pub mod classpath;
pub mod platform;
pub mod rules;
pub mod version;

pub use args::{Argument, Substitutions};
pub use classpath::{separator, Coordinate};
pub use platform::{Arch, Os, Platform};
pub use rules::{evaluate, Action, Features, Rule};
pub use version::{JavaVersion, Library, VersionManifest};
