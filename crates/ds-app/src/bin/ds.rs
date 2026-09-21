//! `ds` - development CLI.
//!
//! Exists so the sign-in chain is exercisable before any UI does. Not shipped
//! to users; the GUI is the product.
//!
//! Argument parsing is hand-rolled rather than pulled from `clap`. Four
//! subcommands with no flags does not justify a dependency and its whole macro
//! tree in a binary that never leaves a developer's machine.

use std::path::PathBuf;
use std::process::ExitCode;

use ds_auth::store::{AccountStore, Keychain};
use ds_auth::{AuthError, Flow};
use ds_core::platform::Platform;
use ds_core::rules::Features;
use ds_mc::Catalog;
use ds_net::Downloader;
use ds_store::Store;

const USAGE: &str = "\
ds - Deepslate development CLI

USAGE:
    ds <COMMAND>

COMMANDS:
    login             Sign in with a Microsoft account through the browser
    whoami            Resume the active account from its stored refresh token
    accounts          List stored accounts
    logout <uuid>     Forget an account and destroy its stored credential
    switch <uuid>     Make an account the active one
    versions [n]      List the newest releases from Mojang
    resolve <id>      Fetch a version, resolve inheritance, summarise it
    prepare <id>      Download everything a version needs
    java [major]      List detected Java runtimes, or pick one for a major version
    java-install <c>  Download a Mojang Java runtime, e.g. jre-legacy
    instances         List instances
    new <name> <ver>  Create an instance
    dry-run <slug>    Build the launch command for an instance and print it
";

/// `%APPDATA%/Deepslate` on Windows, the platform equivalent elsewhere.
fn data_dir() -> Result<PathBuf, String> {
    dirs::data_dir()
        .map(|dir| dir.join("Deepslate"))
        .ok_or_else(|| "could not determine this platform's application data directory".to_owned())
}

fn accounts_path() -> Result<PathBuf, String> {
    Ok(data_dir()?.join("accounts.json"))
}

fn open_store() -> Result<AccountStore<Keychain>, String> {
    AccountStore::load(accounts_path()?, Keychain).map_err(|error| error.to_string())
}

#[tokio::main]
async fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let command = args.first().map(String::as_str);

    let result = match (command, args.get(1)) {
        (Some("login"), _) => login().await,
        (Some("whoami"), _) => whoami().await,
        (Some("accounts"), _) => accounts(),
        (Some("versions"), count) => versions(count.map(String::as_str)).await,
        (Some("resolve"), Some(id)) => resolve_version(id).await,
        (Some("resolve"), None) => Err("that command needs a version id".to_owned()),
        (Some("prepare"), Some(id)) => prepare_version(id).await,
        (Some("prepare"), None) => Err("that command needs a version id".to_owned()),
        (Some("java"), major) => java_runtimes(major.map(String::as_str)),
        (Some("java-install"), Some(component)) => java_install(component).await,
        (Some("java-install"), None) => {
            Err("that command needs a component, e.g. jre-legacy".to_owned())
        }
        (Some("instances"), _) => list_instances(),
        (Some("new"), Some(name)) => new_instance(name, args.get(2).map(String::as_str)),
        (Some("new"), None) => Err("that command needs a name and a version".to_owned()),
        (Some("dry-run"), Some(slug)) => dry_run(slug).await,
        (Some("dry-run"), None) => Err("that command needs an instance slug".to_owned()),
        (Some("logout"), Some(id)) => logout(id),
        (Some("switch"), Some(id)) => switch(id),
        (Some("logout" | "switch"), None) => Err("that command needs an account uuid".to_owned()),
        (Some("-h" | "--help" | "help"), _) | (None, _) => {
            print!("{USAGE}");
            return ExitCode::SUCCESS;
        }
        (Some(other), _) => Err(format!("unknown command '{other}'\n\n{USAGE}")),
    };

    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(message) => {
            eprintln!("error: {message}");
            ExitCode::FAILURE
        }
    }
}

