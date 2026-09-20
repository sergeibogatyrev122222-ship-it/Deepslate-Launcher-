//! Finding a Java runtime that satisfies a version's requirement.
//!
//! The requirement itself is data, read from the resolved manifest - never a
//! table keyed on the version id. See `ds_core::version::JavaVersion`.
//!
//! Detection reads the `release` file that every JDK and JRE ships beside its
//! `bin` directory, rather than running `java -version` on each candidate.
//! Spawning a process per candidate costs hundreds of milliseconds on a machine
//! with several runtimes installed, and that cost would land squarely in the
//! click-to-launch budget.

use std::path::{Path, PathBuf};

/// Where an installation was found. Useful in diagnostics when the wrong Java
/// gets picked.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Source {
    /// A runtime the official launcher already downloaded.
    MojangRuntime,
    /// `JAVA_HOME`.
    JavaHome,
    /// A conventional install location for this platform.
    SystemInstall,
    /// Downloaded and managed by Deepslate.
    Managed,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JavaInstallation {
    /// The executable to spawn. `javaw.exe` on Windows, which has no console
    /// window; anything else would flash a black box on every launch.
    pub executable: PathBuf,
    pub major: u32,
    /// Full version string as the `release` file reports it.
    pub version: String,
    pub vendor: Option<String>,
    pub source: Source,
}

/// Extract the major version from a Java version string.
///
/// Two schemes, both still in the wild:
/// - `1.8.0_412` is Java **8**. Everything up to 8 used the `1.x` prefix.
/// - `21.0.7` is Java **21**. From 9 onwards the first component is the major.
pub fn parse_major(version: &str) -> Option<u32> {
    let trimmed = version.trim().trim_matches('"');
    let mut parts = trimmed.split(['.', '_', '-', '+']);
    let first = parts.next()?;

    if first == "1" {
        return parts.next()?.parse().ok();
    }
    first.parse().ok()
}

/// Read one key out of a JDK `release` file.
fn release_value(contents: &str, key: &str) -> Option<String> {
    for line in contents.lines() {
        if let Some(rest) = line.strip_prefix(key) {
            if let Some(value) = rest.strip_prefix('=') {
                return Some(value.trim().trim_matches('"').to_owned());
            }
        }
    }
    None
}

/// Inspect a Java home directory.
///
/// Returns `None` rather than an error for anything that is not a usable
/// runtime - a directory that merely looks like one is a normal thing to find
/// while scanning, not a failure worth reporting.
pub fn from_home(home: &Path, source: Source) -> Option<JavaInstallation> {
    let executable = executable_in(home)?;
    let contents = std::fs::read_to_string(home.join("release")).ok()?;
    let version = release_value(&contents, "JAVA_VERSION")?;
    let major = parse_major(&version)?;

    Some(JavaInstallation {
        executable,
        major,
        version,
        vendor: release_value(&contents, "IMPLEMENTOR"),
        source,
    })
}

/// The launcher binary inside a Java home, if one is there.
fn executable_in(home: &Path) -> Option<PathBuf> {
    // javaw first on Windows: `java.exe` is the console variant and would open
    // a terminal window behind the game.
    let candidates: &[&str] = if cfg!(windows) {
        &["bin/javaw.exe", "bin/java.exe"]
    } else {
        &["bin/java"]
    };

    candidates
        .iter()
        .map(|relative| home.join(relative))
        .find(|path| path.is_file())
}

/// Directories that may contain Java homes, per platform.
///
/// Each entry is a directory whose *children* are candidate homes, except where
/// noted. Kept as data so adding a vendor is a one-line change.
fn search_roots() -> Vec<(PathBuf, Source)> {
    let mut roots = Vec::new();

    if let Ok(home) = std::env::var("JAVA_HOME") {
        // JAVA_HOME is itself a home, not a directory of homes.
        roots.push((PathBuf::from(home), Source::JavaHome));
    }

    #[cfg(windows)]
    {
        if let Ok(local) = std::env::var("LOCALAPPDATA") {
            let local = PathBuf::from(local);
            // The Microsoft Store launcher's runtimes.
            roots.push((
                local.join(
                    "Packages/Microsoft.4297127D64EC6_8wekyb3d8bbwe/LocalCache/Local/runtime",
                ),
                Source::MojangRuntime,
            ));
        }
        if let Ok(appdata) = std::env::var("APPDATA") {
            let mc = PathBuf::from(appdata).join(".minecraft");
            roots.push((mc.join("runtime"), Source::MojangRuntime));
            roots.push((mc.join("jre"), Source::MojangRuntime));
        }
        roots.push((
            PathBuf::from("C:/Program Files (x86)/Minecraft Launcher/runtime"),
            Source::MojangRuntime,
        ));
        for vendor in [
            "C:/Program Files/Eclipse Adoptium",
            "C:/Program Files/Java",
            "C:/Program Files/Microsoft",
            "C:/Program Files/Zulu",
            "C:/Program Files/Amazon Corretto",
        ] {
            roots.push((PathBuf::from(vendor), Source::SystemInstall));
        }
    }

    #[cfg(target_os = "linux")]
    {
        roots.push((PathBuf::from("/usr/lib/jvm"), Source::SystemInstall));
        roots.push((PathBuf::from("/usr/java"), Source::SystemInstall));
    }

    #[cfg(target_os = "macos")]
    {
        roots.push((
            PathBuf::from("/Library/Java/JavaVirtualMachines"),
            Source::SystemInstall,
        ));
    }

    roots
}

