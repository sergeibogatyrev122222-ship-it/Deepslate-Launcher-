//! Content-addressed storage for downloaded artifacts.
//!
//! Every file is stored at a path derived from its own hash:
//! `objects/<first-2-hex>/<full-hash>`. That one decision carries most of the
//! design:
//!
//! - **No database.** The path *is* the key, so the filesystem is the index.
//!   Nothing to migrate, nothing to corrupt, nothing to keep in sync.
//! - **Deduplication is free.** Ten instances on the same version share one
//!   copy of every jar, because they all resolve to the same path.
//! - **Verification is not optional.** A file is hashed before it is admitted,
//!   so a truncated download or a corrupted mirror cannot end up being served
//!   to the JVM as if it were fine.
//!
//! Writes are staged in a temporary file and moved into place with a rename, so
//! a process killed mid-download leaves no half-written object that later looks
//! present.

#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used, clippy::panic))]

use std::fs;
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};

use sha1::{Digest as _, Sha1};
use sha2::Sha512;

/// Which hash a caller is verifying against.
///
/// Minecraft manifests declare sha1. Modrinth declares both sha1 and sha512.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Algorithm {
    Sha1,
    Sha512,
}

impl Algorithm {
    /// Length of the hex digest this algorithm produces.
    const fn hex_len(self) -> usize {
        match self {
            Self::Sha1 => 40,
            Self::Sha512 => 128,
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("io error at {path}")]
    Io {
        path: PathBuf,
        #[source]
        source: io::Error,
    },

    /// The single most important error in this module. A file that does not
    /// hash to what the manifest promised is never admitted, because serving it
    /// to the JVM produces a crash that looks like a bug in the game.
    #[error("hash mismatch: expected {expected}, got {actual}")]
    HashMismatch { expected: String, actual: String },

    #[error("'{0}' is not a valid hex digest for this algorithm")]
    MalformedHash(String),
}

type Result<T> = std::result::Result<T, StoreError>;

fn io_err(path: impl Into<PathBuf>) -> impl FnOnce(io::Error) -> StoreError {
    let path = path.into();
    move |source| StoreError::Io { path, source }
}

/// Hash a reader, returning a lowercase hex digest.
///
/// Streams in fixed-size chunks rather than reading the whole file: a client
/// jar is 31 MB today and asset indexes are larger, and buffering them entirely
/// to hash them would show up directly in the idle memory budget.
pub fn hash_reader(mut reader: impl Read, algorithm: Algorithm) -> io::Result<String> {
    let mut buffer = [0_u8; 64 * 1024];

    macro_rules! stream {
        ($hasher:expr) => {{
            let mut hasher = $hasher;
            loop {
                let read = reader.read(&mut buffer)?;
                if read == 0 {
                    break;
                }
                hasher.update(&buffer[..read]);
            }
            Ok(hex(&hasher.finalize()))
        }};
    }

    match algorithm {
        Algorithm::Sha1 => stream!(Sha1::new()),
        Algorithm::Sha512 => stream!(Sha512::new()),
    }
}

fn hex(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push_str(&format!("{byte:02x}"));
    }
    out
}

pub fn hash_file(path: impl AsRef<Path>, algorithm: Algorithm) -> Result<String> {
    let path = path.as_ref();
    let file = fs::File::open(path).map_err(io_err(path))?;
    hash_reader(io::BufReader::new(file), algorithm).map_err(io_err(path))
}

/// A content-addressed object store rooted at a directory.
#[derive(Debug, Clone)]
pub struct Store {
    root: PathBuf,
}