fn cache_dir() -> Result<PathBuf, String> {
    // Overridable so a benchmark can run against a throwaway store rather than
    // the real cache, where everything would already be present.
    if let Ok(dir) = std::env::var("DEEPSLATE_CACHE") {
        return Ok(PathBuf::from(dir));
    }
    dirs::cache_dir()
        .map(|dir| dir.join("Deepslate"))
        .ok_or_else(|| "could not determine this platform's cache directory".to_owned())
}

async fn versions(count: Option<&str>) -> Result<(), String> {
    let limit: usize = count.unwrap_or("15").parse().unwrap_or(15);

    let downloader = Downloader::default();
    let catalog = Catalog::load(&downloader)
        .await
        .map_err(|e| e.to_string())?;
    let list = catalog.list();

    println!("latest release  : {}", list.latest.release);
    println!("latest snapshot : {}", list.latest.snapshot);
    println!("known versions  : {}", list.versions.len());
    println!();

    for entry in list.releases().into_iter().take(limit) {
        println!("  {:<12} {}", entry.id, &entry.release_time[..10]);
    }
    Ok(())
}

async fn resolve_version(id: &str) -> Result<(), String> {
    let store = Store::open(cache_dir()?).map_err(|e| e.to_string())?;
    let downloader = Downloader::default();

    println!("Fetching the version list...");
    let catalog = Catalog::load(&downloader)
        .await
        .map_err(|e| e.to_string())?;

    println!("Resolving {id}...");
    let manifest = catalog
        .resolved(&downloader, &store, id)
        .await
        .map_err(|e| e.to_string())?;

    let platform = Platform::host().ok_or("unsupported platform")?;
    let features = Features::new();
    let applicable = manifest.applicable_libraries(&platform, &features);

    println!();
    println!("id            : {}", manifest.id);
    println!("type          : {}", manifest.kind);
    println!("released      : {}", manifest.release_time);
    println!(
        "main class    : {}",
        manifest.main_class.as_deref().unwrap_or("(none)")
    );
    match &manifest.java_version {
        Some(java) => println!(
            "java          : {} (major {})",
            java.component, java.major_version
        ),
        None => println!("java          : not declared, defaults to 8"),
    }
    println!(
        "assets        : {}",
        manifest.assets.as_deref().unwrap_or("(none)")
    );
    println!("legacy assets : {}", manifest.uses_legacy_assets());
    println!(
        "libraries     : {} total, {} apply on {}",
        manifest.libraries.len(),
        applicable.len(),
        platform.os
    );

    match manifest.client_download() {
        Some(client) => println!("client jar    : {:.1} MB", client.size as f64 / 1_048_576.0),
        None => println!("client jar    : (none declared)"),
    }

    let classpath = ds_core::classpath::entries(&manifest, &platform, &features, None)
        .map_err(|e| e.to_string())?;
    println!("classpath     : {} entries", classpath.len());
    println!("cache         : {}", store.root().display());
    Ok(())
}

fn mib(bytes: u64) -> String {
    format!("{:.1} MB", bytes as f64 / 1_048_576.0)
}

