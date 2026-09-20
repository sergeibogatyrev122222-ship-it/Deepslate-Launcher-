//! Maven coordinates and classpath assembly.
//!
//! A library's file location comes from one of two places. Vanilla manifests
//! state it outright in `downloads.artifact.path`. Mod loader profiles usually
//! do not - Fabric, Quilt and NeoForge list libraries by Maven coordinate alone
//! and expect the launcher to derive the path. Both have to work.

use std::fmt;

use crate::platform::{Os, Platform};
use crate::rules::Features;
use crate::version::{Library, VersionManifest};

/// A parsed Maven coordinate: `group:artifact:version[:classifier][@extension]`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Coordinate {
    pub group: String,
    pub artifact: String,
    pub version: String,
    pub classifier: Option<String>,
    pub extension: String,
}

/// A coordinate that does not have at least group, artifact and version.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MalformedCoordinate(pub String);

impl fmt::Display for MalformedCoordinate {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "'{}' is not a Maven coordinate (expected group:artifact:version)",
            self.0
        )
    }
}

impl std::error::Error for MalformedCoordinate {}

impl Coordinate {
    pub fn parse(raw: &str) -> Result<Self, MalformedCoordinate> {
        // The extension suffix comes off first: it can follow any component.
        let (body, extension) = match raw.split_once('@') {
            Some((body, ext)) if !ext.is_empty() => (body, ext.to_owned()),
            _ => (raw, "jar".to_owned()),
        };

        let mut parts = body.split(':');
        let (Some(group), Some(artifact), Some(version)) =
            (parts.next(), parts.next(), parts.next())
        else {
            return Err(MalformedCoordinate(raw.to_owned()));
        };

        if group.is_empty() || artifact.is_empty() || version.is_empty() {
            return Err(MalformedCoordinate(raw.to_owned()));
        }

        Ok(Self {
            group: group.to_owned(),
            artifact: artifact.to_owned(),
            version: version.to_owned(),
            classifier: parts.next().filter(|c| !c.is_empty()).map(str::to_owned),
            extension,
        })
    }

    /// The path this artifact sits at inside a Maven repository, relative and
    /// always forward-slashed - manifests use `/` regardless of platform, and so
    /// do the URLs these paths are also used to build.
    pub fn to_path(&self) -> String {
        let group = self.group.replace('.', "/");
        let classifier = match &self.classifier {
            Some(c) => format!("-{c}"),
            None => String::new(),
        };
        format!(
            "{group}/{artifact}/{version}/{artifact}-{version}{classifier}.{extension}",
            artifact = self.artifact,
            version = self.version,
            extension = self.extension,
        )
    }

    /// `group:artifact`, ignoring version and classifier.
    ///
    /// Two libraries sharing this identity are the same dependency at different
    /// versions, and only one may be on the classpath.
    pub fn identity(&self) -> String {
        format!("{}:{}", self.group, self.artifact)
    }
}

impl Library {
    /// Where this library's jar lives, relative to the library root.
    ///
    /// Prefers the explicit path when the manifest states one, and falls back
    /// to deriving it from the Maven coordinate - which is the only option for
    /// most mod loader libraries.
    pub fn relative_path(&self) -> Result<String, MalformedCoordinate> {
        if let Some(path) = self
            .downloads
            .artifact
            .as_ref()
            .and_then(|artifact| artifact.path.as_deref())
        {
            return Ok(path.to_owned());
        }
        Ok(Coordinate::parse(&self.name)?.to_path())
    }
}

/// The separator the JVM expects between classpath entries.
pub const fn separator(os: Os) -> char {
    match os {
        Os::Windows => ';',
        Os::Linux | Os::Osx => ':',
    }
}

