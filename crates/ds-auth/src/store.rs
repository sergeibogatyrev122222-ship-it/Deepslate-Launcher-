//! Where accounts and their refresh tokens live.
//!
//! The split is the whole point:
//!
//! - **Refresh tokens** go to the OS keychain and nowhere else. They are
//!   long-lived credentials that can mint game sessions, so they never touch a
//!   file we write.
//! - **Everything else** - username, UUID, which account was last used - is
//!   ordinary non-secret metadata in a plain JSON file the user can read,
//!   back up, or delete.
//!
//! Access tokens are in neither. They live in memory for their lifetime and are
//! deliberately never persisted.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Mutex;

use serde::{Deserialize, Serialize};

use crate::error::{AuthError, Result};

/// The keychain service name all entries are filed under.
const SERVICE: &str = "dev.deepslate.launcher";

/// A stored account. Contains nothing secret.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Account {
    /// Minecraft profile UUID, dashless, as the API returns it.
    pub id: String,
    /// Current username. Can change; refreshed on every successful sign-in.
    pub name: String,
    /// Unix seconds. Plain integer rather than a formatted timestamp so the
    /// file stays trivially machine-readable and never depends on a locale.
    pub added_at: i64,
    pub last_used: i64,
}

/// The on-disk shape of `accounts.json`.
#[derive(Debug, Default, Serialize, Deserialize)]
struct AccountsFile {
    accounts: Vec<Account>,
    /// UUID of the account to use when launching.
    active: Option<String>,
}

/// Somewhere to keep a secret.
///
/// A trait so tests can run against memory instead of the real keychain -
/// otherwise every test run would write credentials into the developer's
/// Windows Credential Manager, which is both rude and unreliable in CI.
pub trait SecretStore: Send + Sync {
    fn set(&self, key: &str, secret: &str) -> Result<()>;
    fn get(&self, key: &str) -> Result<Option<String>>;
    fn delete(&self, key: &str) -> Result<()>;
}

/// The real thing: Windows Credential Manager, libsecret, or macOS Keychain.
#[derive(Debug, Default)]
pub struct Keychain;

impl SecretStore for Keychain {
    fn set(&self, key: &str, secret: &str) -> Result<()> {
        keyring::Entry::new(SERVICE, key)
            .and_then(|entry| entry.set_password(secret))
            .map_err(|source| AuthError::Keychain { source })
    }

    fn get(&self, key: &str) -> Result<Option<String>> {
        match keyring::Entry::new(SERVICE, key).and_then(|entry| entry.get_password()) {
            Ok(secret) => Ok(Some(secret)),
            // Absent is a normal state, not a failure: it simply means this
            // account has to sign in interactively.
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(source) => Err(AuthError::Keychain { source }),
        }
    }

    fn delete(&self, key: &str) -> Result<()> {
        match keyring::Entry::new(SERVICE, key).and_then(|entry| entry.delete_credential()) {
            Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
            Err(source) => Err(AuthError::Keychain { source }),
        }
    }
}

/// In-memory secrets, for tests only.
#[derive(Debug, Default)]
pub struct MemorySecrets {
    entries: Mutex<HashMap<String, String>>,
}

impl SecretStore for MemorySecrets {
    fn set(&self, key: &str, secret: &str) -> Result<()> {
        if let Ok(mut entries) = self.entries.lock() {
            entries.insert(key.to_owned(), secret.to_owned());
        }
        Ok(())
    }

    fn get(&self, key: &str) -> Result<Option<String>> {
        Ok(self
            .entries
            .lock()
            .ok()
            .and_then(|entries| entries.get(key).cloned()))
    }

    fn delete(&self, key: &str) -> Result<()> {
        if let Ok(mut entries) = self.entries.lock() {
            entries.remove(key);
        }
        Ok(())
    }
}

/// Accounts plus their secrets, kept consistent with each other.
pub struct AccountStore<S: SecretStore> {
    path: PathBuf,
    secrets: S,
    file: AccountsFile,
}

