//! Asset indexes and the three on-disk layouts Minecraft has used.
//!
//! Assets are stored content-addressed, exactly like libraries. What differs is
//! how the game expects to *find* them, and there are three answers depending
//! on the version. All three were confirmed against real indexes rather than
//! taken from documentation:
//!
//! | Index | Flag | Where the game looks |
//! |---|---|---|
//! | `29` (modern) | none | the hashed store, via `--assetsDir` |
//! | `legacy` (1.6–1.7) | `"virtual": true` | `assets/virtual/legacy/<path>` |
//! | `pre-1.6` | `"map_to_resources": true` | `<gameDir>/resources/<path>` |
//!
//! The two legacy layouts have to be materialised into real directory trees
//! with human-readable names, because those versions open assets by path. The
//! modern layout needs nothing - the game is handed the store and looks up by
//! hash itself, which is why a modern version costs no extra disk at all.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use ds_net::Artifact;
use ds_store::{Algorithm, Store, StoreError};
use serde::Deserialize;

/// Where asset objects are served from. Content-addressed, like the store.
pub const ASSET_BASE_URL: &str = "https://resources.download.minecraft.net";

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct AssetObject {
    pub hash: String,
    pub size: u64,
}

impl AssetObject {
    /// Objects are served under `<first-2-hex>/<hash>`, the same sharding the
    /// local store uses.
    pub fn url(&self) -> String {
        let shard = self.hash.get(..2).unwrap_or("00");
        format!("{ASSET_BASE_URL}/{shard}/{}", self.hash)
    }

    pub fn artifact(&self) -> Artifact {
        Artifact::sha1(self.url(), self.hash.clone(), self.size)
    }
}

/// How this version expects to find its assets on disk.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Layout {
    /// Modern. The game is pointed at the hashed store and looks up by hash.
    /// Costs no extra disk.
    Hashed,
    /// 1.6–1.7. Materialised under `assets/virtual/<index-id>/`.
    Virtual,
    /// Pre-1.6. Materialised into `<gameDir>/resources/`.
    MapToResources,
}

#[derive(Debug, Clone, Deserialize)]
pub struct AssetIndex {
    pub objects: BTreeMap<String, AssetObject>,

    /// `virtual` is a reserved word in Rust, hence the rename.
    #[serde(rename = "virtual", default)]
    pub is_virtual: bool,

    #[serde(default)]
    pub map_to_resources: bool,
}

impl AssetIndex {
    pub fn parse(json: &str) -> Result<Self, serde_json::Error> {
        serde_json::from_str(json)
    }

    pub fn layout(&self) -> Layout {
        // Checked in this order because an index could in principle carry both,
        // and map_to_resources is the older, more specific instruction.
        if self.map_to_resources {
            Layout::MapToResources
        } else if self.is_virtual {
            Layout::Virtual
        } else {
            Layout::Hashed
        }
    }

    /// Everything that needs downloading.
    ///
    /// Deduplicated by hash: an index maps many paths, and several of them
    /// routinely point at the same object. 1.21.11's index has 4590 entries and
    /// fewer distinct objects, so skipping this would mean re-fetching files we
    /// already hold.
    pub fn artifacts(&self) -> Vec<Artifact> {
        let mut seen = Vec::new();
        let mut out = Vec::new();

        for object in self.objects.values() {
            if seen.contains(&object.hash) {
                continue;
            }
            seen.push(object.hash.clone());
            out.push(object.artifact());
        }
        out
    }

    /// Total bytes of distinct objects, for a progress bar that does not lie.
    pub fn total_size(&self) -> u64 {
        let mut seen = Vec::new();
        let mut total = 0;
        for object in self.objects.values() {
            if seen.contains(&object.hash) {
                continue;
            }
            seen.push(object.hash.clone());
            total += object.size;
        }
        total
    }