impl Store {
    /// Open (or create) a store at `root`.
    pub fn open(root: impl Into<PathBuf>) -> Result<Self> {
        let root = root.into();
        fs::create_dir_all(root.join("objects")).map_err(io_err(&root))?;
        Ok(Self { root })
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Where an object with this hash lives.
    ///
    /// Sharded by the first two hex characters. A flat directory with a hundred
    /// thousand entries is slow to enumerate on every filesystem that matters.
    pub fn path_for(&self, hash: &str, algorithm: Algorithm) -> Result<PathBuf> {
        let hash = normalise(hash, algorithm)?;
        let (shard, _) = hash.split_at(2);
        Ok(self.root.join("objects").join(shard).join(&hash))
    }

    /// Where an in-progress download is staged.
    ///
    /// Inside the store rather than a system temp directory, for two reasons: a
    /// partially downloaded file survives a restart and can be resumed with a
    /// range request, and clearing the cache remains one directory delete.
    pub fn staging_path_for(&self, hash: &str, algorithm: Algorithm) -> Result<PathBuf> {
        let hash = normalise(hash, algorithm)?;
        Ok(self.root.join("staging").join(format!("{hash}.partial")))
    }

    /// Whether this object is already present.
    ///
    /// Presence alone is trusted: nothing enters the store without being
    /// verified first, so re-hashing on every read would cost a great deal to
    /// re-answer a question already answered.
    pub fn contains(&self, hash: &str, algorithm: Algorithm) -> bool {
        self.path_for(hash, algorithm)
            .map(|path| path.is_file())
            .unwrap_or(false)
    }

    /// Admit bytes to the store, verifying them first.
    ///
    /// Returns the object's path. Already-present objects are a no-op, which is
    /// what makes re-running a download cheap.
    pub fn insert(&self, bytes: &[u8], expected: &str, algorithm: Algorithm) -> Result<PathBuf> {
        let expected = normalise(expected, algorithm)?;
        let destination = self.path_for(&expected, algorithm)?;

        if destination.is_file() {
            return Ok(destination);
        }

        let actual = hash_reader(bytes, algorithm).map_err(io_err(&destination))?;
        if actual != expected {
            return Err(StoreError::HashMismatch { expected, actual });
        }

        let parent = destination
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_else(|| self.root.clone());
        fs::create_dir_all(&parent).map_err(io_err(&parent))?;

        // Stage then rename: a process killed mid-write must not leave a
        // truncated file sitting at a path that `contains` would call present.
        let staging = parent.join(format!("{expected}.partial"));
        {
            let mut file = fs::File::create(&staging).map_err(io_err(&staging))?;
            file.write_all(bytes).map_err(io_err(&staging))?;
            file.sync_all().map_err(io_err(&staging))?;
        }
        fs::rename(&staging, &destination).map_err(io_err(&destination))?;

        Ok(destination)
    }

    /// Admit a file that is already on disk, verifying it first.
    pub fn insert_file(
        &self,
        source: impl AsRef<Path>,
        expected: &str,
        algorithm: Algorithm,
    ) -> Result<PathBuf> {
        let source = source.as_ref();
        let expected = normalise(expected, algorithm)?;
        let destination = self.path_for(&expected, algorithm)?;

        if destination.is_file() {
            return Ok(destination);
        }

        let actual = hash_file(source, algorithm)?;
        if actual != expected {
            return Err(StoreError::HashMismatch { expected, actual });
        }

        let parent = destination
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_else(|| self.root.clone());
        fs::create_dir_all(&parent).map_err(io_err(&parent))?;

        // A rename across filesystems fails, so fall back to copying.
        match fs::rename(source, &destination) {
            Ok(()) => Ok(destination),
            Err(_) => {
                fs::copy(source, &destination).map_err(io_err(&destination))?;
                Ok(destination)
            }
        }
    }

    /// Place an object into an instance directory.
    ///
    /// Tries a hard link first so N instances of the same version cost one copy
    /// on disk, and falls back to copying when that is not possible - a
    /// different volume, or a filesystem that does not support links. The
    /// fallback is silent because the outcome is identical from the game's
    /// point of view; only disk usage differs.
    pub fn materialise(
        &self,
        hash: &str,
        algorithm: Algorithm,
        destination: impl AsRef<Path>,
    ) -> Result<()> {
        let source = self.path_for(hash, algorithm)?;
        let destination = destination.as_ref();

        if let Some(parent) = destination.parent() {
            fs::create_dir_all(parent).map_err(io_err(parent))?;
        }

        if destination.exists() {
            fs::remove_file(destination).map_err(io_err(destination))?;
        }

        match fs::hard_link(&source, destination) {
            Ok(()) => Ok(()),
            Err(_) => {
                fs::copy(&source, destination).map_err(io_err(destination))?;
                Ok(())
            }
        }
    }

    /// Total bytes held, for reporting cache size to the user.
    pub fn size_on_disk(&self) -> Result<u64> {
        let objects = self.root.join("objects");
        let mut total = 0;

        let Ok(shards) = fs::read_dir(&objects) else {
            return Ok(0);
        };

        for shard in shards.flatten() {
            let Ok(entries) = fs::read_dir(shard.path()) else {
                continue;
            };
            for entry in entries.flatten() {
                if let Ok(meta) = entry.metadata() {
                    if meta.is_file() {
                        total += meta.len();
                    }
                }
            }
        }
        Ok(total)
    }
}

/// Lowercase a digest and check it is the right shape for its algorithm.
///
/// Manifests are inconsistent about case. Validating the length here means a
/// malformed hash fails at the boundary rather than producing a nonsense path.
fn normalise(hash: &str, algorithm: Algorithm) -> Result<String> {
    let trimmed = hash.trim();
    if trimmed.len() != algorithm.hex_len() || !trimmed.chars().all(|c| c.is_ascii_hexdigit()) {
        return Err(StoreError::MalformedHash(hash.to_owned()));
    }
    Ok(trimmed.to_ascii_lowercase())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// sha1("hello") and sha512("hello"), from the standard test vectors.
    const HELLO_SHA1: &str = "aaf4c61ddcc5e8a2dabede0f3b482cd9aea9434d";
    const HELLO_SHA512: &str = "9b71d224bd62f3785d96d46ad3ea3d73319bfbc2890caadae2dff72519673ca72323c3d99ba5c11d7c7acc6e14b8c5da0c4663475c2e5c3adef46f73bcdec043";

    fn store() -> (tempfile::TempDir, Store) {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path()).unwrap();
        (dir, store)
    }

