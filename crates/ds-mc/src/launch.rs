//! Building and spawning the JVM command.
//!
//! Construction is deliberately separate from spawning. The argument vector is
//! the thing most likely to be subtly wrong, and keeping it pure means it can
//! be asserted exactly in a test without launching anything.
//!
//! Arguments are always a **vector**, never a joined string. Player names,
//! instance paths and JVM flags routinely contain spaces, and a launcher that
//! builds a command line by concatenation and lets something else re-split it
//! is a launcher with an argument-injection bug waiting to be found.

use std::path::{Path, PathBuf};

use ds_core::args::{self, Substitutions, UnresolvedPlaceholders};
use ds_core::classpath;
use ds_core::platform::Platform;
use ds_core::rules::Features;
use ds_core::version::VersionManifest;

use crate::instance::Instance;

/// Identifies the launcher to the game and to Mojang's telemetry.
pub const LAUNCHER_NAME: &str = "deepslate";
pub const LAUNCHER_VERSION: &str = env!("CARGO_PKG_VERSION");

/// The authenticated session the game is launched with.
///
/// Defined here rather than taken from `ds-auth` to keep the dependency
/// direction one-way: launching knows what a session looks like, not how one is
/// obtained.
#[derive(Debug, Clone)]
pub struct Session {
    pub username: String,
    /// Dashless UUID, as Minecraft services returns it.
    pub uuid: String,
    pub access_token: String,
    /// `msa` for a Microsoft account. The only value this launcher produces.
    pub user_type: String,
    pub xuid: Option<String>,
}

#[derive(Debug, thiserror::Error)]
pub enum LaunchError {
    #[error("the version manifest declares no main class, so there is nothing to run")]
    NoMainClass,

    #[error("the version manifest declares no launch arguments")]
    NoArguments,