impl<S: SecretStore> AccountStore<S> {
    /// Load from disk, or start empty if the file is not there yet.
    ///
    /// A corrupt file is an error rather than a silent reset: quietly throwing
    /// away someone's account list because a byte flipped is worse than saying
    /// so.
    pub fn load(path: impl Into<PathBuf>, secrets: S) -> Result<Self> {
        let path = path.into();
        let file = match std::fs::read_to_string(&path) {
            Ok(text) => serde_json::from_str(&text).map_err(|error| AuthError::Protocol {
                stage: crate::error::Stage::Microsoft,
                detail: format!("{} is not valid JSON: {error}", path.display()),
            })?,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => AccountsFile::default(),
            Err(source) => return Err(AuthError::Loopback { source }),
        };
        Ok(Self {
            path,
            secrets,
            file,
        })
    }

    pub fn accounts(&self) -> &[Account] {
        &self.file.accounts
    }

    pub fn active(&self) -> Option<&Account> {
        let id = self.file.active.as_deref()?;
        self.file.accounts.iter().find(|a| a.id == id)
    }

    /// Add an account, or update one already present.
    ///
    /// Re-signing in with an account that already exists must not duplicate it,
    /// and must pick up a username change.
    pub fn upsert(&mut self, account: Account, refresh_token: &str) -> Result<()> {
        self.secrets.set(&account.id, refresh_token)?;

        match self.file.accounts.iter_mut().find(|a| a.id == account.id) {
            Some(existing) => {
                existing.name = account.name;
                existing.last_used = account.last_used;
            }
            None => self.file.accounts.push(account.clone()),
        }

        if self.file.active.is_none() {
            self.file.active = Some(account.id);
        }
        self.persist()
    }

    pub fn refresh_token(&self, account_id: &str) -> Result<Option<String>> {
        self.secrets.get(account_id)
    }

    pub fn set_active(&mut self, account_id: &str) -> Result<bool> {
        if !self.file.accounts.iter().any(|a| a.id == account_id) {
            return Ok(false);
        }
        self.file.active = Some(account_id.to_owned());
        self.persist()?;
        Ok(true)
    }

    /// Forget an account, secret included.
    ///
    /// The secret goes first. If the process dies midway, an orphaned metadata
    /// entry with no token is recoverable - the user signs in again. An
    /// orphaned *token* with no metadata is a credential nobody can see or
    /// remove.
    pub fn remove(&mut self, account_id: &str) -> Result<bool> {
        if !self.file.accounts.iter().any(|a| a.id == account_id) {
            return Ok(false);
        }
        self.secrets.delete(account_id)?;
        self.file.accounts.retain(|a| a.id != account_id);
        if self.file.active.as_deref() == Some(account_id) {
            self.file.active = self.file.accounts.first().map(|a| a.id.clone());
        }
        self.persist()?;
        Ok(true)
    }

    /// Write atomically: a temporary file, then a rename.
    ///
    /// Writing in place risks a truncated account list if the process is killed
    /// mid-write.
    fn persist(&self) -> Result<()> {
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent).map_err(|source| AuthError::Loopback { source })?;
        }

        let json =
            serde_json::to_string_pretty(&self.file).map_err(|error| AuthError::Protocol {
                stage: crate::error::Stage::Microsoft,
                detail: format!("could not serialise accounts: {error}"),
            })?;

        let temp = self.path.with_extension("json.tmp");
        std::fs::write(&temp, json).map_err(|source| AuthError::Loopback { source })?;
        std::fs::rename(&temp, &self.path).map_err(|source| AuthError::Loopback { source })
    }
}

