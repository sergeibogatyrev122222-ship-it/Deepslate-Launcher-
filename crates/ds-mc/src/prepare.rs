//! Turning a resolved version into files on disk.
//!
//! Collects everything a version needs - client jar, libraries, natives, asset
//! index, asset objects - fetches whatever is missing, and reports where it
//! all landed.
//!
//! Classpath entries point **into the content-addressed store**, not into the
//! instance. Libraries are immutable and hash-verified, so ten instances on the
//! same version share one copy of every jar and the instance directory holds
//! only what the game writes.

use std::path::PathBuf;

use ds_core::platform::Platform;
use ds_core::rules::Features;
use ds_core::version::VersionManifest;
use ds_net::{Artifact, Downloader, Progress};
use ds_store::{Algorithm, Store};

use crate::assets::{self, AssetIndex, Layout};
use crate::catalog::CatalogError;
use crate::instance::Instance;

type Result<T> = std::result::Result<T, CatalogError>;

/// Everything a version needs, and where it is.
#[derive(Debug)]
pub struct Prepared {
    pub manifest: VersionManifest,
    pub asset_index: Option<AssetIndex>,
    pub asset_index_id: Option<String>,
    /// Absolute paths into the store, in classpath order, client jar last.
    pub classpath: Vec<PathBuf>,
    /// Legacy natives jars needing extraction. Empty for modern versions.
    pub native_jars: Vec<PathBuf>,
    pub bytes_fetched: u64,
}

/// What still needs downloading, before anything is fetched.
#[derive(Debug, Default)]
pub struct Plan {
    pub libraries: Vec<Artifact>,
    pub natives: Vec<Artifact>,
    pub client: Option<Artifact>,
    pub assets: Vec<Artifact>,
}

impl Plan {
    pub fn total_files(&self) -> usize {
        self.libraries.len()
            + self.natives.len()
            + self.assets.len()
            + usize::from(self.client.is_some())
    }

    pub fn total_bytes(&self) -> u64 {
        let sum = |list: &[Artifact]| list.iter().map(|a| a.size).sum::<u64>();
        sum(&self.libraries)
            + sum(&self.natives)
            + sum(&self.assets)
            + self.client.as_ref().map_or(0, |c| c.size)
    }
}

/// Everything the version needs, whether or not it is already present.
///
/// Separate from fetching so the UI can show a real total before anything
/// starts, rather than a bar that grows as it discovers more work.
pub fn plan(manifest: &VersionManifest, platform: &Platform, features: &Features) -> Plan {
    let mut plan = Plan {
        client: manifest
            .client_download()
            .map(|d| Artifact::sha1(d.url.clone(), d.sha1.clone(), d.size)),
        ..Plan::default()
    };

    for library in manifest.applicable_libraries(platform, features) {
        if let Some(artifact) = &library.downloads.artifact {
            plan.libraries.push(Artifact::sha1(
                artifact.url.clone(),
                artifact.sha1.clone(),
                artifact.size,
            ));
        }

        // Pre-1.19 versions carry natives in a separate classified jar that is
        // extracted rather than put on the classpath.
        if let Some(native) = library.native_download(platform) {
            plan.natives.push(Artifact::sha1(
                native.url.clone(),
                native.sha1.clone(),
                native.size,
            ));
        }
    }

    plan
}

