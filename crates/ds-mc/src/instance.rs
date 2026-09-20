//! Instances: one isolated Minecraft installation each.
//!
//! Isolation here is structural, not a convention anyone has to remember. Each
//! instance owns a directory tree, and the game is launched with both its
//! working directory and `--gameDir` pointed inside it. A mod writing to its
//! config directory in one instance cannot reach another.
//!
//! `-Duser.home` is deliberately **not** set. It would catch the small number
//! of mods that write to the home directory rather than the game directory, but
//! it also redirects JVM internals - preferences, certificate stores, temporary
//! files - and no major launcher sets it by default. The isolation that matters
//! for Minecraft comes from `--gameDir`; escaping it requires a mod that
//! deliberately ignores the API it is given.
//!
//! ```text
//! instances/<slug>/
//!   instance.toml      settings - plain TOML, hand-editable, exportable
//!   minecraft/         THE game directory: saves, config, mods, options.txt
//!   natives/           extracted per-instance; see below
//! ```
//!
//! What instances *share* is the content-addressed store, which holds only
//! immutable hash-verified artifacts. Ten instances on the same version share
//! one copy of every jar and nothing writable.
//!
//! Natives are the deliberate exception to that sharing: a running JVM holds
//! open handles on its native DLLs, so two instances running at once would
//! collide over them.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

#[derive(Debug, thiserror::Error)]
pub enum InstanceError {
    #[error("io error at {path}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("'{0}' does not contain any characters usable in a folder name")]
    UnusableName(String),

    #[error("an instance called '{0}' already exists")]
    AlreadyExists(String),

    #[error("no instance called '{0}'")]
    NotFound(String),

    #[error("could not read {path}")]
    Malformed {
        path: PathBuf,
        #[source]
        source: toml::de::Error,
    },

    #[error("could not serialise the instance settings")]
    Serialise(#[source] toml::ser::Error),
}

type Result<T> = std::result::Result<T, InstanceError>;

fn io_err(path: impl Into<PathBuf>) -> impl FnOnce(std::io::Error) -> InstanceError {
    let path = path.into();
    move |source| InstanceError::Io { path, source }
}

/// Per-instance settings. Everything here is overridable per instance, which is
/// the point of instances.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InstanceConfig {
    /// What the user called it. May contain anything; the slug is derived.
    pub name: String,

    /// Version id to launch. An opaque string - `1.21.11` and `26.2` do not
    /// compare.
    pub version: String,

    /// Heap size. `None` means "decide from system RAM at launch".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub memory_mb: Option<u32>,

    /// Extra JVM arguments, appended after the generated ones so a user can
    /// override anything the launcher chose.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub jvm_args: Vec<String>,

    /// Use this exact Java rather than whatever discovery selects.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub java_path: Option<PathBuf>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub window_width: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub window_height: Option<u32>,
}

impl InstanceConfig {
    pub fn new(name: impl Into<String>, version: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            version: version.into(),
            memory_mb: None,
            jvm_args: Vec::new(),
            java_path: None,
            window_width: None,
            window_height: None,
        }
    }
}

/// A folder-safe identifier derived from a display name.
///
/// Deliberately conservative: lowercase ASCII alphanumerics and hyphens only.
/// Instance names come from users, and a name is about to become a path - a
/// filter that only allows known-good characters cannot be tricked the way a
/// blocklist can.
pub fn slugify(name: &str) -> Option<String> {
    let mut slug = String::with_capacity(name.len());
    let mut last_was_dash = true; // leading dashes are dropped

    for ch in name.chars() {
        if ch.is_ascii_alphanumeric() {
            slug.push(ch.to_ascii_lowercase());
            last_was_dash = false;
        } else if !last_was_dash {
            slug.push('-');
            last_was_dash = true;
        }
    }

    while slug.ends_with('-') {
        slug.pop();
    }

    if slug.is_empty() {
        return None;
    }

    // Windows refuses these as file names regardless of extension.
    const RESERVED: [&str; 22] = [
        "con", "prn", "aux", "nul", "com1", "com2", "com3", "com4", "com5", "com6", "com7", "com8",
        "com9", "lpt1", "lpt2", "lpt3", "lpt4", "lpt5", "lpt6", "lpt7", "lpt8", "lpt9",
    ];
    if RESERVED.contains(&slug.as_str()) {
        slug.push_str("-instance");
    }

    Some(slug)
}

/// One instance on disk.
#[derive(Debug, Clone)]
pub struct Instance {
    slug: String,
    root: PathBuf,
    config: InstanceConfig,
}

