//! Downloading a Java runtime Mojang publishes.
//!
//! Used only when [`crate::java::discover`] finds nothing matching a version's
//! requirement. On a machine that has run the official launcher, that is often
//! never.
//!
//! Two documents: an index keyed by platform and component
//! ([`RUNTIME_INDEX_URL`]), and a per-runtime manifest listing every file. The
//! files are content-addressed by sha1, so they go through the same store,
//! verification and deduplication as everything else - two components sharing a
//! file store it once.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use ds_core::platform::{Arch, Os, Platform};
use ds_net::{Artifact, Downloader, Progress};
use ds_store::{Algorithm, Store};
use serde::Deserialize;

use crate::catalog::CatalogError;

pub const RUNTIME_INDEX_URL: &str =
    "https://launchermeta.mojang.com/v1/products/java-runtime/2ec0cc96c44e5a76b9c8b7c39df7210883d12871/all.json";

type Result<T> = std::result::Result<T, CatalogError>;

/// Mojang's platform key for this machine.
///
/// Returns `None` for a platform Mojang publishes no runtime for, rather than
/// guessing at a near match - a runtime for the wrong architecture fails in a
/// way that looks like a corrupt download.
pub fn platform_key(platform: &Platform) -> Option<&'static str> {
    Some(match (platform.os, platform.arch) {
        (Os::Windows, Arch::X86_64) => "windows-x64",
        (Os::Windows, Arch::X86) => "windows-x86",
        (Os::Windows, Arch::Arm64) => "windows-arm64",
        (Os::Linux, Arch::X86_64) => "linux",
        (Os::Linux, Arch::X86) => "linux-i386",
        (Os::Osx, Arch::X86_64) => "mac-os",
        (Os::Osx, Arch::Arm64) => "mac-os-arm64",
        // Mojang publishes neither of these.
        (Os::Linux, Arch::Arm64) | (Os::Osx, Arch::X86) => return None,
    })
}

#[derive(Debug, Clone, Deserialize)]
pub struct RuntimeRef {
    pub manifest: FileRef,
    pub version: RuntimeVersion,
}

#[derive(Debug, Clone, Deserialize)]
pub struct RuntimeVersion {
    pub name: String,
    #[serde(default)]
    pub released: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct FileRef {
    pub sha1: String,
    pub size: u64,
    pub url: String,
}

/// The index: platform -> component -> candidate runtimes.
#[derive(Debug, Clone, Deserialize)]
pub struct RuntimeIndex(BTreeMap<String, BTreeMap<String, Vec<RuntimeRef>>>);

impl RuntimeIndex {
    pub fn parse(json: &str) -> Result<Self> {
        serde_json::from_str(json).map_err(|source| CatalogError::Parse {
            what: "the Java runtime index".to_owned(),
            source,
        })
    }

    /// The runtime published for a component on a platform.
    ///
    /// The list is usually one entry and occasionally empty - Mojang does not
    /// publish every component for every platform, and an empty list is a
    /// normal answer meaning "not available here", not a malformed document.
    pub fn find(&self, platform_key: &str, component: &str) -> Option<&RuntimeRef> {
        self.0.get(platform_key)?.get(component)?.first()
    }

    pub fn components(&self, platform_key: &str) -> Vec<&str> {
        self.0
            .get(platform_key)
            .map(|components| components.keys().map(String::as_str).collect())
            .unwrap_or_default()
    }
}

/// One entry in a runtime's file manifest.
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum RuntimeEntry {
    Directory,
    File {
        downloads: FileDownloads,
        #[serde(default)]
        executable: bool,
    },
    /// macOS runtimes use symlinks inside the bundle.
    Link {
        target: String,
    },
}

#[derive(Debug, Clone, Deserialize)]
pub struct FileDownloads {
    pub raw: FileRef,
    /// An LZMA-compressed copy. Ignored: it saves bandwidth but costs a
    /// decompressor dependency, and a runtime is downloaded once per major
    /// version per machine.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lzma: Option<FileRef>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct RuntimeManifest {
    pub files: BTreeMap<String, RuntimeEntry>,
}

impl RuntimeManifest {
    pub fn parse(json: &str) -> Result<Self> {
        serde_json::from_str(json).map_err(|source| CatalogError::Parse {
            what: "a Java runtime manifest".to_owned(),
            source,
        })
    }