/// Fetch everything a version needs.
///
/// Progress is reported per file as it lands; coalescing for a UI happens at
/// the app boundary.
pub async fn prepare<F>(
    downloader: &Downloader,
    store: &Store,
    manifest: VersionManifest,
    platform: &Platform,
    features: &Features,
    mut on_progress: F,
) -> Result<Prepared>
where
    F: FnMut(Progress),
{
    // The asset index is fetched first: it is one small file that determines
    // several thousand more, so knowing the real total before starting is worth
    // one round trip.
    let (asset_index, asset_index_id) = match &manifest.asset_index {
        Some(reference) => {
            let artifact = Artifact::sha1(
                reference.url.clone(),
                reference.sha1.clone(),
                reference.size,
            );
            let path = downloader.fetch(store, &artifact).await?;
            let json = std::fs::read_to_string(&path).map_err(|source| CatalogError::Read {
                id: reference.id.clone(),
                source,
            })?;
            let index = AssetIndex::parse(&json).map_err(|source| CatalogError::Parse {
                what: format!("asset index '{}'", reference.id),
                source,
            })?;
            (Some(index), Some(reference.id.clone()))
        }
        None => (None, None),
    };

    let mut work = plan(&manifest, platform, features);
    if let Some(index) = &asset_index {
        work.assets = index.artifacts();
    }

    // One batch so concurrency is shared across every kind of file rather than
    // being serialised into phases that each fail to saturate the link.
    let mut everything = Vec::with_capacity(work.total_files());
    everything.extend(work.libraries.iter().cloned());
    everything.extend(work.natives.iter().cloned());
    if let Some(client) = &work.client {
        everything.push(client.clone());
    }
    everything.extend(work.assets.iter().cloned());

    let bytes_fetched = work.total_bytes();
    downloader
        .fetch_all(store, &everything, &mut on_progress)
        .await?;

    // Classpath points into the store. Libraries are immutable and verified, so
    // instances share them rather than each holding a copy.
    let mut classpath = Vec::with_capacity(work.libraries.len() + 1);
    for artifact in &work.libraries {
        classpath.push(store.path_for(&artifact.hash, Algorithm::Sha1)?);
    }
    if let Some(client) = &work.client {
        classpath.push(store.path_for(&client.hash, Algorithm::Sha1)?);
    }

    let mut native_jars = Vec::with_capacity(work.natives.len());
    for artifact in &work.natives {
        native_jars.push(store.path_for(&artifact.hash, Algorithm::Sha1)?);
    }

    Ok(Prepared {
        manifest,
        asset_index,
        asset_index_id,
        classpath,
        native_jars,
        bytes_fetched,
    })
}

/// What staging an instance actually did.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Staged {
    pub natives_written: usize,
    pub assets_materialised: usize,
}

/// Put everything in place inside an instance.
///
/// Two jobs, both no-ops for a modern version:
///
/// - **Natives** are unpacked into the instance's own directory. Pre-1.19 only;
///   newer versions ship natives as ordinary libraries.
/// - **Legacy assets** are materialised into a named tree, because `legacy` and
///   `pre-1.6` versions open assets by path rather than by hash. Modern
///   versions read the shared store directly and cost no extra disk.
pub fn stage(
    prepared: &Prepared,
    instance: &Instance,
    store: &Store,
    platform: &Platform,
    features: &Features,
) -> Result<Staged> {
    let mut staged = Staged::default();

    if !prepared.native_jars.is_empty() {
        // Exclusions are per-library, but in practice every natives jar in a
        // version lists the same ones (META-INF/). Collecting them into one set
        // avoids re-opening each jar once per library.
        let mut exclude: Vec<String> = Vec::new();
        for library in prepared.manifest.applicable_libraries(platform, features) {
            if let Some(extract) = &library.extract {
                for pattern in &extract.exclude {
                    if !exclude.contains(pattern) {
                        exclude.push(pattern.clone());
                    }
                }
            }
        }

        staged.natives_written =
            crate::natives::extract_all(&prepared.native_jars, &instance.natives_dir(), &exclude)
                .map_err(|error| CatalogError::Read {
                id: "natives".to_owned(),
                source: std::io::Error::other(error.to_string()),
            })?;
    }

    if let (Some(index), Some(index_id)) = (&prepared.asset_index, &prepared.asset_index_id) {
        let assets_root = store.root().join("assets");
        let target = match index.layout() {
            Layout::Hashed => None,
            Layout::Virtual => Some(assets::virtual_dir(&assets_root, index_id)),
            Layout::MapToResources => Some(assets::resources_dir(&instance.game_dir())),
        };

        if let Some(target) = target {
            staged.assets_materialised = index.materialise(store, &target)?;
        }
    }

    Ok(staged)
}