/// Find every usable Java runtime on this machine.
///
/// Mojang's own runtime directories are searched, so a user who has run the
/// official launcher usually needs no download at all.
pub fn discover() -> Vec<JavaInstallation> {
    let mut found: Vec<JavaInstallation> = Vec::new();

    for (root, source) in search_roots() {
        // JAVA_HOME points at a home directly.
        if let Some(installation) = from_home(&root, source) {
            push_unique(&mut found, installation);
            continue;
        }

        // Mojang nests two levels: <component>/<platform>/<component>/.
        // Vendors nest one: <vendor-dir>/<jdk-x.y.z>/.
        // macOS bundles add a third: <jdk>/Contents/Home/.
        for depth1 in read_dirs(&root) {
            if let Some(installation) = from_home(&depth1, source) {
                push_unique(&mut found, installation);
                continue;
            }
            if let Some(installation) = from_home(&depth1.join("Contents/Home"), source) {
                push_unique(&mut found, installation);
                continue;
            }
            for depth2 in read_dirs(&depth1) {
                if let Some(installation) = from_home(&depth2, source) {
                    push_unique(&mut found, installation);
                    continue;
                }
                for depth3 in read_dirs(&depth2) {
                    if let Some(installation) = from_home(&depth3, source) {
                        push_unique(&mut found, installation);
                    }
                }
            }
        }
    }

    found
}

fn read_dirs(path: &Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(path) else {
        return Vec::new();
    };
    entries
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| path.is_dir())
        .collect()
}

fn push_unique(list: &mut Vec<JavaInstallation>, installation: JavaInstallation) {
    if !list.iter().any(|e| e.executable == installation.executable) {
        list.push(installation);
    }
}

/// Pick a runtime for a version that asks for `required`.
///
/// **Exact major version only.** Minecraft declares a precise requirement and
/// mismatches are not a gentle degradation: 1.12.2 will not start on Java 17,
/// and older Forge breaks on anything past 8. Silently substituting a
/// "close enough" runtime turns a clear "needs Java 8" into an unexplained
/// crash. When nothing matches, the answer is to download the right one.
///
/// Among equals, prefers the newest patch release, then a Mojang-supplied
/// runtime over a system one - the former is what the official launcher would
/// have used for this exact version.
pub fn select(installations: &[JavaInstallation], required: u32) -> Option<&JavaInstallation> {
    installations
        .iter()
        .filter(|candidate| candidate.major == required)
        .max_by(|a, b| {
            version_key(&a.version)
                .cmp(&version_key(&b.version))
                .then_with(|| source_rank(a.source).cmp(&source_rank(b.source)))
        })
}

/// A numerically comparable key for a Java version string.
///
/// String comparison is wrong here and quietly so: lexicographically
/// "21.0.7" beats "21.0.11", which would pick an older patch release and look
/// like it worked. Unlike Minecraft version ids, Java versions really do have
/// numeric components, so comparing them as numbers is correct rather than a
/// guess. Non-numeric build suffixes are dropped.
fn version_key(version: &str) -> Vec<u32> {
    version
        .trim()
        .trim_matches('"')
        .split(['.', '_', '-', '+'])
        .map_while(|part| part.parse::<u32>().ok())
        .collect()
}

/// Pick the runtime for an instance, honouring an explicit override.
///
/// An override is trusted without checking its version. Someone who has set an
/// exact path has said what they want; second-guessing it would make the
/// setting useless for the cases it exists for - testing a JDK build, or a
/// runtime this scan does not know how to find.
pub fn for_instance<'a>(
    override_path: Option<&'a Path>,
    installed: &'a [JavaInstallation],
    required: u32,
) -> Option<&'a Path> {
    if let Some(path) = override_path {
        return Some(path);
    }
    select(installed, required).map(|installation| installation.executable.as_path())
}