impl Instance {
    pub fn slug(&self) -> &str {
        &self.slug
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn config(&self) -> &InstanceConfig {
        &self.config
    }

    pub fn config_mut(&mut self) -> &mut InstanceConfig {
        &mut self.config
    }

    /// The game directory. Everything Minecraft writes goes here and nowhere
    /// else.
    pub fn game_dir(&self) -> PathBuf {
        self.root.join("minecraft")
    }

    /// Where native libraries are extracted for this instance alone.
    pub fn natives_dir(&self) -> PathBuf {
        self.root.join("natives")
    }

    pub fn config_path(&self) -> PathBuf {
        self.root.join("instance.toml")
    }

    /// Create a new instance directory tree.
    pub fn create(instances_root: &Path, config: InstanceConfig) -> Result<Self> {
        let slug = slugify(&config.name)
            .ok_or_else(|| InstanceError::UnusableName(config.name.clone()))?;
        let root = instances_root.join(&slug);

        if root.exists() {
            return Err(InstanceError::AlreadyExists(slug));
        }

        let instance = Self { slug, root, config };
        std::fs::create_dir_all(instance.game_dir()).map_err(io_err(instance.game_dir()))?;
        std::fs::create_dir_all(instance.natives_dir()).map_err(io_err(instance.natives_dir()))?;
        instance.save()?;
        Ok(instance)
    }

    /// Load an existing instance.
    pub fn load(instances_root: &Path, slug: &str) -> Result<Self> {
        let root = instances_root.join(slug);
        let path = root.join("instance.toml");

        let text = std::fs::read_to_string(&path).map_err(|source| {
            if source.kind() == std::io::ErrorKind::NotFound {
                InstanceError::NotFound(slug.to_owned())
            } else {
                InstanceError::Io {
                    path: path.clone(),
                    source,
                }
            }
        })?;

        let config =
            toml::from_str(&text).map_err(|source| InstanceError::Malformed { path, source })?;

        Ok(Self {
            slug: slug.to_owned(),
            root,
            config,
        })
    }

    /// Write settings atomically: temporary file, then rename.
    ///
    /// Writing in place risks a truncated config if the process dies mid-write,
    /// which would lose an instance's settings rather than a moment's work.
    pub fn save(&self) -> Result<()> {
        let text = toml::to_string_pretty(&self.config).map_err(InstanceError::Serialise)?;
        let path = self.config_path();
        let temp = path.with_extension("toml.tmp");

        std::fs::write(&temp, text).map_err(io_err(&temp))?;
        std::fs::rename(&temp, &path).map_err(io_err(&path))
    }

    /// Every instance under a root, sorted by slug.
    ///
    /// A directory that is not an instance is skipped rather than reported -
    /// users put things in folders, and one stray directory should not stop the
    /// list rendering.
    pub fn list(instances_root: &Path) -> Vec<Self> {
        let Ok(entries) = std::fs::read_dir(instances_root) else {
            return Vec::new();
        };

        let mut found: Vec<Self> = entries
            .flatten()
            .filter(|entry| entry.path().is_dir())
            .filter_map(|entry| {
                let slug = entry.file_name().to_string_lossy().into_owned();
                Self::load(instances_root, &slug).ok()
            })
            .collect();

        found.sort_by(|a, b| a.slug.cmp(&b.slug));
        found
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn root() -> tempfile::TempDir {
        tempfile::tempdir().expect("temp dir")
    }

    #[test]
    fn slugs_are_lowercase_and_folder_safe() {
        assert_eq!(slugify("My Instance").as_deref(), Some("my-instance"));
        assert_eq!(slugify("1.21.11 Fabric").as_deref(), Some("1-21-11-fabric"));
        assert_eq!(slugify("ALL CAPS").as_deref(), Some("all-caps"));
    }

    /// An instance name is about to become a path, and it comes from a user.
    /// Only known-good characters survive.
    #[test]
    fn path_traversal_cannot_survive_slugification() {
        for attack in [
            "../../etc/passwd",
            "..\\..\\windows",
            "a/b/c",
            "C:\\windows",
            "....//....//",
        ] {
            let slug = slugify(attack);
            if let Some(slug) = slug {
                assert!(!slug.contains('/'), "{attack} -> {slug}");
                assert!(!slug.contains('\\'), "{attack} -> {slug}");
                assert!(!slug.contains(".."), "{attack} -> {slug}");
                assert!(!slug.contains(':'), "{attack} -> {slug}");
            }
        }
    }

    #[test]
    fn names_with_nothing_usable_are_rejected() {
        assert_eq!(slugify(""), None);
        assert_eq!(slugify("   "), None);
        assert_eq!(slugify("///"), None);
        assert_eq!(slugify("..."), None);
    }

    /// Windows refuses these as file names whatever the extension.
    #[test]
    fn windows_reserved_names_are_escaped() {
        assert_eq!(slugify("CON").as_deref(), Some("con-instance"));
        assert_eq!(slugify("nul").as_deref(), Some("nul-instance"));
        assert_eq!(slugify("COM1").as_deref(), Some("com1-instance"));
        // Only exact matches are reserved.
        assert_eq!(slugify("console").as_deref(), Some("console"));
    }

    #[test]
    fn creating_an_instance_builds_the_whole_tree() {
        let dir = root();
        let instance =
            Instance::create(dir.path(), InstanceConfig::new("Test One", "1.21.11")).unwrap();

        assert_eq!(instance.slug(), "test-one");
        assert!(instance.game_dir().is_dir(), "game dir missing");
        assert!(instance.natives_dir().is_dir(), "natives dir missing");
        assert!(instance.config_path().is_file(), "config missing");
    }

    #[test]
    fn settings_survive_a_reload() {
        let dir = root();
        let mut config = InstanceConfig::new("Modded", "1.21.11");
        config.memory_mb = Some(6144);
        config.jvm_args = vec!["-XX:+UseG1GC".to_owned()];
        config.window_width = Some(1600);

        Instance::create(dir.path(), config.clone()).unwrap();
        let loaded = Instance::load(dir.path(), "modded").unwrap();

        assert_eq!(loaded.config(), &config);
    }

    #[test]
    fn creating_the_same_name_twice_is_refused() {
        let dir = root();
        Instance::create(dir.path(), InstanceConfig::new("Dupe", "1.21.11")).unwrap();

        let second = Instance::create(dir.path(), InstanceConfig::new("Dupe", "26.2"));
        assert!(
            matches!(second, Err(InstanceError::AlreadyExists(_))),
            "an existing instance was silently overwritten"
        );
    }

    #[test]
    fn loading_something_that_is_not_there_says_so() {
        let dir = root();
        assert!(matches!(
            Instance::load(dir.path(), "ghost"),
            Err(InstanceError::NotFound(_))
        ));
    }

    /// The whole promise of instances: two of them share nothing writable.
    #[test]
    fn two_instances_share_no_writable_directory() {
        let dir = root();
        let a = Instance::create(dir.path(), InstanceConfig::new("Alpha", "1.21.11")).unwrap();
        let b = Instance::create(dir.path(), InstanceConfig::new("Beta", "1.21.11")).unwrap();

        assert_ne!(a.root(), b.root());
        assert_ne!(a.game_dir(), b.game_dir());
        assert_ne!(a.natives_dir(), b.natives_dir());

        // Writing into one must not appear in the other.
        std::fs::write(a.game_dir().join("options.txt"), "fov:90").unwrap();
        assert!(a.game_dir().join("options.txt").is_file());
        assert!(
            !b.game_dir().join("options.txt").exists(),
            "instances share a game directory"
        );
    }

    /// Same version, different instances - still fully separate on disk.
    #[test]
    fn instances_on_the_same_version_are_still_separate() {
        let dir = root();
        let a = Instance::create(dir.path(), InstanceConfig::new("Vanilla", "1.21.11")).unwrap();
        let b = Instance::create(dir.path(), InstanceConfig::new("Modded", "1.21.11")).unwrap();

        assert_eq!(a.config().version, b.config().version);
        assert_ne!(a.game_dir(), b.game_dir());

        std::fs::create_dir_all(b.game_dir().join("mods")).unwrap();
        std::fs::write(b.game_dir().join("mods/sodium.jar"), "x").unwrap();
        assert!(
            !a.game_dir().join("mods").exists(),
            "a mod folder leaked between instances"
        );
    }

    #[test]
    fn listing_returns_instances_sorted_and_skips_stray_folders() {
        let dir = root();
        Instance::create(dir.path(), InstanceConfig::new("Zulu", "1.21.11")).unwrap();
        Instance::create(dir.path(), InstanceConfig::new("Alpha", "26.2")).unwrap();
        std::fs::create_dir_all(dir.path().join("not-an-instance")).unwrap();

        let all = Instance::list(dir.path());
        let slugs: Vec<&str> = all.iter().map(Instance::slug).collect();
        assert_eq!(slugs, ["alpha", "zulu"], "stray folder was not skipped");
    }

    #[test]
    fn an_empty_root_lists_nothing_rather_than_failing() {
        let dir = root();
        assert!(Instance::list(dir.path()).is_empty());
        assert!(Instance::list(Path::new("/definitely/not/here")).is_empty());
    }

    /// The config is the user's to read and edit; it must be plain and legible.
    #[test]
    fn the_config_file_is_readable_toml() {
        let dir = root();
        let mut config = InstanceConfig::new("Readable", "1.21.11");
        config.memory_mb = Some(4096);
        let instance = Instance::create(dir.path(), config).unwrap();

        let text = std::fs::read_to_string(instance.config_path()).unwrap();
        assert!(text.contains("name = \"Readable\""), "{text}");
        assert!(text.contains("version = \"1.21.11\""), "{text}");
        assert!(text.contains("memory_mb = 4096"), "{text}");
    }
}
