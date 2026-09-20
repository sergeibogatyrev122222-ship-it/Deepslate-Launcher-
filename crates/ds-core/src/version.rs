//! The version manifest model.
//!
//! Deserialises Mojang's per-version JSON. The shapes here were taken from real
//! manifests rather than from documentation, because the documentation is
//! incomplete and the format has drifted across fifteen years.
//!
//! Notably, modern versions (checked against 1.21.11) carry **no** `natives` or
//! `classifiers` keys at all - native libraries are ordinary entries gated by an
//! OS rule. The `natives`/`classifiers`/`extract` mechanism only appears on
//! pre-1.19 manifests, so all three are optional and absence is normal.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::args::Argument;
use crate::platform::Platform;
use crate::rules::{evaluate, Features, Rule};

/// A downloadable file, with the hash to verify it against.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Download {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    pub sha1: String,
    pub size: u64,
    pub url: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct LibraryDownloads {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub artifact: Option<Download>,
    /// Pre-1.19 only: per-platform native jars, keyed by classifier.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub classifiers: Option<BTreeMap<String, Download>>,
}

/// Paths to skip when unpacking a native jar. Pre-1.19 only.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Extract {
    #[serde(default)]
    pub exclude: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Library {
    /// Maven coordinates, `group:artifact:version[:classifier]`.
    pub name: String,
    #[serde(default)]
    pub downloads: LibraryDownloads,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub rules: Vec<Rule>,
    /// Pre-1.19 only: maps an OS to the classifier holding its natives.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub natives: Option<BTreeMap<String, String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub extract: Option<Extract>,
}

impl Library {
    /// Whether this library applies on the given platform.
    pub fn applies_to(&self, platform: &Platform, features: &Features) -> bool {
        evaluate(&self.rules, platform, features)
    }

    /// The native classifier for this platform, if this is a legacy natives
    /// library.
    ///
    /// Expands `${arch}` to the bit width, not the architecture name:
    /// `natives-windows-${arch}` becomes `natives-windows-64`.
    pub fn native_classifier(&self, platform: &Platform) -> Option<String> {
        let natives = self.natives.as_ref()?;
        let template = natives.get(platform.os.manifest_name())?;
        Some(template.replace("${arch}", platform.arch.bits()))
    }

    /// The native jar for this platform, if there is one.
    pub fn native_download(&self, platform: &Platform) -> Option<&Download> {
        let classifier = self.native_classifier(platform)?;
        self.downloads.classifiers.as_ref()?.get(&classifier)
    }
}

/// Which Java runtime a version needs.
///
/// Read from the manifest, never inferred from the version id. Components seen
/// so far: `jre-legacy` (8), `java-runtime-alpha` (16), `java-runtime-beta` and
/// `java-runtime-gamma` (17), `java-runtime-delta` (21), `java-runtime-epsilon`
/// (25). New ones appear without warning, which is exactly why this is data.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct JavaVersion {
    pub component: String,
    pub major_version: u32,
}

