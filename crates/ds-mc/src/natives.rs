//! Unpacking native libraries.
//!
//! Pre-1.19 versions ship platform-specific code in classified jars that have
//! to be unpacked before the JVM can load them. Modern versions do not: their
//! natives are ordinary libraries gated by an OS rule, so this whole module is
//! a no-op for anything recent.
//!
//! Extraction is **per instance**, not shared. A running JVM holds open handles
//! on its native DLLs, so two instances running at once would collide over one
//! shared directory - on Windows the second would fail to start and the error
//! would point nowhere useful.
//!
//! Every entry path in a zip is untrusted input. See [`is_safe_entry`].

use std::io::Read as _;
use std::path::{Component, Path, PathBuf};

#[derive(Debug, thiserror::Error)]
pub enum NativesError {
    #[error("could not open the native library archive {path}")]
    Open {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("{path} is not a readable jar")]
    Archive {
        path: PathBuf,
        #[source]
        source: zip::result::ZipError,
    },

    #[error("could not write {path}")]
    Write {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
}

type Result<T> = std::result::Result<T, NativesError>;

/// Whether a zip entry may be written under a target directory.
///
/// Zip entries carry their own path strings, and an archive can name
/// `../../evil`. Extracting that writes outside the directory the caller chose -
/// the "zip slip" bug, which has shipped in a great many extraction routines.
///
/// Rejected rather than sanitised. A path that has been "made safe" is still a
/// path someone deliberately crafted, and quietly rewriting it hides the fact
/// that an archive tried.
pub fn is_safe_entry(name: &str) -> bool {
    if name.is_empty() {
        return false;
    }

    let path = Path::new(name);
    path.components()
        .all(|component| matches!(component, Component::Normal(_)))
}

/// Whether an entry is excluded by the manifest's `extract.exclude` list.
///
/// Mojang lists directory prefixes such as `META-INF/`. Matching is a plain
/// prefix test, which is what the format means.
fn is_excluded(name: &str, exclude: &[String]) -> bool {
    exclude
        .iter()
        .any(|prefix| name.starts_with(prefix.as_str()))
}

/// Unpack one natives jar into a directory.
///
/// Returns how many files were written. Existing files are replaced, so
/// re-extracting after a corrupted run is safe and needs no cleanup first.
pub fn extract(jar: &Path, target: &Path, exclude: &[String]) -> Result<usize> {
    let file = std::fs::File::open(jar).map_err(|source| NativesError::Open {
        path: jar.to_path_buf(),
        source,
    })?;

    let mut archive = zip::ZipArchive::new(std::io::BufReader::new(file)).map_err(|source| {
        NativesError::Archive {
            path: jar.to_path_buf(),
            source,
        }
    })?;

    std::fs::create_dir_all(target).map_err(|source| NativesError::Write {
        path: target.to_path_buf(),
        source,
    })?;

    let mut written = 0;

    for index in 0..archive.len() {
        let mut entry = archive
            .by_index(index)
            .map_err(|source| NativesError::Archive {
                path: jar.to_path_buf(),
                source,
            })?;

        if entry.is_dir() {
            continue;
        }

        let name = entry.name().to_owned();
        if !is_safe_entry(&name) || is_excluded(&name, exclude) {
            continue;
        }

        // Natives are loaded by file name from one flat directory; the JVM does
        // not walk subdirectories of java.library.path. Some jars nest them
        // anyway, so flatten to the file name.
        let Some(file_name) = Path::new(&name).file_name() else {
            continue;
        };
        let destination = target.join(file_name);

        let mut bytes = Vec::with_capacity(entry.size() as usize);
        entry
            .read_to_end(&mut bytes)
            .map_err(|source| NativesError::Write {
                path: destination.clone(),
                source,
            })?;

        std::fs::write(&destination, &bytes).map_err(|source| NativesError::Write {
            path: destination.clone(),
            source,
        })?;
        written += 1;
    }

    Ok(written)
}

/// Unpack every natives jar a version needs.
pub fn extract_all(jars: &[PathBuf], target: &Path, exclude: &[String]) -> Result<usize> {
    let mut total = 0;
    for jar in jars {
        total += extract(jar, target, exclude)?;
    }
    Ok(total)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write as _;
    use zip::write::SimpleFileOptions;

    /// Build a jar in memory with the given entries.
    fn make_jar(path: &Path, entries: &[(&str, &[u8])]) {
        let file = std::fs::File::create(path).unwrap();
        let mut zip = zip::ZipWriter::new(file);
        let options = SimpleFileOptions::default();

        for (name, contents) in entries {
            zip.start_file(*name, options).unwrap();
            zip.write_all(contents).unwrap();
        }
        zip.finish().unwrap();
    }

    #[test]
    fn native_libraries_are_written_out() {
        let dir = tempfile::tempdir().unwrap();
        let jar = dir.path().join("natives.jar");
        make_jar(
            &jar,
            &[("lwjgl64.dll", b"MZ-fake"), ("OpenAL64.dll", b"MZ-also")],
        );

        let target = dir.path().join("natives");
        assert_eq!(extract(&jar, &target, &[]).unwrap(), 2);

        assert_eq!(
            std::fs::read(target.join("lwjgl64.dll")).unwrap(),
            b"MZ-fake"
        );
        assert!(target.join("OpenAL64.dll").is_file());
    }

    /// Mojang lists directory prefixes such as `META-INF/`.
    #[test]
    fn excluded_prefixes_are_skipped() {
        let dir = tempfile::tempdir().unwrap();
        let jar = dir.path().join("natives.jar");
        make_jar(
            &jar,
            &[
                ("lwjgl64.dll", b"real"),
                ("META-INF/MANIFEST.MF", b"junk"),
                ("META-INF/maven/pom.xml", b"junk"),
            ],
        );

        let target = dir.path().join("natives");
        let written = extract(&jar, &target, &["META-INF/".to_owned()]).unwrap();

        assert_eq!(written, 1, "an excluded entry was written");
        assert!(target.join("lwjgl64.dll").is_file());
        assert!(!target.join("MANIFEST.MF").exists());
        assert!(!target.join("pom.xml").exists());
    }

    /// Zip slip. An archive naming `../../evil` must not write outside the
    /// target directory.
    #[test]
    fn entries_that_escape_the_target_are_refused() {
        for bad in [
            "../escaped.dll",
            "a/../../escaped.dll",
            "/absolute.dll",
            "..\\windows\\escaped.dll",
            "",
        ] {
            assert!(!is_safe_entry(bad), "'{bad}' should be refused");
        }

        for good in ["lwjgl64.dll", "natives/lwjgl64.dll", "a/b/c.so"] {
            assert!(is_safe_entry(good), "'{good}' should be allowed");
        }
    }

    #[test]
    fn a_traversing_archive_writes_nothing_outside_the_target() {
        let dir = tempfile::tempdir().unwrap();
        let jar = dir.path().join("evil.jar");
        make_jar(&jar, &[("../escaped.dll", b"pwned"), ("fine.dll", b"ok")]);

        let target = dir.path().join("deep/natives");
        let written = extract(&jar, &target, &[]).unwrap();

        assert_eq!(written, 1, "the traversing entry was extracted");
        assert!(target.join("fine.dll").is_file());
        assert!(
            !dir.path().join("deep/escaped.dll").exists()
                && !dir.path().join("escaped.dll").exists(),
            "an entry escaped the target directory"
        );
    }

    /// The JVM does not walk subdirectories of java.library.path, so nested
    /// entries are flattened to their file name.
    #[test]
    fn nested_entries_are_flattened_to_one_directory() {
        let dir = tempfile::tempdir().unwrap();
        let jar = dir.path().join("nested.jar");
        make_jar(&jar, &[("windows/x64/lwjgl.dll", b"nested")]);

        let target = dir.path().join("natives");
        assert_eq!(extract(&jar, &target, &[]).unwrap(), 1);

        assert!(
            target.join("lwjgl.dll").is_file(),
            "a nested native was not flattened"
        );
    }

    /// Re-extracting after an interrupted run must not need a cleanup first.
    #[test]
    fn extracting_twice_replaces_rather_than_failing() {
        let dir = tempfile::tempdir().unwrap();
        let jar = dir.path().join("natives.jar");
        make_jar(&jar, &[("lwjgl64.dll", b"version-one")]);

        let target = dir.path().join("natives");
        extract(&jar, &target, &[]).unwrap();

        make_jar(&jar, &[("lwjgl64.dll", b"version-two")]);
        extract(&jar, &target, &[]).unwrap();

        assert_eq!(
            std::fs::read(target.join("lwjgl64.dll")).unwrap(),
            b"version-two"
        );
    }

    #[test]
    fn several_jars_extract_into_one_directory() {
        let dir = tempfile::tempdir().unwrap();
        let a = dir.path().join("a.jar");
        let b = dir.path().join("b.jar");
        make_jar(&a, &[("lwjgl64.dll", b"a")]);
        make_jar(&b, &[("jinput64.dll", b"b")]);

        let target = dir.path().join("natives");
        let written = extract_all(&[a, b], &target, &[]).unwrap();

        assert_eq!(written, 2);
        assert!(target.join("lwjgl64.dll").is_file());
        assert!(target.join("jinput64.dll").is_file());
    }

    #[test]
    fn a_missing_jar_is_reported_not_ignored() {
        let dir = tempfile::tempdir().unwrap();
        let result = extract(&dir.path().join("nope.jar"), &dir.path().join("out"), &[]);
        assert!(matches!(result, Err(NativesError::Open { .. })));
    }

    #[test]
    fn a_file_that_is_not_a_jar_is_reported() {
        let dir = tempfile::tempdir().unwrap();
        let fake = dir.path().join("not-a-jar.jar");
        std::fs::write(&fake, b"this is not a zip archive at all").unwrap();

        let result = extract(&fake, &dir.path().join("out"), &[]);
        assert!(matches!(result, Err(NativesError::Archive { .. })));
    }

    #[test]
    fn extracting_nothing_is_not_an_error() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(extract_all(&[], &dir.path().join("out"), &[]).unwrap(), 0);
    }
}