    /// Materialise the named tree a legacy version expects.
    ///
    /// A no-op for [`Layout::Hashed`], which is most versions - they read
    /// straight out of the store.
    ///
    /// `target` is `assets/virtual/<index-id>` or `<gameDir>/resources`
    /// depending on the layout; see [`virtual_dir`] and [`resources_dir`].
    pub fn materialise(&self, store: &Store, target: &Path) -> Result<usize, StoreError> {
        if self.layout() == Layout::Hashed {
            return Ok(0);
        }

        let mut written = 0;
        for (name, object) in &self.objects {
            // A path from a downloaded index is untrusted input. Anything that
            // could climb out of the target directory is refused rather than
            // sanitised, because a "fixed" path is still a path someone tried
            // to escape with.
            if !is_safe_relative(name) {
                continue;
            }

            let destination = target.join(name);
            store.materialise(&object.hash, Algorithm::Sha1, &destination)?;
            written += 1;
        }
        Ok(written)
    }
}

/// Reject anything that could escape the directory it is joined onto.
///
/// Absolute paths, drive letters, and `..` components are all refused. These
/// names come out of a JSON document fetched over the network.
fn is_safe_relative(name: &str) -> bool {
    if name.is_empty() || name.starts_with('/') || name.starts_with('\\') {
        return false;
    }
    // A Windows drive prefix such as `C:`.
    if name.chars().nth(1) == Some(':') {
        return false;
    }
    !name
        .split(['/', '\\'])
        .any(|part| part == ".." || part == "." || part.is_empty())
}

/// `assets/virtual/<index-id>` — where a `virtual` index is materialised.
pub fn virtual_dir(assets_root: &Path, index_id: &str) -> PathBuf {
    assets_root.join("virtual").join(index_id)
}