    /// Every file that needs fetching.
    pub fn artifacts(&self) -> Vec<Artifact> {
        self.files
            .values()
            .filter_map(|entry| match entry {
                RuntimeEntry::File { downloads, .. } => Some(Artifact::sha1(
                    downloads.raw.url.clone(),
                    downloads.raw.sha1.clone(),
                    downloads.raw.size,
                )),
                _ => None,
            })
            .collect()
    }

    pub fn total_size(&self) -> u64 {
        self.files
            .values()
            .filter_map(|entry| match entry {
                RuntimeEntry::File { downloads, .. } => Some(downloads.raw.size),
                _ => None,
            })
            .sum()
    }

    pub fn file_count(&self) -> usize {
        self.files
            .values()
            .filter(|entry| matches!(entry, RuntimeEntry::File { .. }))
            .count()
    }
}

/// Where a managed runtime is installed.
pub fn install_dir(store_root: &Path, component: &str, platform_key: &str) -> PathBuf {
    store_root.join("java").join(platform_key).join(component)
}

/// Whether a manifest path may be written under the install directory.
///
/// Same reasoning as asset and zip entry paths: these come from a document
/// fetched over the network, and a name that escapes the target is refused
/// rather than rewritten.
fn is_safe_relative(name: &str) -> bool {
    !name.is_empty()
        && Path::new(name)
            .components()
            .all(|c| matches!(c, std::path::Component::Normal(_)))
}

/// Download and lay out a Java runtime.
///
/// Returns the path to its home directory - the one that contains `bin` and
/// `release`, which is exactly what [`crate::java::from_home`] reads.
pub async fn install<F>(
    downloader: &Downloader,
    store: &Store,
    component: &str,
    platform: &Platform,
    on_progress: F,
) -> Result<PathBuf>
where
    F: FnMut(Progress),
{
    let key = platform_key(platform).ok_or_else(|| {
        CatalogError::UnknownVersion(format!(
            "Mojang publishes no Java runtime for {} {}",
            platform.os, platform.arch
        ))
    })?;

    let index_json = downloader.fetch_text(RUNTIME_INDEX_URL).await?;
    let index = RuntimeIndex::parse(&index_json)?;

    let runtime = index.find(key, component).ok_or_else(|| {
        CatalogError::UnknownVersion(format!("no '{component}' runtime published for {key}"))
    })?;

    let manifest_artifact = Artifact::sha1(
        runtime.manifest.url.clone(),
        runtime.manifest.sha1.clone(),
        runtime.manifest.size,
    );
    let manifest_path = downloader.fetch(store, &manifest_artifact).await?;
    let manifest_json =
        std::fs::read_to_string(&manifest_path).map_err(|source| CatalogError::Read {
            id: component.to_owned(),
            source,
        })?;
    let manifest = RuntimeManifest::parse(&manifest_json)?;

    downloader
        .fetch_all(store, &manifest.artifacts(), on_progress)
        .await?;

    let home = install_dir(store.root(), component, key);
    lay_out(&manifest, store, &home)?;
    Ok(home)
}

/// Place a downloaded runtime's files where the JVM expects them.
fn lay_out(manifest: &RuntimeManifest, store: &Store, home: &Path) -> Result<()> {
    for (name, entry) in &manifest.files {
        if !is_safe_relative(name) {
            continue;
        }
        let destination = home.join(name);

        match entry {
            RuntimeEntry::Directory => {
                std::fs::create_dir_all(&destination).map_err(|source| CatalogError::Read {
                    id: name.clone(),
                    source,
                })?;
            }
            RuntimeEntry::File {
                downloads,
                executable,
            } => {
                store.materialise(&downloads.raw.sha1, Algorithm::Sha1, &destination)?;
                if *executable {
                    set_executable(&destination);
                }
            }
            RuntimeEntry::Link { target } => {
                // Only macOS runtimes carry these. Copying the target instead of
                // linking would work but doubles the bundle; a failure here is
                // not fatal because the JVM usually still starts.
                let _best_effort = symlink(target, &destination);
            }
        }
    }
    Ok(())
}

#[cfg(unix)]
fn set_executable(path: &Path) {
    use std::os::unix::fs::PermissionsExt as _;
    if let Ok(metadata) = std::fs::metadata(path) {
        let mut permissions = metadata.permissions();
        permissions.set_mode(permissions.mode() | 0o755);
        let _best_effort = std::fs::set_permissions(path, permissions);
    }
}

#[cfg(not(unix))]
fn set_executable(_path: &Path) {
    // Windows has no executable bit; the extension decides.
}

#[cfg(unix)]
fn symlink(target: &str, destination: &Path) -> std::io::Result<()> {
    if let Some(parent) = destination.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let _replace = std::fs::remove_file(destination);
    std::os::unix::fs::symlink(target, destination)
}

#[cfg(not(unix))]
fn symlink(_target: &str, _destination: &Path) -> std::io::Result<()> {
    // Windows symlinks need elevation or developer mode. No Mojang runtime for
    // Windows contains link entries, so this never fires there.
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn platform_keys_match_mojangs_spelling() {
        let win64 = Platform::new(Os::Windows, Arch::X86_64, "10.0");
        assert_eq!(platform_key(&win64), Some("windows-x64"));

        let linux = Platform::new(Os::Linux, Arch::X86_64, "6.8");
        assert_eq!(platform_key(&linux), Some("linux"), "not 'linux-x64'");

        let mac = Platform::new(Os::Osx, Arch::Arm64, "14.5");
        assert_eq!(platform_key(&mac), Some("mac-os-arm64"));
    }

    /// Mojang publishes nothing for Linux on ARM64. Guessing at a near match
    /// would download a runtime for the wrong CPU, which fails looking like a
    /// corrupt download.
    #[test]
    fn an_unpublished_platform_returns_nothing_rather_than_guessing() {
        let linux_arm = Platform::new(Os::Linux, Arch::Arm64, "6.8");
        assert_eq!(platform_key(&linux_arm), None);

        let mac_32 = Platform::new(Os::Osx, Arch::X86, "10.14");
        assert_eq!(platform_key(&mac_32), None);
    }

    /// Shape taken from the real index.
    const INDEX: &str = r#"{
        "windows-x64": {
            "jre-legacy": [{
                "availability": {"group": 4030, "progress": 100},
                "manifest": {"sha1":"0382","size":80031,"url":"https://x/manifest.json"},
                "version": {"name":"8u51-cacert462b08","released":"2025-10-06T13:49:59+00:00"}
            }],
            "java-runtime-delta": [],
            "minecraft-java-exe": [{
                "availability": {"group": 1, "progress": 100},
                "manifest": {"sha1":"abcd","size":100,"url":"https://x/exe.json"},
                "version": {"name":"1.0","released":"2025-01-01T00:00:00+00:00"}
            }]
        },
        "linux": {}
    }"#;