/// Current time as Unix seconds.
pub fn now_unix() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn account(id: &str, name: &str) -> Account {
        Account {
            id: id.to_owned(),
            name: name.to_owned(),
            added_at: 1_700_000_000,
            last_used: 1_700_000_000,
        }
    }

    fn temp_path(label: &str) -> PathBuf {
        let mut path = std::env::temp_dir();
        path.push(format!(
            "deepslate-test-{label}-{}.json",
            std::process::id()
        ));
        let _unused = std::fs::remove_file(&path);
        path
    }

    fn store(label: &str) -> AccountStore<MemorySecrets> {
        AccountStore::load(temp_path(label), MemorySecrets::default()).unwrap()
    }

    #[test]
    fn missing_file_starts_empty_rather_than_failing() {
        let store = store("missing");
        assert!(store.accounts().is_empty());
        assert!(store.active().is_none());
    }

    #[test]
    fn first_account_added_becomes_active() {
        let mut store = store("first-active");
        store
            .upsert(account("uuid-a", "Alice"), "refresh-a")
            .unwrap();
        assert_eq!(store.active().map(|a| a.name.as_str()), Some("Alice"));
    }

    #[test]
    fn second_account_does_not_steal_focus() {
        let mut store = store("second");
        store.upsert(account("uuid-a", "Alice"), "ra").unwrap();
        store.upsert(account("uuid-b", "Bob"), "rb").unwrap();

        assert_eq!(store.accounts().len(), 2);
        assert_eq!(store.active().map(|a| a.id.as_str()), Some("uuid-a"));
    }

    /// Signing in again with the same account must update it, not clone it.
    #[test]
    fn re_signin_updates_in_place_and_picks_up_a_rename() {
        let mut store = store("upsert");
        store.upsert(account("uuid-a", "OldName"), "r1").unwrap();

        let mut renamed = account("uuid-a", "NewName");
        renamed.last_used = 1_800_000_000;
        store.upsert(renamed, "r2").unwrap();

        assert_eq!(store.accounts().len(), 1, "account was duplicated");
        assert_eq!(store.accounts()[0].name, "NewName");
        assert_eq!(store.accounts()[0].last_used, 1_800_000_000);
        assert_eq!(
            store.refresh_token("uuid-a").unwrap().as_deref(),
            Some("r2"),
            "refresh token was not replaced"
        );
    }

    #[test]
    fn removing_the_active_account_promotes_another() {
        let mut store = store("remove-active");
        store.upsert(account("uuid-a", "Alice"), "ra").unwrap();
        store.upsert(account("uuid-b", "Bob"), "rb").unwrap();

        assert!(store.remove("uuid-a").unwrap());
        assert_eq!(store.active().map(|a| a.id.as_str()), Some("uuid-b"));
    }

    /// The credential must not outlive the account it belongs to.
    #[test]
    fn removing_an_account_destroys_its_secret() {
        let mut store = store("remove-secret");
        store.upsert(account("uuid-a", "Alice"), "secret").unwrap();
        assert!(store.refresh_token("uuid-a").unwrap().is_some());

        store.remove("uuid-a").unwrap();
        assert!(
            store.refresh_token("uuid-a").unwrap().is_none(),
            "refresh token survived account removal"
        );
    }

    #[test]
    fn removing_an_unknown_account_is_not_an_error() {
        let mut store = store("remove-unknown");
        assert!(!store.remove("nope").unwrap());
    }

    #[test]
    fn switching_to_an_unknown_account_is_refused() {
        let mut store = store("switch-unknown");
        store.upsert(account("uuid-a", "Alice"), "ra").unwrap();
        assert!(!store.set_active("ghost").unwrap());
        assert_eq!(store.active().map(|a| a.id.as_str()), Some("uuid-a"));
    }

    #[test]
    fn accounts_survive_a_reload() {
        let path = temp_path("reload");
        {
            let mut store = AccountStore::load(path.clone(), MemorySecrets::default()).unwrap();
            store.upsert(account("uuid-a", "Alice"), "ra").unwrap();
            store.upsert(account("uuid-b", "Bob"), "rb").unwrap();
            store.set_active("uuid-b").unwrap();
        }

        let reloaded = AccountStore::load(path.clone(), MemorySecrets::default()).unwrap();
        assert_eq!(reloaded.accounts().len(), 2);
        assert_eq!(reloaded.active().map(|a| a.id.as_str()), Some("uuid-b"));
        let _unused = std::fs::remove_file(&path);
    }

    /// The file is the user's to read, and must never contain a credential.
    #[test]
    fn the_accounts_file_contains_no_secrets() {
        let path = temp_path("no-secrets");
        let mut store = AccountStore::load(path.clone(), MemorySecrets::default()).unwrap();
        store
            .upsert(account("uuid-a", "Alice"), "super-secret-refresh-token")
            .unwrap();

        let written = std::fs::read_to_string(&path).unwrap();
        assert!(
            !written.contains("super-secret-refresh-token"),
            "refresh token was written to disk:\n{written}"
        );
        assert!(written.contains("Alice"));
        let _unused = std::fs::remove_file(&path);
    }

    #[test]
    fn a_corrupt_file_is_reported_not_silently_discarded() {
        let path = temp_path("corrupt");
        std::fs::write(&path, "{ this is not json").unwrap();

        let result = AccountStore::load(path.clone(), MemorySecrets::default());
        assert!(result.is_err(), "corrupt file should not load as empty");
        let _unused = std::fs::remove_file(&path);
    }
}