impl Default for JavaVersion {
    /// Manifests written before 2021 have no `javaVersion` at all. Those
    /// versions predate Java 9 and run on 8.
    fn default() -> Self {
        Self {
            component: "jre-legacy".to_owned(),
            major_version: 8,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AssetIndexRef {
    pub id: String,
    pub sha1: String,
    pub size: u64,
    #[serde(default)]
    pub total_size: u64,
    pub url: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
pub struct Arguments {
    #[serde(default)]
    pub game: Vec<Argument>,
    #[serde(default)]
    pub jvm: Vec<Argument>,
}

/// A parsed version manifest.
///
/// This is the raw document. Resolving `inheritsFrom` - which mod loader
/// profiles rely on - happens in `ds-mc`, because it needs to fetch the parent.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VersionManifest {
    /// **An opaque string.** `1.21.11` and `26.2` do not compare under any
    /// version-parsing scheme; ordering comes from `release_time` and `kind`.
    pub id: String,

    /// `release`, `snapshot`, `old_beta`, `old_alpha`.
    #[serde(rename = "type", default)]
    pub kind: String,

    #[serde(default)]
    pub release_time: String,

    /// Absent on loader profiles, which inherit it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub main_class: Option<String>,

    /// The version whose manifest this one extends. Set by Fabric, Quilt,
    /// NeoForge and Forge profiles.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub inherits_from: Option<String>,

    /// Asset index id. `legacy` and `pre-1.6` select the old on-disk layouts.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub assets: Option<String>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub asset_index: Option<AssetIndexRef>,

    /// Absent on loader profiles, which inherit it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub java_version: Option<JavaVersion>,

    #[serde(default)]
    pub downloads: BTreeMap<String, Download>,

    #[serde(default)]
    pub libraries: Vec<Library>,

    /// Modern (1.13+) argument lists.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub arguments: Option<Arguments>,

    /// Legacy (pre-1.13) single-string argument line.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub minecraft_arguments: Option<String>,
}

impl VersionManifest {
    pub fn parse(json: &str) -> Result<Self, serde_json::Error> {
        serde_json::from_str(json)
    }

    /// Libraries that apply on this platform, in manifest order.
    ///
    /// Order is preserved because it is the classpath order, and Minecraft has
    /// historically depended on it.
    ///
    /// Returns a `Vec` rather than an iterator on purpose: an iterator would
    /// borrow `platform` for its whole lifetime, which makes every caller write
    /// a `let` binding for what looks like a throwaway value. 107 pointers is
    /// not an allocation worth optimising.
    pub fn applicable_libraries<'a>(
        &'a self,
        platform: &Platform,
        features: &Features,
    ) -> Vec<&'a Library> {
        self.libraries
            .iter()
            .filter(|library| library.applies_to(platform, features))
            .collect()
    }

    /// The client jar, if this manifest declares one.
    pub fn client_download(&self) -> Option<&Download> {
        self.downloads.get("client")
    }

    /// Whether this version uses the old flat `resources/` asset layout.
    pub fn uses_legacy_assets(&self) -> bool {
        matches!(self.assets.as_deref(), Some("legacy" | "pre-1.6"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::platform::{Arch, Os};

    const REAL: &str = include_str!("../tests/fixtures/1.21.11.trimmed.json");

    fn manifest() -> VersionManifest {
        VersionManifest::parse(REAL).expect("the real 1.21.11 manifest should parse")
    }

    fn platform(os: Os) -> Platform {
        Platform::new(os, Arch::X86_64, "10.0.26200")
    }

    #[test]
    fn parses_a_real_manifest() {
        let m = manifest();
        assert_eq!(m.id, "1.21.11");
        assert_eq!(m.kind, "release");
        assert_eq!(
            m.main_class.as_deref(),
            Some("net.minecraft.client.main.Main")
        );
        assert!(m.inherits_from.is_none());
    }

    /// The rule that a hardcoded table would break. 1.21.11 wants Java 21 via
    /// `java-runtime-delta`, and it says so in the manifest.
    #[test]
    fn java_requirement_comes_from_the_manifest() {
        let java = manifest()
            .java_version
            .expect("1.21.11 declares javaVersion");
        assert_eq!(java.component, "java-runtime-delta");
        assert_eq!(java.major_version, 21);
    }

    /// Pre-2021 manifests omit javaVersion entirely; those versions run on 8.
    #[test]
    fn an_absent_java_version_defaults_to_eight() {
        let m = VersionManifest::parse(r#"{"id":"1.8.9","type":"release"}"#).unwrap();
        assert!(m.java_version.is_none());
        assert_eq!(JavaVersion::default().major_version, 8);
    }

    /// Each platform gets a different set of libraries out of the same file.
    #[test]
    fn library_selection_differs_per_platform() {
        let m = manifest();
        let features = Features::new();

        let counts: Vec<usize> = [Os::Windows, Os::Linux, Os::Osx]
            .into_iter()
            .map(|os| m.applicable_libraries(&platform(os), &features).len())
            .collect();

        // Three unconditional plus exactly one OS-gated library each.
        assert_eq!(counts, [4, 4, 4], "got {counts:?}");

        let on_windows: Vec<&str> = m
            .applicable_libraries(&platform(Os::Windows), &features)
            .into_iter()
            .map(|l| l.name.as_str())
            .collect();
        assert!(
            !on_windows.iter().any(|n| n.contains("java-objc-bridge")),
            "the macOS-only bridge leaked onto Windows: {on_windows:?}"
        );
    }

    /// Order is classpath order, and Minecraft has historically depended on it.
    #[test]
    fn library_order_is_preserved() {
        let m = manifest();
        let features = Features::new();
        let selected: Vec<&str> = m
            .applicable_libraries(&platform(Os::Windows), &features)
            .into_iter()
            .map(|l| l.name.as_str())
            .collect();

        let manifest_order: Vec<&str> = m
            .libraries
            .iter()
            .filter(|l| l.applies_to(&platform(Os::Windows), &features))
            .map(|l| l.name.as_str())
            .collect();

        assert_eq!(selected, manifest_order);
    }

    #[test]
    fn client_jar_and_asset_index_are_present() {
        let m = manifest();
        let client = m.client_download().expect("client download");
        assert!(client.url.ends_with("client.jar"));
        assert_eq!(client.sha1.len(), 40);

        let assets = m.asset_index.expect("assetIndex");
        assert_eq!(assets.id, "29");
        assert!(assets.total_size > 0);
    }

    /// Modern versions carry no natives/classifiers at all - absence is normal,
    /// not a parse failure.
    #[test]
    fn modern_manifests_have_no_legacy_natives() {
        let m = manifest();
        for library in &m.libraries {
            assert!(
                library.natives.is_none(),
                "{} unexpectedly has a natives block",
                library.name
            );
        }
    }

    /// The legacy shape still has to work for old versions.
    #[test]
    fn legacy_natives_resolve_with_arch_expanded() {
        let json = r#"{
            "id":"1.8.9","type":"release",
            "libraries":[{
                "name":"org.lwjgl:lwjgl-platform:2.9.4",
                "natives":{"windows":"natives-windows-${arch}","linux":"natives-linux"},
                "downloads":{"classifiers":{
                    "natives-windows-64":{"sha1":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa","size":1,"url":"http://x/64.jar"},
                    "natives-windows-32":{"sha1":"bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb","size":1,"url":"http://x/32.jar"},
                    "natives-linux":{"sha1":"cccccccccccccccccccccccccccccccccccccccc","size":1,"url":"http://x/l.jar"}
                }},
                "extract":{"exclude":["META-INF/"]}
            }]
        }"#;

        let m = VersionManifest::parse(json).expect("legacy manifest should parse");
        let library = &m.libraries[0];

        let win64 = Platform::new(Os::Windows, Arch::X86_64, "10.0");
        assert_eq!(
            library.native_classifier(&win64).as_deref(),
            Some("natives-windows-64")
        );
        assert_eq!(
            library.native_download(&win64).map(|d| d.url.as_str()),
            Some("http://x/64.jar")
        );

        let win32 = Platform::new(Os::Windows, Arch::X86, "10.0");
        assert_eq!(
            library.native_classifier(&win32).as_deref(),
            Some("natives-windows-32")
        );

        // No macOS entry in this library's natives map.
        let mac = Platform::new(Os::Osx, Arch::Arm64, "14.0");
        assert!(library.native_classifier(&mac).is_none());

        let extract = library.extract.as_ref().expect("extract block");
        assert_eq!(extract.exclude, ["META-INF/"]);
    }

    #[test]
    fn legacy_asset_layouts_are_recognised() {
        for (assets, expected) in [
            (Some("29"), false),
            (Some("legacy"), true),
            (Some("pre-1.6"), true),
            (None, false),
        ] {
            let json = match assets {
                Some(a) => format!(r#"{{"id":"x","type":"release","assets":"{a}"}}"#),
                None => r#"{"id":"x","type":"release"}"#.to_owned(),
            };
            let m = VersionManifest::parse(&json).unwrap();
            assert_eq!(m.uses_legacy_assets(), expected, "assets={assets:?}");
        }
    }

    /// A loader profile carries inheritsFrom and omits most other fields. It
    /// must still parse - resolving the parent is ds-mc's job.
    #[test]
    fn a_loader_profile_parses_with_almost_everything_absent() {
        let json = r#"{
            "id":"fabric-loader-0.18.5-1.21.11",
            "inheritsFrom":"1.21.11",
            "type":"release",
            "mainClass":"net.fabricmc.loader.impl.launch.knot.KnotClient",
            "libraries":[]
        }"#;
        let m = VersionManifest::parse(json).expect("loader profile should parse");
        assert_eq!(m.inherits_from.as_deref(), Some("1.21.11"));
        assert!(
            m.java_version.is_none(),
            "must inherit Java from the parent"
        );
        assert!(m.asset_index.is_none());
    }
}