    #[error(transparent)]
    Unresolved(#[from] UnresolvedPlaceholders),

    #[error(transparent)]
    Classpath(#[from] classpath::MalformedCoordinate),

    #[error("could not start the game process")]
    Spawn(#[source] std::io::Error),
}

type Result<T> = std::result::Result<T, LaunchError>;

/// A command ready to run, inspected before it is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LaunchCommand {
    pub program: PathBuf,
    pub args: Vec<String>,
    pub working_dir: PathBuf,
}

impl LaunchCommand {
    /// A form safe to log or show a user.
    ///
    /// The access token is redacted. It is a live credential that grants the
    /// bearer the account's Minecraft session, and launcher logs are the single
    /// most commonly pasted file in any support thread.
    pub fn redacted(&self, session: &Session) -> Vec<String> {
        self.args
            .iter()
            .map(|arg| {
                if !session.access_token.is_empty() && arg.contains(&session.access_token) {
                    arg.replace(&session.access_token, "<redacted>")
                } else {
                    arg.clone()
                }
            })
            .collect()
    }
}

/// Everything the game needs that is not in the manifest.
pub struct LaunchContext<'a> {
    pub java: &'a Path,
    pub instance: &'a Instance,
    pub session: &'a Session,
    /// Classpath entries, absolute, in order. Usually paths into the store.
    pub classpath: &'a [PathBuf],
    /// Directory holding the shared `objects` and `indexes` asset folders.
    pub assets_root: &'a Path,
    /// Resolved asset index id, e.g. `29`.
    pub assets_index: &'a str,
    pub platform: &'a Platform,
}

/// Build the command without running it.
pub fn build(manifest: &VersionManifest, context: &LaunchContext<'_>) -> Result<LaunchCommand> {
    let main_class = manifest
        .main_class
        .as_deref()
        .ok_or(LaunchError::NoMainClass)?;

    let game_dir = context.instance.game_dir();
    let natives_dir = context.instance.natives_dir();
    let separator = classpath::separator(context.platform.os);

    let joined_classpath = context
        .classpath
        .iter()
        .map(|path| path.display().to_string())
        .collect::<Vec<_>>()
        .join(&separator.to_string());

    let config = context.instance.config();
    let mut features = Features::new();
    if config.window_width.is_some() && config.window_height.is_some() {
        features = features.with("has_custom_resolution", true);
    }

    let mut subs = Substitutions::new()
        .with("auth_player_name", &context.session.username)
        .with("auth_uuid", &context.session.uuid)
        .with("auth_access_token", &context.session.access_token)
        .with(
            "auth_session",
            format!("token:{}", context.session.access_token),
        )
        .with("user_type", &context.session.user_type)
        .with(
            "auth_xuid",
            context.session.xuid.clone().unwrap_or_default(),
        )
        .with("clientid", "")
        .with("version_name", &manifest.id)
        .with("version_type", &manifest.kind)
        .with("game_directory", game_dir.display().to_string())
        .with("assets_root", context.assets_root.display().to_string())
        .with("game_assets", context.assets_root.display().to_string())
        .with("assets_index_name", context.assets_index)
        .with("natives_directory", natives_dir.display().to_string())
        .with("classpath", &joined_classpath)
        .with("classpath_separator", separator.to_string())
        .with(
            "library_directory",
            context.assets_root.display().to_string(),
        )
        .with("launcher_name", LAUNCHER_NAME)
        .with("launcher_version", LAUNCHER_VERSION)
        .with("user_properties", "{}");

    if let (Some(width), Some(height)) = (config.window_width, config.window_height) {
        subs = subs
            .with("resolution_width", width.to_string())
            .with("resolution_height", height.to_string());
    }

    let mut jvm_args = memory_flags(config.memory_mb);

    match &manifest.arguments {
        Some(arguments) => {
            jvm_args.extend(args::resolve(
                &arguments.jvm,
                context.platform,
                &features,
                &subs,
            )?);
        }
        None => {
            // Pre-1.13 manifests carry no jvm arguments at all; the launcher is
            // expected to supply these two itself.
            jvm_args.push(format!("-Djava.library.path={}", natives_dir.display()));
            jvm_args.push("-cp".to_owned());
            jvm_args.push(joined_classpath.clone());
        }
    }

    // User arguments go last so they can override anything generated above -
    // that is the point of an "I know what I'm doing" field.
    jvm_args.extend(config.jvm_args.iter().cloned());

    let game_args = match (&manifest.arguments, &manifest.minecraft_arguments) {
        (Some(arguments), _) => args::resolve(&arguments.game, context.platform, &features, &subs)?,
        (None, Some(legacy)) => args::resolve_legacy(legacy, &subs)?,
        (None, None) => return Err(LaunchError::NoArguments),
    };

    let mut all = jvm_args;
    all.push(main_class.to_owned());
    all.extend(game_args);

    Ok(LaunchCommand {
        program: context.java.to_path_buf(),
        args: all,
        working_dir: game_dir,
    })
}

/// Heap flags.
///
/// Automatic sizing from system RAM, and GC tuning per heap size, belong to the
/// performance milestone. Until then a fixed, conservative default that works
/// on any machine is better than a clever one that does not.
fn memory_flags(memory_mb: Option<u32>) -> Vec<String> {
    let max = memory_mb.unwrap_or(4096);
    // Equal min and max avoids the JVM spending early game time growing the
    // heap, which shows up as stutter during world load.
    vec![format!("-Xms{max}M"), format!("-Xmx{max}M")]
}

/// Start the game.
///
/// Spawned directly, never through a shell. A shell would re-parse the
/// arguments we were careful to keep as a vector, and on Windows it would also
/// flash a console window.
pub fn spawn(command: &LaunchCommand) -> Result<std::process::Child> {
    let mut process = std::process::Command::new(&command.program);
    process
        .args(&command.args)
        .current_dir(&command.working_dir)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());

    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        // CREATE_NO_WINDOW: without it every launch flashes a console.
        process.creation_flags(0x0800_0000);
    }

    process.spawn().map_err(LaunchError::Spawn)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::instance::InstanceConfig;
    use ds_core::platform::{Arch, Os};

    fn platform() -> Platform {
        Platform::new(Os::Windows, Arch::X86_64, "10.0")
    }

    fn session() -> Session {
        Session {
            username: "Notch".to_owned(),
            uuid: "069a79f444e94726a5befca90e38aaf5".to_owned(),
            access_token: "SECRET-TOKEN-VALUE".to_owned(),
            user_type: "msa".to_owned(),
            xuid: Some("2535".to_owned()),
        }
    }

    fn modern() -> VersionManifest {
        VersionManifest::parse(
            r#"{
                "id":"1.21.11","type":"release",
                "mainClass":"net.minecraft.client.main.Main",
                "arguments":{
                    "game":["--username","${auth_player_name}","--uuid","${auth_uuid}",
                            "--accessToken","${auth_access_token}","--gameDir","${game_directory}",
                            "--assetsDir","${assets_root}","--assetIndex","${assets_index_name}",
                            {"rules":[{"action":"allow","features":{"has_custom_resolution":true}}],
                             "value":["--width","${resolution_width}","--height","${resolution_height}"]}],
                    "jvm":["-Djava.library.path=${natives_directory}","-cp","${classpath}"]
                }
            }"#,
        )
        .expect("manifest")
    }

    fn legacy() -> VersionManifest {
        VersionManifest::parse(
            r#"{
                "id":"1.5.2","type":"release",
                "mainClass":"net.minecraft.launchwrapper.Launch",
                "minecraftArguments":"--username ${auth_player_name} --gameDir ${game_directory} --assetsDir ${game_assets}"
            }"#,
        )
        .expect("manifest")
    }

    struct Fixture {
        _dir: tempfile::TempDir,
        instance: Instance,
        classpath: Vec<PathBuf>,
        assets: PathBuf,
    }

    fn fixture(config: InstanceConfig) -> Fixture {
        let dir = tempfile::tempdir().expect("temp");
        let instance = Instance::create(dir.path(), config).expect("instance");
        let classpath = vec![PathBuf::from("/store/a.jar"), PathBuf::from("/store/b.jar")];
        let assets = dir.path().join("assets");
        Fixture {
            _dir: dir,
            instance,
            classpath,
            assets,
        }
    }

    fn build_with(
        manifest: &VersionManifest,
        fixture: &Fixture,
        session: &Session,
    ) -> LaunchCommand {
        let platform = platform();
        build(
            manifest,
            &LaunchContext {
                java: Path::new("C:/java/bin/javaw.exe"),
                instance: &fixture.instance,
                session,
                classpath: &fixture.classpath,
                assets_root: &fixture.assets,
                assets_index: "29",
                platform: &platform,
            },
        )
        .expect("command should build")
    }

    #[test]
    fn the_command_has_jvm_args_then_main_class_then_game_args() {
        let f = fixture(InstanceConfig::new("Test", "1.21.11"));
        let command = build_with(&modern(), &f, &session());

        let main = command
            .args
            .iter()
            .position(|a| a == "net.minecraft.client.main.Main")
            .expect("main class must be present");

        assert!(command.args[..main].iter().any(|a| a == "-cp"));
        assert!(command.args[main..].iter().any(|a| a == "--username"));
    }

    #[test]
    fn session_details_reach_the_game_arguments() {
        let f = fixture(InstanceConfig::new("Test", "1.21.11"));
        let command = build_with(&modern(), &f, &session());

        assert!(command.args.contains(&"Notch".to_owned()));
        assert!(command
            .args
            .contains(&"069a79f444e94726a5befca90e38aaf5".to_owned()));
        assert!(command.args.contains(&"SECRET-TOKEN-VALUE".to_owned()));
    }

    /// Launcher logs are the most commonly pasted file in any support thread,
    /// and the access token in them is a live credential.
    #[test]
    fn the_access_token_is_redacted_for_logging() {
        let f = fixture(InstanceConfig::new("Test", "1.21.11"));
        let session = session();
        let command = build_with(&modern(), &f, &session);

        let redacted = command.redacted(&session);
        assert!(
            !redacted.iter().any(|a| a.contains("SECRET-TOKEN-VALUE")),
            "the token survived redaction: {redacted:?}"
        );
        assert!(redacted.iter().any(|a| a == "<redacted>"));
        // Everything else is untouched.
        assert!(redacted.contains(&"Notch".to_owned()));
    }

    #[test]
    fn the_game_runs_inside_its_own_instance_directory() {
        let f = fixture(InstanceConfig::new("Isolated", "1.21.11"));
        let command = build_with(&modern(), &f, &session());

        assert_eq!(command.working_dir, f.instance.game_dir());

        let game_dir = f.instance.game_dir().display().to_string();
        let index = command
            .args
            .iter()
            .position(|a| a == "--gameDir")
            .expect("--gameDir must be passed");
        assert_eq!(command.args[index + 1], game_dir);
    }

    #[test]
    fn the_classpath_is_joined_with_the_platform_separator() {
        let f = fixture(InstanceConfig::new("Test", "1.21.11"));
        let command = build_with(&modern(), &f, &session());

        let index = command.args.iter().position(|a| a == "-cp").unwrap();
        let classpath = &command.args[index + 1];
        assert!(
            classpath.contains(';'),
            "windows separator expected: {classpath}"
        );
        assert!(classpath.contains("a.jar") && classpath.contains("b.jar"));
    }

    #[test]
    fn memory_flags_default_and_can_be_overridden() {
        let f = fixture(InstanceConfig::new("Default", "1.21.11"));
        let command = build_with(&modern(), &f, &session());
        assert!(command.args.contains(&"-Xmx4096M".to_owned()));

        let mut config = InstanceConfig::new("Big", "1.21.11");
        config.memory_mb = Some(8192);
        let f = fixture(config);
        let command = build_with(&modern(), &f, &session());
        assert!(command.args.contains(&"-Xmx8192M".to_owned()));
        assert!(command.args.contains(&"-Xms8192M".to_owned()));
    }

    /// The "I know what I'm doing" field has to actually win.
    #[test]
    fn user_jvm_arguments_come_after_the_generated_ones() {
        let mut config = InstanceConfig::new("Tuned", "1.21.11");
        config.memory_mb = Some(4096);
        config.jvm_args = vec!["-XX:+UseZGC".to_owned(), "-Xmx2048M".to_owned()];
        let f = fixture(config);
        let command = build_with(&modern(), &f, &session());

        let generated = command.args.iter().position(|a| a == "-Xmx4096M").unwrap();
        let user = command.args.iter().position(|a| a == "-Xmx2048M").unwrap();
        assert!(
            user > generated,
            "a user override was placed before the default"
        );
        assert!(command.args.contains(&"-XX:+UseZGC".to_owned()));
    }

    /// Resolution arguments are gated on a feature flag, so they appear only
    /// when the instance actually sets a window size.
    #[test]
    fn window_size_arguments_appear_only_when_configured() {
        let f = fixture(InstanceConfig::new("Default", "1.21.11"));
        let command = build_with(&modern(), &f, &session());
        assert!(!command.args.contains(&"--width".to_owned()));

        let mut config = InstanceConfig::new("Sized", "1.21.11");
        config.window_width = Some(1600);
        config.window_height = Some(900);
        let f = fixture(config);
        let command = build_with(&modern(), &f, &session());

        assert!(command.args.contains(&"--width".to_owned()));
        assert!(command.args.contains(&"1600".to_owned()));
        assert!(command.args.contains(&"900".to_owned()));
    }

    /// Pre-1.13 versions have a single argument string and no jvm arguments at
    /// all; the launcher supplies the classpath and library path itself.
    #[test]
    fn a_legacy_version_still_builds_a_complete_command() {
        let f = fixture(InstanceConfig::new("Old", "1.5.2"));
        let command = build_with(&legacy(), &f, &session());

        assert!(command.args.iter().any(|a| a == "-cp"));
        assert!(command
            .args
            .iter()
            .any(|a| a.starts_with("-Djava.library.path=")));
        assert!(command
            .args
            .contains(&"net.minecraft.launchwrapper.Launch".to_owned()));
        assert!(command.args.contains(&"Notch".to_owned()));
    }

    /// A path with a space must stay one argument. This is the bug class the
    /// vector-not-string rule exists to prevent.
    #[test]
    fn paths_containing_spaces_remain_single_arguments() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("My Games/Deep Slate");
        std::fs::create_dir_all(&root).unwrap();
        let instance =
            Instance::create(&root, InstanceConfig::new("With Space", "1.21.11")).unwrap();

        let platform = platform();
        let command = build(
            &modern(),
            &LaunchContext {
                java: Path::new("C:/Program Files/Java/bin/javaw.exe"),
                instance: &instance,
                session: &session(),
                classpath: &[PathBuf::from("C:/Program Files/a.jar")],
                assets_root: &root.join("assets"),
                assets_index: "29",
                platform: &platform,
            },
        )
        .unwrap();

        let index = command.args.iter().position(|a| a == "--gameDir").unwrap();
        let game_dir = &command.args[index + 1];
        assert!(game_dir.contains(' '), "expected a space in {game_dir}");
        assert_eq!(
            *game_dir,
            instance.game_dir().display().to_string(),
            "the path was split or mangled"
        );
    }

    /// Exercises the real spawn path - process creation, working directory,
    /// piped output - without launching the game, which needs a session token.
    ///
    /// Skips rather than fails when no Java is installed: a machine without one
    /// is a legitimate state, not a broken test.
    #[test]
    fn the_spawn_path_starts_a_real_jvm_and_captures_its_output() {
        use std::io::Read as _;

        let runtimes = crate::java::discover(None);
        let Some(java) = runtimes.first() else {
            eprintln!("skipped: no Java installed on this machine");
            return;
        };

        let dir = tempfile::tempdir().unwrap();
        let command = LaunchCommand {
            program: java.executable.clone(),
            args: vec!["-version".to_owned()],
            working_dir: dir.path().to_path_buf(),
        };

        let mut child = spawn(&command).expect("the JVM should start");

        // -version writes to stderr, which is the same pipe the game's crash
        // output arrives on.
        let mut stderr = String::new();
        child
            .stderr
            .take()
            .expect("stderr must be piped")
            .read_to_string(&mut stderr)
            .expect("reading stderr");

        let status = child.wait().expect("the JVM should exit");

        assert!(status.success(), "java -version failed: {stderr}");
        assert!(
            stderr.to_lowercase().contains("version"),
            "no version banner captured: {stderr}"
        );
    }

    /// A launch into a directory that does not exist must fail with a real
    /// error rather than silently starting somewhere else.
    #[test]
    fn spawning_into_a_missing_directory_is_an_error() {
        let runtimes = crate::java::discover(None);
        let Some(java) = runtimes.first() else {
            return;
        };

        let command = LaunchCommand {
            program: java.executable.clone(),
            args: vec!["-version".to_owned()],
            working_dir: PathBuf::from("/definitely/not/a/real/directory"),
        };

        assert!(matches!(spawn(&command), Err(LaunchError::Spawn(_))));
    }

    #[test]
    fn a_manifest_with_no_main_class_is_refused() {
        let manifest = VersionManifest::parse(r#"{"id":"x","type":"release"}"#).unwrap();
        let f = fixture(InstanceConfig::new("Broken", "x"));
        let platform = platform();

        let result = build(
            &manifest,
            &LaunchContext {
                java: Path::new("java"),
                instance: &f.instance,
                session: &session(),
                classpath: &f.classpath,
                assets_root: &f.assets,
                assets_index: "29",
                platform: &platform,
            },
        );
        assert!(matches!(result, Err(LaunchError::NoMainClass)));
    }
}