async fn prepare_version(id: &str) -> Result<(), String> {
    let store = Store::open(cache_dir()?).map_err(|e| e.to_string())?;
    let downloader = Downloader::default();

    let catalog = Catalog::load(&downloader)
        .await
        .map_err(|e| e.to_string())?;
    let manifest = catalog
        .resolved(&downloader, &store, id)
        .await
        .map_err(|e| e.to_string())?;

    let platform = Platform::host().ok_or("unsupported platform")?;
    let features = Features::new();

    let before = store.size_on_disk().unwrap_or(0);
    let work = ds_mc::plan(&manifest, &platform, &features);
    println!(
        "{id}: {} libraries, {} natives, client jar {}",
        work.libraries.len(),
        work.natives.len(),
        work.client
            .as_ref()
            .map(|c| mib(c.size))
            .unwrap_or_else(|| "none".into())
    );
    println!("Fetching asset index and downloading...");

    let started = std::time::Instant::now();
    let mut last = 0_u64;

    let prepared = ds_mc::prepare(
        &downloader,
        &store,
        manifest,
        &platform,
        &features,
        |progress| {
            // One line every 250 files: enough to see movement, not enough to
            // make the terminal the bottleneck.
            if progress.completed - last >= 250 || progress.completed == progress.total {
                last = progress.completed;
                println!("  {} / {} files", progress.completed, progress.total);
            }
        },
    )
    .await
    .map_err(|e| e.to_string())?;

    let elapsed = started.elapsed();
    let after = store.size_on_disk().unwrap_or(0);

    println!();
    println!("done in {:.1}s", elapsed.as_secs_f64());
    println!("classpath      : {} entries", prepared.classpath.len());
    println!("native jars    : {}", prepared.native_jars.len());

    match (&prepared.asset_index, &prepared.asset_index_id) {
        (Some(index), Some(index_id)) => {
            println!(
                "assets         : {} objects, {} distinct, {} ({:?} layout, index {index_id})",
                index.objects.len(),
                index.artifacts().len(),
                mib(index.total_size()),
                index.layout()
            );
        }
        _ => println!("assets         : none declared"),
    }

    println!("cache was      : {}", mib(before));
    println!("cache now      : {}", mib(after));
    println!("fetched this run: {}", mib(after.saturating_sub(before)));
    println!("store          : {}", store.root().display());
    Ok(())
}

fn java_runtimes(major: Option<&str>) -> Result<(), String> {
    let found = ds_mc::java::discover(cache_dir().ok().as_deref());

    if found.is_empty() {
        println!("No Java runtimes found.");
        return Ok(());
    }

    println!("{} runtime(s) found:", found.len());
    for installation in &found {
        println!(
            "  Java {:<3} {:<12} {:<16} {}",
            installation.major,
            installation.version,
            format!("{:?}", installation.source),
            installation.executable.display()
        );
    }

    if let Some(major) = major {
        let wanted: u32 = major
            .parse()
            .map_err(|_| format!("'{major}' is not a major version"))?;
        println!();
        match ds_mc::java::select(&found, wanted) {
            Some(chosen) => println!(
                "for Java {wanted}: {} ({})",
                chosen.executable.display(),
                chosen.version
            ),
            None => {
                println!("for Java {wanted}: nothing installed matches - it would be downloaded")
            }
        }
    }
    Ok(())
}

fn instances_dir() -> Result<PathBuf, String> {
    Ok(data_dir()?.join("instances"))
}

fn list_instances() -> Result<(), String> {
    let all = ds_mc::Instance::list(&instances_dir()?);
    if all.is_empty() {
        println!("No instances. Create one with: ds new <name> <version>");
        return Ok(());
    }
    for instance in &all {
        println!(
            "  {:<20} {:<12} {}",
            instance.slug(),
            instance.config().version,
            instance.root().display()
        );
    }
    Ok(())
}

fn new_instance(name: &str, version: Option<&str>) -> Result<(), String> {
    let version = version.ok_or("that command needs a version, e.g. ds new Test 1.21.11")?;
    let root = instances_dir()?;
    std::fs::create_dir_all(&root).map_err(|e| e.to_string())?;

    let instance = ds_mc::Instance::create(&root, ds_mc::InstanceConfig::new(name, version))
        .map_err(|e| e.to_string())?;

    println!("Created '{}' ({})", instance.slug(), version);
    println!("  game dir : {}", instance.game_dir().display());
    println!("  natives  : {}", instance.natives_dir().display());
    println!("  settings : {}", instance.config_path().display());
    Ok(())
}