/// Build the classpath entries for a version.
///
/// Two rules that matter:
///
/// - **Order is manifest order**, with the client jar last. Minecraft has
///   historically depended on library ordering.
/// - **The first occurrence of a `group:artifact` wins.** Mod loaders override
///   vanilla libraries by listing their own version, so whoever merges an
///   `inheritsFrom` chain must place the overriding manifest's libraries
///   first. That contract lives here because this is where it takes effect.
pub fn entries(
    manifest: &VersionManifest,
    platform: &Platform,
    features: &Features,
    client_jar: Option<&str>,
) -> Result<Vec<String>, MalformedCoordinate> {
    let mut seen = Vec::new();
    let mut out = Vec::new();

    for library in manifest.applicable_libraries(platform, features) {
        // A legacy natives-only library contributes an extracted directory, not
        // a classpath entry.
        if library.natives.is_some() && library.downloads.artifact.is_none() {
            continue;
        }

        let identity = match Coordinate::parse(&library.name) {
            Ok(coordinate) => coordinate.identity(),
            // A name we cannot parse still has a usable explicit path; fall back
            // to the whole name as its identity rather than dropping it.
            Err(_) if library.downloads.artifact.is_some() => library.name.clone(),
            Err(error) => return Err(error),
        };

        if seen.contains(&identity) {
            continue;
        }
        seen.push(identity);
        out.push(library.relative_path()?);
    }

    if let Some(jar) = client_jar {
        out.push(jar.to_owned());
    }

    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::platform::{Arch, Os};

    fn windows() -> Platform {
        Platform::new(Os::Windows, Arch::X86_64, "10.0.26200")
    }

    #[test]
    fn parses_a_plain_coordinate() {
        let c = Coordinate::parse("com.mojang:patchy:1.3.9").unwrap();
        assert_eq!(c.group, "com.mojang");
        assert_eq!(c.artifact, "patchy");
        assert_eq!(c.version, "1.3.9");
        assert_eq!(c.classifier, None);
        assert_eq!(c.extension, "jar");
        assert_eq!(c.to_path(), "com/mojang/patchy/1.3.9/patchy-1.3.9.jar");
    }

    #[test]
    fn a_classifier_is_appended_to_the_filename() {
        let c = Coordinate::parse("org.lwjgl:lwjgl:3.3.3:natives-windows").unwrap();
        assert_eq!(c.classifier.as_deref(), Some("natives-windows"));
        assert_eq!(
            c.to_path(),
            "org/lwjgl/lwjgl/3.3.3/lwjgl-3.3.3-natives-windows.jar"
        );
    }

    /// Forge and NeoForge use `@zip` and `@txt` artifacts, not only jars.
    #[test]
    fn an_extension_override_is_honoured() {
        let c = Coordinate::parse("de.oceanlabs.mcp:mcp_config:1.21.11@zip").unwrap();
        assert_eq!(c.extension, "zip");
        assert_eq!(
            c.to_path(),
            "de/oceanlabs/mcp/mcp_config/1.21.11/mcp_config-1.21.11.zip"
        );
    }

    #[test]
    fn classifier_and_extension_together() {
        let c = Coordinate::parse("a.b:c:1.0:natives@zip").unwrap();
        assert_eq!(c.classifier.as_deref(), Some("natives"));
        assert_eq!(c.extension, "zip");
        assert_eq!(c.to_path(), "a/b/c/1.0/c-1.0-natives.zip");
    }

    #[test]
    fn group_dots_become_directories() {
        let c = Coordinate::parse("net.fabricmc.fabric-api:fabric-api:0.100.0").unwrap();
        assert!(c
            .to_path()
            .starts_with("net/fabricmc/fabric-api/fabric-api/"));
    }

    #[test]
    fn malformed_coordinates_are_rejected_not_guessed() {
        for bad in ["", "justaname", "group:artifact", "group::version", ":a:1"] {
            assert!(
                Coordinate::parse(bad).is_err(),
                "'{bad}' should not have parsed"
            );
        }
    }

    #[test]
    fn classpath_separator_is_platform_specific() {
        assert_eq!(separator(Os::Windows), ';');
        assert_eq!(separator(Os::Linux), ':');
        assert_eq!(separator(Os::Osx), ':');
    }

    fn manifest_with(libraries: &str) -> VersionManifest {
        VersionManifest::parse(&format!(
            r#"{{"id":"t","type":"release","libraries":[{libraries}]}}"#
        ))
        .expect("test manifest should parse")
    }

    #[test]
    fn explicit_path_is_preferred_over_the_coordinate() {
        let m = manifest_with(
            r#"{"name":"com.mojang:patchy:1.3.9","downloads":{"artifact":{
                "path":"custom/place/patchy.jar","sha1":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
                "size":1,"url":"http://x"}}}"#,
        );
        let got = entries(&m, &windows(), &Features::new(), None).unwrap();
        assert_eq!(got, ["custom/place/patchy.jar"]);
    }

    /// Loader libraries usually have no downloads block at all.
    #[test]
    fn a_library_with_no_downloads_falls_back_to_its_coordinate() {
        let m = manifest_with(r#"{"name":"net.fabricmc:intermediary:1.21.11"}"#);
        let got = entries(&m, &windows(), &Features::new(), None).unwrap();
        assert_eq!(
            got,
            ["net/fabricmc/intermediary/1.21.11/intermediary-1.21.11.jar"]
        );
    }

    /// The override contract: a loader listing its own version of a vanilla
    /// library must win, and it wins by being listed first.
    #[test]
    fn the_first_occurrence_of_a_group_artifact_wins() {
        let m = manifest_with(
            r#"{"name":"com.google.guava:guava:33.0.0"},
               {"name":"com.google.guava:guava:21.0"}"#,
        );
        let got = entries(&m, &windows(), &Features::new(), None).unwrap();
        assert_eq!(got.len(), 1, "both versions ended up on the classpath");
        assert!(got[0].contains("33.0.0"), "the wrong version won: {got:?}");
    }

    #[test]
    fn different_artifacts_from_one_group_all_survive() {
        let m = manifest_with(
            r#"{"name":"org.lwjgl:lwjgl:3.3.3"},
               {"name":"org.lwjgl:lwjgl-glfw:3.3.3"}"#,
        );
        let got = entries(&m, &windows(), &Features::new(), None).unwrap();
        assert_eq!(got.len(), 2);
    }

    #[test]
    fn the_client_jar_goes_last() {
        let m = manifest_with(r#"{"name":"a:b:1"}"#);
        let got = entries(&m, &windows(), &Features::new(), Some("versions/t/t.jar")).unwrap();
        assert_eq!(got.last().map(String::as_str), Some("versions/t/t.jar"));
        assert_eq!(got.len(), 2);
    }

    #[test]
    fn excluded_libraries_never_reach_the_classpath() {
        let m = manifest_with(
            r#"{"name":"mac.only:thing:1","rules":[{"action":"allow","os":{"name":"osx"}}]}"#,
        );
        let got = entries(&m, &windows(), &Features::new(), None).unwrap();
        assert!(got.is_empty(), "macOS-only library leaked: {got:?}");
    }

    /// A pre-1.19 natives-only library contributes an extracted directory, not
    /// a classpath entry.
    #[test]
    fn legacy_natives_only_libraries_are_not_classpath_entries() {
        let m = manifest_with(
            r#"{"name":"org.lwjgl:lwjgl-platform:2.9.4",
                "natives":{"windows":"natives-windows-${arch}"},
                "downloads":{"classifiers":{"natives-windows-64":{
                    "sha1":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa","size":1,"url":"http://x"}}}}"#,
        );
        let got = entries(&m, &windows(), &Features::new(), None).unwrap();
        assert!(
            got.is_empty(),
            "a natives jar reached the classpath: {got:?}"
        );
    }

    /// Order is classpath order and Minecraft has depended on it historically.
    #[test]
    fn manifest_order_is_preserved() {
        let m = manifest_with(
            r#"{"name":"z.z:first:1"},{"name":"a.a:second:1"},{"name":"m.m:third:1"}"#,
        );
        let got = entries(&m, &windows(), &Features::new(), None).unwrap();
        assert!(got[0].contains("first"));
        assert!(got[1].contains("second"));
        assert!(got[2].contains("third"));
    }
}