const fn source_rank(source: Source) -> u8 {
    match source {
        Source::Managed => 3,
        Source::MojangRuntime => 2,
        Source::JavaHome => 1,
        Source::SystemInstall => 0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn java_8_uses_the_old_one_dot_scheme() {
        assert_eq!(parse_major("1.8.0_412"), Some(8));
        assert_eq!(parse_major("1.7.0_80"), Some(7));
        assert_eq!(parse_major("1.6.0_45"), Some(6));
    }

    #[test]
    fn java_9_onwards_uses_the_first_component() {
        assert_eq!(parse_major("21.0.7"), Some(21));
        assert_eq!(parse_major("25"), Some(25));
        assert_eq!(parse_major("17.0.9+9"), Some(17));
        assert_eq!(parse_major("9"), Some(9));
    }

    #[test]
    fn quoted_and_padded_versions_still_parse() {
        assert_eq!(parse_major("\"21.0.7\""), Some(21));
        assert_eq!(parse_major("  21.0.7  "), Some(21));
    }

    #[test]
    fn nonsense_versions_are_rejected_rather_than_guessed() {
        assert_eq!(parse_major(""), None);
        assert_eq!(parse_major("banana"), None);
        assert_eq!(parse_major("1."), None);
        assert_eq!(parse_major("1.x"), None);
    }

    /// Shape taken from a real Adoptium `release` file.
    #[test]
    fn release_file_values_are_extracted() {
        let contents =
            "IMPLEMENTOR=\"Eclipse Adoptium\"\nJAVA_VERSION=\"21.0.11\"\nOS_ARCH=\"x86_64\"\n";
        assert_eq!(
            release_value(contents, "JAVA_VERSION").as_deref(),
            Some("21.0.11")
        );
        assert_eq!(
            release_value(contents, "IMPLEMENTOR").as_deref(),
            Some("Eclipse Adoptium")
        );
        assert_eq!(release_value(contents, "MISSING"), None);
    }

    /// Mojang's own runtimes carry JAVA_VERSION but no IMPLEMENTOR, so a
    /// missing vendor must not disqualify an installation.
    #[test]
    fn a_missing_vendor_is_tolerated() {
        let contents = "JAVA_VERSION=\"21.0.7\"\n";
        assert_eq!(
            release_value(contents, "JAVA_VERSION").as_deref(),
            Some("21.0.7")
        );
        assert_eq!(release_value(contents, "IMPLEMENTOR"), None);
    }

    fn installation(major: u32, version: &str, source: Source) -> JavaInstallation {
        JavaInstallation {
            executable: PathBuf::from(format!("/jvm/{version}/bin/java")),
            major,
            version: version.to_owned(),
            vendor: None,
            source,
        }
    }

    #[test]
    fn selection_requires_the_exact_major_version() {
        let installed = vec![
            installation(17, "17.0.9", Source::SystemInstall),
            installation(21, "21.0.7", Source::SystemInstall),
        ];

        assert_eq!(select(&installed, 21).map(|j| j.major), Some(21));
        assert_eq!(select(&installed, 17).map(|j| j.major), Some(17));
    }

    /// The important one. A version asking for 8 must not be handed 21 - that
    /// turns a clear "needs Java 8" into an unexplained crash.
    #[test]
    fn a_newer_runtime_is_not_substituted_for_an_older_requirement() {
        let installed = vec![installation(21, "21.0.7", Source::SystemInstall)];
        assert!(
            select(&installed, 8).is_none(),
            "Java 21 was substituted for a Java 8 requirement"
        );
    }

    #[test]
    fn an_older_runtime_is_not_substituted_either() {
        let installed = vec![installation(8, "1.8.0_412", Source::SystemInstall)];
        assert!(select(&installed, 21).is_none());
    }

    #[test]
    fn the_newest_patch_of_a_matching_major_wins() {
        let installed = vec![
            installation(21, "21.0.2", Source::SystemInstall),
            installation(21, "21.0.11", Source::SystemInstall),
            installation(21, "21.0.7", Source::SystemInstall),
        ];
        assert_eq!(
            select(&installed, 21).map(|j| j.version.as_str()),
            Some("21.0.11")
        );
    }

    /// Lexicographically "21.0.7" > "21.0.11", which would silently pick an
    /// older patch. Components have to compare as numbers.
    #[test]
    fn version_keys_compare_numerically_not_lexicographically() {
        assert!(version_key("21.0.11") > version_key("21.0.7"));
        assert!(version_key("21.0.2") < version_key("21.0.11"));
        assert!(version_key("1.8.0_412") > version_key("1.8.0_92"));
        assert_eq!(version_key("17.0.9+9"), vec![17, 0, 9, 9]);
    }

    /// The field exists to be obeyed. An override wins even over a matching
    /// installation, and even when nothing matches at all.
    #[test]
    fn an_explicit_java_path_overrides_discovery() {
        let installed = vec![installation(21, "21.0.11", Source::SystemInstall)];
        let chosen = Path::new("D:/custom/jdk/bin/java.exe");

        assert_eq!(
            for_instance(Some(chosen), &installed, 21),
            Some(chosen),
            "an override lost to a matching installation"
        );
        assert_eq!(
            for_instance(Some(chosen), &installed, 8),
            Some(chosen),
            "an override was discarded because nothing else matched"
        );
    }

    #[test]
    fn without_an_override_discovery_decides() {
        let installed = vec![installation(21, "21.0.11", Source::SystemInstall)];
        assert!(for_instance(None, &installed, 21).is_some());
        assert!(for_instance(None, &installed, 8).is_none());
    }

    #[test]
    fn nothing_installed_means_nothing_selected() {
        assert!(select(&[], 21).is_none());
    }

    /// Discovery must never panic or error on a machine with no Java at all;
    /// an empty list is a normal answer.
    #[test]
    fn discovery_is_infallible() {
        let found = discover();
        for installation in &found {
            assert!(installation.major > 0);
            assert!(installation.executable.is_file(), "{installation:?}");
        }
    }
}
