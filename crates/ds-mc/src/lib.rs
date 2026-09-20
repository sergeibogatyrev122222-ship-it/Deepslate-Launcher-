//! The Minecraft side of version resolution: the catalogue, inheritance, and
//! turning a resolved version into a list of files to fetch.
//!
//! The pure logic it builds on lives in `ds-core`; fetching and storage are
//! `ds-net` and `ds-store`. This crate is the part that knows Minecraft.

#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used, clippy::panic))]

pub mod assets;
pub mod catalog;
pub mod inherit;
pub mod prepare;

pub use assets::{AssetIndex, Layout};
pub use catalog::{Catalog, CatalogError, VersionEntry, VersionList};
pub use inherit::{merge, resolve, InheritError, ResolveError};
pub use prepare::{plan, prepare, Plan, Prepared};