/// Build the exact command an instance would launch with, and print it.
///
/// Does not start the game: that needs a real session, which needs Mojang to
/// approve the app registration. Everything up to the spawn is verifiable now.
async fn dry_run(slug: &str) -> Result<(), String> {
    let instance = ds_mc::Instance::load(&instances_dir()?, slug).map_err(|e| e.to_string())?;
    let version_id = instance.config().version.clone();

    let store = Store::open(cache_dir()?).map_err(|e| e.to_string())?;
    let downloader = Downloader::default();
    let catalog = Catalog::load(&downloader)
        .await
        .map_err(|e| e.to_string())?;
    let manifest = catalog
        .resolved(&downloader, &store, &version_id)
        .await
        .map_err(|e| e.to_string())?;

    let platform = Platform::host().ok_or("unsupported platform")?;
    let features = Features::new();

    let required = manifest
        .java_version
        .as_ref()
        .map(|j| j.major_version)
        .unwrap_or(8);
    let runtimes = ds_mc::java::discover(Some(store.root()));
    let java =
        ds_mc::java::for_instance(instance.config().java_path.as_deref(), &runtimes, required)
            .ok_or_else(|| format!("no Java {required} installed; it would be downloaded"))?
            .to_path_buf();

    let work = ds_mc::plan(&manifest, &platform, &features);
    let mut classpath: Vec<PathBuf> = Vec::new();
    for artifact in &work.libraries {
        classpath.push(
            store
                .path_for(&artifact.hash, ds_store::Algorithm::Sha1)
                .map_err(|e| e.to_string())?,
        );
    }
    if let Some(client) = &work.client {
        classpath.push(
            store
                .path_for(&client.hash, ds_store::Algorithm::Sha1)
                .map_err(|e| e.to_string())?,
        );
    }

    // A placeholder session. This build has no path that launches the game with
    // one: the command is printed, never spawned.
    let session = ds_mc::Session {
        username: "<player>".to_owned(),
        uuid: "<uuid>".to_owned(),
        access_token: "<token>".to_owned(),
        user_type: "msa".to_owned(),
        xuid: None,
    };

    let assets_root = store.root().join("assets");
    let command = ds_mc::launch::build(
        &manifest,
        &ds_mc::LaunchContext {
            java: &java,
            instance: &instance,
            session: &session,
            classpath: &classpath,
            assets_root: &assets_root,
            assets_index: manifest.assets.as_deref().unwrap_or("legacy"),
            platform: &platform,
        },
    )
    .map_err(|e| e.to_string())?;

    println!("instance   : {} ({})", instance.slug(), version_id);
    println!("java       : {} (needs Java {required})", java.display());
    println!("classpath  : {} entries", classpath.len());
    println!("working dir: {}", command.working_dir.display());
    println!("arguments  : {} total", command.args.len());
    println!();

    let redacted = command.redacted(&session);
    let main = redacted
        .iter()
        .position(|a| Some(a.as_str()) == manifest.main_class.as_deref());

    println!("--- JVM arguments ---");
    for arg in &redacted[..main.unwrap_or(0)] {
        // The classpath is thousands of characters; show its shape instead.
        if arg.len() > 120 {
            println!(
                "  <{} chars: {} entries>",
                arg.len(),
                arg.matches(';').count() + 1
            );
        } else {
            println!("  {arg}");
        }
    }
    if let Some(main) = main {
        println!("--- main class ---");
        println!("  {}", redacted[main]);
        println!("--- game arguments ---");
        for arg in &redacted[main + 1..] {
            println!("  {arg}");
        }
    }

    println!();
    println!("Not launched: that needs a real session token.");
    Ok(())
}

async fn java_install(component: &str) -> Result<(), String> {
    let store = Store::open(cache_dir()?).map_err(|e| e.to_string())?;
    let downloader = Downloader::default();
    let platform = Platform::host().ok_or("unsupported platform")?;

    println!(
        "Installing {component} for {} {}...",
        platform.os, platform.arch
    );

    let started = std::time::Instant::now();
    let mut last = 0_u64;

    let home = ds_mc::runtime::install(&downloader, &store, component, &platform, |progress| {
        if progress.completed - last >= 50 || progress.completed == progress.total {
            last = progress.completed;
            println!("  {} / {} files", progress.completed, progress.total);
        }
    })
    .await
    .map_err(|e| e.to_string())?;

    println!();
    println!("installed in {:.1}s", started.elapsed().as_secs_f64());
    println!("home: {}", home.display());

    // The proof that it worked is that discovery can read it back.
    match ds_mc::java::from_home(&home, ds_mc::JavaSource::Managed) {
        Some(found) => println!("verified: Java {} ({})", found.major, found.version),
        None => println!("WARNING: installed, but not recognisable as a Java home"),
    }
    Ok(())
}