    #[test]
    fn the_index_resolves_a_component_for_a_platform() {
        let index = RuntimeIndex::parse(INDEX).unwrap();
        let found = index.find("windows-x64", "jre-legacy").unwrap();

        assert_eq!(found.version.name, "8u51-cacert462b08");
        assert_eq!(found.manifest.url, "https://x/manifest.json");
    }

    /// An empty list means "not published here", which is a normal answer, not
    /// a malformed document.
    #[test]
    fn a_component_with_no_entries_resolves_to_nothing() {
        let index = RuntimeIndex::parse(INDEX).unwrap();
        assert!(index.find("windows-x64", "java-runtime-delta").is_none());
        assert!(index.find("linux", "jre-legacy").is_none());
        assert!(index.find("solaris", "jre-legacy").is_none());
    }

    #[test]
    fn components_can_be_listed_for_a_platform() {
        let index = RuntimeIndex::parse(INDEX).unwrap();
        let components = index.components("windows-x64");
        assert!(components.contains(&"jre-legacy"));
        assert!(index.components("nowhere").is_empty());
    }

    /// Shape taken from a real runtime manifest: files, directories, and the
    /// symlinks macOS bundles use.
    const MANIFEST: &str = r#"{"files": {
        "bin": {"type":"directory"},
        "bin/java.exe": {
            "type":"file","executable":true,
            "downloads":{
                "raw":{"sha1":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa","size":50000,"url":"https://x/java.exe"},
                "lzma":{"sha1":"bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb","size":20000,"url":"https://x/java.exe.lzma"}
            }
        },
        "release": {
            "type":"file","executable":false,
            "downloads":{"raw":{"sha1":"cccccccccccccccccccccccccccccccccccccccc","size":1200,"url":"https://x/release"}}
        },
        "legal/link": {"type":"link","target":"../other"}
    }}"#;

    #[test]
    fn the_manifest_separates_files_from_directories_and_links() {
        let manifest = RuntimeManifest::parse(MANIFEST).unwrap();

        assert_eq!(manifest.files.len(), 4);
        assert_eq!(manifest.file_count(), 2, "only real files are downloadable");
        assert_eq!(manifest.artifacts().len(), 2);
        assert_eq!(manifest.total_size(), 51_200);
    }

    /// The raw copy is used, not the lzma one: it saves bandwidth but costs a
    /// decompressor dependency for something downloaded once per machine.
    #[test]
    fn artifacts_use_the_raw_download_not_the_compressed_one() {
        let manifest = RuntimeManifest::parse(MANIFEST).unwrap();
        let artifacts = manifest.artifacts();
        let urls: Vec<&str> = artifacts.iter().map(|a| a.url.as_str()).collect();

        assert!(urls.contains(&"https://x/java.exe"));
        assert!(
            !urls.iter().any(|u| u.ends_with(".lzma")),
            "a compressed copy was queued: {urls:?}"
        );
    }

    /// A manifest with no lzma entry at all must still parse.
    #[test]
    fn a_missing_compressed_copy_is_tolerated() {
        let manifest = RuntimeManifest::parse(MANIFEST).unwrap();
        let release = manifest.files.get("release").unwrap();
        match release {
            RuntimeEntry::File { downloads, .. } => assert!(downloads.lzma.is_none()),
            other => panic!("expected a file, got {other:?}"),
        }
    }

    #[test]
    fn the_executable_flag_is_read() {
        let manifest = RuntimeManifest::parse(MANIFEST).unwrap();
        match manifest.files.get("bin/java.exe").unwrap() {
            RuntimeEntry::File { executable, .. } => assert!(*executable),
            other => panic!("expected a file, got {other:?}"),
        }
        match manifest.files.get("release").unwrap() {
            RuntimeEntry::File { executable, .. } => assert!(!*executable),
            other => panic!("expected a file, got {other:?}"),
        }
    }

    /// Manifest paths come from a document fetched over the network.
    #[test]
    fn paths_that_escape_the_install_directory_are_refused() {
        for bad in ["../escaped", "a/../../escaped", "/absolute", ""] {
            assert!(!is_safe_relative(bad), "'{bad}' should be refused");
        }
        for good in ["bin/java.exe", "release", "lib/modules"] {
            assert!(is_safe_relative(good), "'{good}' should be allowed");
        }
    }

    /// The install directory is keyed by platform as well as component, so a
    /// cache copied between machines cannot serve the wrong architecture.
    #[test]
    fn install_paths_are_keyed_by_platform_and_component() {
        let root = Path::new("/cache");
        assert_eq!(
            install_dir(root, "jre-legacy", "windows-x64"),
            Path::new("/cache/java/windows-x64/jre-legacy")
        );
        assert_ne!(
            install_dir(root, "jre-legacy", "windows-x64"),
            install_dir(root, "jre-legacy", "linux")
        );
    }
}