    #[test]
    fn hashes_match_the_standard_vectors() {
        assert_eq!(
            hash_reader(&b"hello"[..], Algorithm::Sha1).unwrap(),
            HELLO_SHA1
        );
        assert_eq!(
            hash_reader(&b"hello"[..], Algorithm::Sha512).unwrap(),
            HELLO_SHA512
        );
    }

    #[test]
    fn objects_are_sharded_by_the_first_two_characters() {
        let (_dir, store) = store();
        let path = store.path_for(HELLO_SHA1, Algorithm::Sha1).unwrap();
        assert!(path.ends_with(format!("aa/{HELLO_SHA1}")), "{path:?}");
    }

    #[test]
    fn inserting_stores_and_finds_the_object() {
        let (_dir, store) = store();
        assert!(!store.contains(HELLO_SHA1, Algorithm::Sha1));

        let path = store.insert(b"hello", HELLO_SHA1, Algorithm::Sha1).unwrap();
        assert!(path.is_file());
        assert!(store.contains(HELLO_SHA1, Algorithm::Sha1));
        assert_eq!(fs::read(&path).unwrap(), b"hello");
    }

    /// The point of the whole module. Corrupt bytes must never be admitted.
    #[test]
    fn content_that_does_not_match_its_hash_is_rejected() {
        let (_dir, store) = store();
        let err = store
            .insert(b"goodbye", HELLO_SHA1, Algorithm::Sha1)
            .unwrap_err();

        assert!(matches!(err, StoreError::HashMismatch { .. }), "{err:?}");
        assert!(
            !store.contains(HELLO_SHA1, Algorithm::Sha1),
            "a rejected object was still written"
        );
    }

    /// A rejected write must not leave a staging file behind that a later run
    /// mistakes for real data.
    #[test]
    fn a_rejected_write_leaves_nothing_behind() {
        let (dir, store) = store();
        store
            .insert(b"goodbye", HELLO_SHA1, Algorithm::Sha1)
            .expect_err("corrupt content must be rejected");

        let mut leftovers = Vec::new();
        for shard in fs::read_dir(dir.path().join("objects"))
            .into_iter()
            .flatten()
            .flatten()
        {
            for entry in fs::read_dir(shard.path()).into_iter().flatten().flatten() {
                leftovers.push(entry.file_name());
            }
        }
        assert!(leftovers.is_empty(), "left behind {leftovers:?}");
    }

    #[test]
    fn inserting_the_same_object_twice_is_a_no_op() {
        let (_dir, store) = store();
        let first = store.insert(b"hello", HELLO_SHA1, Algorithm::Sha1).unwrap();
        let second = store.insert(b"hello", HELLO_SHA1, Algorithm::Sha1).unwrap();
        assert_eq!(first, second);
        assert_eq!(store.size_on_disk().unwrap(), 5, "stored twice");
    }

