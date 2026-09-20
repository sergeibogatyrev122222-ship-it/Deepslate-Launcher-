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