/// `<game-dir>/resources` — where a `map_to_resources` index is materialised.
pub fn resources_dir(game_dir: &Path) -> PathBuf {
    game_dir.join("resources")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Shapes taken from the real indexes: `29` has neither flag, `legacy`
    /// carries `virtual`, `pre-1.6` carries `map_to_resources`.
    const MODERN: &str = r#"{"objects":{
        "icons/icon_16x16.png":{"hash":"5ff04807c356f1beed0b86ccf659b44b9983e3fa","size":781},
        "icons/icon_32x32.png":{"hash":"92750c5f93c312ba9ab413d546f32190c56d6f1f","size":5362}
    }}"#;

    const LEGACY_VIRTUAL: &str = r#"{"virtual":true,"objects":{
        "sound/step/grass1.ogg":{"hash":"5ff04807c356f1beed0b86ccf659b44b9983e3fa","size":781}
    }}"#;

    const PRE_1_6: &str = r#"{"map_to_resources":true,"objects":{
        "READ_ME_I_AM_VERY_IMPORTANT":{"hash":"5ff04807c356f1beed0b86ccf659b44b9983e3fa","size":546}
    }}"#;

    #[test]
    fn the_three_layouts_are_told_apart() {
        assert_eq!(AssetIndex::parse(MODERN).unwrap().layout(), Layout::Hashed);
        assert_eq!(
            AssetIndex::parse(LEGACY_VIRTUAL).unwrap().layout(),
            Layout::Virtual
        );
        assert_eq!(
            AssetIndex::parse(PRE_1_6).unwrap().layout(),
            Layout::MapToResources
        );
    }

    #[test]
    fn objects_are_served_from_the_sharded_asset_host() {
        let index = AssetIndex::parse(MODERN).unwrap();
        let object = &index.objects["icons/icon_16x16.png"];
        assert_eq!(
            object.url(),
            "https://resources.download.minecraft.net/5f/5ff04807c356f1beed0b86ccf659b44b9983e3fa"
        );
    }

    /// Several paths routinely map to the same object; fetching it once is the
    /// difference between 425 MB and rather more.
    #[test]
    fn repeated_objects_are_downloaded_once() {
        let json = r#"{"objects":{
            "a.png":{"hash":"5ff04807c356f1beed0b86ccf659b44b9983e3fa","size":100},
            "b.png":{"hash":"5ff04807c356f1beed0b86ccf659b44b9983e3fa","size":100},
            "c.png":{"hash":"92750c5f93c312ba9ab413d546f32190c56d6f1f","size":50}
        }}"#;
        let index = AssetIndex::parse(json).unwrap();

        assert_eq!(index.objects.len(), 3);
        assert_eq!(index.artifacts().len(), 2, "duplicate object re-fetched");
        assert_eq!(index.total_size(), 150, "duplicate counted twice");
    }

    #[test]
    fn a_modern_index_materialises_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path().join("cache")).unwrap();
        let index = AssetIndex::parse(MODERN).unwrap();

        let written = index.materialise(&store, &dir.path().join("out")).unwrap();
        assert_eq!(written, 0, "the hashed layout needs no copies");
        assert!(!dir.path().join("out").exists());
    }

    #[test]
    fn a_legacy_index_materialises_named_paths() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path().join("cache")).unwrap();
        store
            .insert(
                b"hello",
                "aaf4c61ddcc5e8a2dabede0f3b482cd9aea9434d",
                Algorithm::Sha1,
            )
            .unwrap();

        let json = r#"{"virtual":true,"objects":{
            "sound/step/grass1.ogg":{"hash":"aaf4c61ddcc5e8a2dabede0f3b482cd9aea9434d","size":5}
        }}"#;
        let index = AssetIndex::parse(json).unwrap();

        let target = dir.path().join("virtual/legacy");
        assert_eq!(index.materialise(&store, &target).unwrap(), 1);

        let written = target.join("sound/step/grass1.ogg");
        assert!(written.is_file(), "asset not materialised");
        assert_eq!(std::fs::read(&written).unwrap(), b"hello");
    }

    /// Index paths come out of a document fetched over the network. Anything
    /// that could escape the target directory is refused.
    #[test]
    fn paths_that_escape_the_target_are_refused() {
        for bad in [
            "../evil.txt",
            "a/../../evil.txt",
            "/etc/passwd",
            "\\windows\\system32",
            "C:/windows/evil",
            "a/./b",
            "",
        ] {
            assert!(!is_safe_relative(bad), "'{bad}' should be refused");
        }

        for good in ["icons/icon_16x16.png", "sound/step/grass1.ogg", "a.txt"] {
            assert!(is_safe_relative(good), "'{good}' should be allowed");
        }
    }

    #[test]
    fn a_traversing_entry_is_skipped_not_written() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path().join("cache")).unwrap();
        store
            .insert(
                b"hello",
                "aaf4c61ddcc5e8a2dabede0f3b482cd9aea9434d",
                Algorithm::Sha1,
            )
            .unwrap();

        let json = r#"{"virtual":true,"objects":{
            "../escaped.txt":{"hash":"aaf4c61ddcc5e8a2dabede0f3b482cd9aea9434d","size":5},
            "fine.txt":{"hash":"aaf4c61ddcc5e8a2dabede0f3b482cd9aea9434d","size":5}
        }}"#;
        let index = AssetIndex::parse(json).unwrap();

        let target = dir.path().join("out");
        assert_eq!(index.materialise(&store, &target).unwrap(), 1);
        assert!(target.join("fine.txt").is_file());
        assert!(
            !dir.path().join("escaped.txt").exists(),
            "a traversal wrote outside the target directory"
        );
    }

    #[test]
    fn layout_directories_match_what_the_game_expects() {
        let assets = Path::new("/cache/assets");
        assert_eq!(
            virtual_dir(assets, "legacy"),
            Path::new("/cache/assets/virtual/legacy")
        );

        let game = Path::new("/inst/minecraft");
        assert_eq!(resources_dir(game), Path::new("/inst/minecraft/resources"));
    }
}