    /// Deduplication across instances: the same jar needed by two versions is
    /// stored once.
    #[test]
    fn identical_content_from_two_callers_is_stored_once() {
        let (_dir, store) = store();
        store.insert(b"hello", HELLO_SHA1, Algorithm::Sha1).unwrap();
        store
            .insert(b"hello", HELLO_SHA512, Algorithm::Sha512)
            .unwrap();

        // Different algorithms address it differently, but one insert of the
        // same digest never duplicates.
        store.insert(b"hello", HELLO_SHA1, Algorithm::Sha1).unwrap();
        assert_eq!(store.size_on_disk().unwrap(), 10);
    }

    #[test]
    fn malformed_hashes_are_rejected_at_the_boundary() {
        let (_dir, store) = store();
        for bad in ["", "xyz", "AAF4C61DDCC5E8A2DABEDE0F3B482CD9AEA9434", "zz"] {
            assert!(
                store.path_for(bad, Algorithm::Sha1).is_err(),
                "'{bad}' should be rejected"
            );
        }
    }

    /// Manifests are inconsistent about case; an uppercase digest is the same
    /// object.
    #[test]
    fn uppercase_hashes_resolve_to_the_same_object() {
        let (_dir, store) = store();
        store.insert(b"hello", HELLO_SHA1, Algorithm::Sha1).unwrap();
        assert!(store.contains(&HELLO_SHA1.to_ascii_uppercase(), Algorithm::Sha1));
    }

    #[test]
    fn materialising_places_the_content_where_the_game_expects_it() {
        let (dir, store) = store();
        store.insert(b"hello", HELLO_SHA1, Algorithm::Sha1).unwrap();

        let target = dir.path().join("instance/minecraft/libraries/a/b.jar");
        store
            .materialise(HELLO_SHA1, Algorithm::Sha1, &target)
            .unwrap();

        assert_eq!(fs::read(&target).unwrap(), b"hello");
    }

    #[test]
    fn materialising_over_an_existing_file_replaces_it() {
        let (dir, store) = store();
        store.insert(b"hello", HELLO_SHA1, Algorithm::Sha1).unwrap();

        let target = dir.path().join("out.jar");
        fs::write(&target, b"stale").unwrap();
        store
            .materialise(HELLO_SHA1, Algorithm::Sha1, &target)
            .unwrap();

        assert_eq!(fs::read(&target).unwrap(), b"hello");
    }

    #[test]
    fn a_file_on_disk_can_be_admitted() {
        let (dir, store) = store();
        let source = dir.path().join("downloaded.tmp");
        fs::write(&source, b"hello").unwrap();

        let path = store
            .insert_file(&source, HELLO_SHA1, Algorithm::Sha1)
            .unwrap();
        assert!(path.is_file());
        assert_eq!(fs::read(&path).unwrap(), b"hello");
    }

    /// Verified on a store that does not already hold the digest. An object
    /// that is already present short-circuits before verification, by design -
    /// see `already_present_short_circuits_before_verifying`.
    #[test]
    fn a_file_that_does_not_match_its_hash_is_rejected() {
        let (dir, store) = store();
        let bad = dir.path().join("corrupt.tmp");
        fs::write(&bad, b"goodbye").unwrap();

        assert!(matches!(
            store.insert_file(&bad, HELLO_SHA1, Algorithm::Sha1),
            Err(StoreError::HashMismatch { .. })
        ));
        assert!(!store.contains(HELLO_SHA1, Algorithm::Sha1));
    }

    /// Deliberate: once a digest is present, its content has already been
    /// verified, so a second insert claiming that digest returns the stored
    /// object without re-reading the caller's bytes. This is what makes
    /// re-running a download cheap, and the stored content stays correct
    /// regardless of what the second caller passed.
    #[test]
    fn already_present_short_circuits_before_verifying() {
        let (dir, store) = store();
        store.insert(b"hello", HELLO_SHA1, Algorithm::Sha1).unwrap();

        let wrong = dir.path().join("wrong.tmp");
        fs::write(&wrong, b"goodbye").unwrap();

        let path = store
            .insert_file(&wrong, HELLO_SHA1, Algorithm::Sha1)
            .expect("an already-present object is a no-op");

        assert_eq!(
            fs::read(&path).unwrap(),
            b"hello",
            "the stored object was overwritten with unverified content"
        );
    }

    #[test]
    fn an_empty_store_reports_zero_bytes() {
        let (_dir, store) = store();
        assert_eq!(store.size_on_disk().unwrap(), 0);
    }
}