/// Where the game should be told to look for assets.
///
/// A `virtual` version is pointed at its materialised tree rather than the
/// hashed store; `map_to_resources` reads from the game directory and the value
/// here is unused; everything modern uses the store.
pub fn assets_dir_for(prepared: &Prepared, store: &Store) -> PathBuf {
    let assets_root = store.root().join("assets");

    match (&prepared.asset_index, &prepared.asset_index_id) {
        (Some(index), Some(index_id)) if index.layout() == Layout::Virtual => {
            assets::virtual_dir(&assets_root, index_id)
        }
        _ => assets_root,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ds_core::platform::{Arch, Os};

    fn windows() -> Platform {
        Platform::new(Os::Windows, Arch::X86_64, "10.0")
    }

    fn manifest(json: &str) -> VersionManifest {
        VersionManifest::parse(json).expect("test manifest should parse")
    }

    const SHA_A: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    const SHA_B: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
    const SHA_C: &str = "cccccccccccccccccccccccccccccccccccccccc";

    #[test]
    fn the_plan_covers_client_and_libraries() {
        let m = manifest(&format!(
            r#"{{"id":"t","type":"release",
                "downloads":{{"client":{{"sha1":"{SHA_A}","size":1000,"url":"http://x/client.jar"}}}},
                "libraries":[
                    {{"name":"a:b:1","downloads":{{"artifact":{{"path":"a/b.jar","sha1":"{SHA_B}","size":10,"url":"http://x/b.jar"}}}}}}
                ]}}"#
        ));

        let plan = plan(&m, &windows(), &Features::new());
        assert_eq!(plan.libraries.len(), 1);
        assert!(plan.client.is_some());
        assert_eq!(plan.total_files(), 2);
        assert_eq!(plan.total_bytes(), 1010);
    }

    /// A library excluded by rules must not be downloaded, not merely left off
    /// the classpath.
    #[test]
    fn excluded_libraries_are_never_fetched() {
        let m = manifest(&format!(
            r#"{{"id":"t","type":"release","libraries":[
                {{"name":"mac:only:1","rules":[{{"action":"allow","os":{{"name":"osx"}}}}],
                 "downloads":{{"artifact":{{"path":"m.jar","sha1":"{SHA_A}","size":10,"url":"http://x/m.jar"}}}}}}
            ]}}"#
        ));

        let plan = plan(&m, &windows(), &Features::new());
        assert!(
            plan.libraries.is_empty(),
            "a macOS library was planned on Windows"
        );
        assert_eq!(plan.total_bytes(), 0);
    }

    /// Pre-1.19 natives are planned separately: they are extracted, not put on
    /// the classpath.
    #[test]
    fn legacy_natives_are_planned_apart_from_libraries() {
        let m = manifest(&format!(
            r#"{{"id":"t","type":"release","libraries":[{{
                "name":"org.lwjgl:lwjgl-platform:2.9.4",
                "natives":{{"windows":"natives-windows-${{arch}}"}},
                "downloads":{{"classifiers":{{
                    "natives-windows-64":{{"sha1":"{SHA_C}","size":500,"url":"http://x/n.jar"}}
                }}}}
            }}]}}"#
        ));

        let plan = plan(&m, &windows(), &Features::new());
        assert!(
            plan.libraries.is_empty(),
            "a natives jar reached the classpath plan"
        );
        assert_eq!(plan.natives.len(), 1);
        assert_eq!(plan.total_bytes(), 500);
    }

    /// The whole total has to be known before the first byte, so a progress bar
    /// does not grow while it runs.
    #[test]
    fn totals_are_known_before_anything_is_fetched() {
        let mut plan = Plan::default();
        plan.libraries
            .push(Artifact::sha1("http://x/a", SHA_A, 100));
        plan.assets.push(Artifact::sha1("http://x/b", SHA_B, 250));
        plan.client = Some(Artifact::sha1("http://x/c", SHA_C, 1000));

        assert_eq!(plan.total_files(), 3);
        assert_eq!(plan.total_bytes(), 1350);
    }
}