async fn login() -> Result<(), String> {
    println!("Opening your browser to sign in with Microsoft...");
    println!("(if nothing opens, check behind this window)");

    let signed_in = Flow::new().sign_in().await.map_err(describe)?;

    let mut store = open_store()?;
    let account = signed_in.account();

    match signed_in.refresh_token.as_deref() {
        Some(token) => {
            store
                .upsert(account.clone(), token)
                .map_err(|error| error.to_string())?;
            println!("\nSigned in as {} ({})", account.name, account.id);
            println!("Refresh token stored in the system keychain.");
        }
        None => {
            // Honest about the consequence rather than silently storing nothing
            // and failing confusingly on the next launch.
            println!("\nSigned in as {} ({})", account.name, account.id);
            println!(
                "Microsoft issued no refresh token, so this session was not saved - \
                 signing in again will be required next time."
            );
        }
    }

    Ok(())
}

async fn whoami() -> Result<(), String> {
    let store = open_store()?;
    let Some(account) = store.active() else {
        return Err("no active account; run `ds login` first".to_owned());
    };

    let Some(refresh) = store
        .refresh_token(&account.id)
        .map_err(|error| error.to_string())?
    else {
        return Err(format!(
            "no stored credential for {}; run `ds login` again",
            account.name
        ));
    };

    println!("Resuming {} from the stored refresh token...", account.name);
    let signed_in = Flow::new().resume(&refresh).await.map_err(describe)?;

    println!("\nUsername : {}", signed_in.profile.name);
    println!("UUID     : {}", signed_in.profile.id);
    println!(
        "Session  : valid for {} seconds",
        signed_in.session.expires_in_secs
    );
    Ok(())
}

fn accounts() -> Result<(), String> {
    let store = open_store()?;
    let active = store.active().map(|a| a.id.clone());

    if store.accounts().is_empty() {
        println!("No accounts. Run `ds login`.");
        return Ok(());
    }

    for account in store.accounts() {
        let marker = if Some(&account.id) == active.as_ref() {
            "*"
        } else {
            " "
        };
        println!("{marker} {:<18} {}", account.name, account.id);
    }
    Ok(())
}

fn logout(id: &str) -> Result<(), String> {
    let mut store = open_store()?;
    if store.remove(id).map_err(|error| error.to_string())? {
        println!("Removed {id} and destroyed its stored credential.");
        Ok(())
    } else {
        Err(format!("no account with uuid {id}"))
    }
}

fn switch(id: &str) -> Result<(), String> {
    let mut store = open_store()?;
    if store.set_active(id).map_err(|error| error.to_string())? {
        println!("Active account is now {id}.");
        Ok(())
    } else {
        Err(format!("no account with uuid {id}"))
    }
}

/// Turn an auth failure into something worth reading.
///
/// The error messages are already actionable; this adds the one piece of
/// context the CLI knows and the library does not - what the operator should do
/// about it right now.
fn describe(error: AuthError) -> String {
    let hint = match &error {
        AuthError::AppNotApproved => Some(
            "This build's Azure application has not been approved for the Minecraft API yet.\n       \
             This is expected until Mojang approves the request; see docs/microsoft-login-setup.md.",
        ),
        AuthError::NoProfile => {
            Some("Choose a username at minecraft.net, then run this again.")
        }
        AuthError::ChildAccount => {
            Some("An adult must add this account to a Microsoft Family at account.microsoft.com/family.")
        }
        AuthError::NoXboxAccount => Some("Create an Xbox profile at xbox.com, then run this again."),
        AuthError::RefreshExpired => Some("Run `ds login` to sign in again."),
        AuthError::XstsUnknown { xerr } => {
            // Deliberately surfaced: an unmapped code is still actionable if the
            // number reaches the person who can search for it.
            return format!(
                "{error}\n       Please report this code ({xerr}) - it is not one we have seen."
            );
        }
        _ => None,
    };

    match hint {
        Some(hint) => format!("{error}\n       {hint}"),
        None => error.to_string(),
    }
}
