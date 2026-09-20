//! Resolving `inheritsFrom`.
//!
//! A mod loader profile is not a complete version. Fabric's
//! `fabric-loader-0.18.5-1.21.11` carries its own libraries and main class and
//! nothing else - no asset index, no Java requirement, no client jar. It says
//! `inheritsFrom: "1.21.11"` and expects the launcher to fill the rest in.
//!
//! The merge is not symmetric, and the asymmetry is the whole subtlety:
//!
//! - **Scalars: the child wins.** Its main class is the loader's entry point,
//!   which is the entire reason it exists.
//! - **Libraries: the child goes first.** Combined with first-occurrence-wins
//!   deduplication in `ds_core::classpath`, that is how a loader overrides a
//!   vanilla library with its own version. Reversing this silently loads the
//!   wrong ASM and the game dies somewhere unhelpful.
//! - **Arguments: the parent goes first.** These accumulate rather than
//!   override - the loader is adding flags to vanilla's, not replacing them.

use ds_core::version::{Arguments, VersionManifest};

/// How deep a chain may go before we call it a bug.
///
/// Real chains are two links (loader over vanilla). Three would be unusual.
/// Eight is not a chain, it is a loop someone dressed up.
pub const MAX_DEPTH: usize = 8;

#[derive(Debug, thiserror::Error)]
pub enum InheritError {
    #[error("version '{0}' inherits from itself, directly or through a cycle")]
    Cycle(String),

    #[error("inheritance chain from '{0}' is deeper than {MAX_DEPTH} links")]
    TooDeep(String),
}

/// Merge a child profile onto the parent it inherits from.
///
/// The result carries the parent's `inherits_from`, not the child's: one link
/// has been consumed, and the parent may inherit from something in turn.
pub fn merge(child: VersionManifest, parent: VersionManifest) -> VersionManifest {
    // Child first. `ds_core::classpath` keeps the first occurrence of each
    // group:artifact, so this is what makes a loader's version of a library win.
    let mut libraries = child.libraries;
    libraries.extend(parent.libraries);

    let arguments = match (parent.arguments, child.arguments) {
        (Some(parent_args), Some(child_args)) => Some(Arguments {
            // Parent first: these accumulate. The loader is adding to vanilla's
            // command line, not replacing it.
            game: [parent_args.game, child_args.game].concat(),
            jvm: [parent_args.jvm, child_args.jvm].concat(),
        }),
        (Some(only), None) | (None, Some(only)) => Some(only),
        (None, None) => None,
    };

    VersionManifest {
        // The child's identity is the one the user picked.
        id: child.id,
        kind: if child.kind.is_empty() {
            parent.kind
        } else {
            child.kind
        },
        release_time: if child.release_time.is_empty() {
            parent.release_time
        } else {
            child.release_time
        },
        // The child's link has been consumed, but the PARENT may itself
        // inherit from something. Clearing this outright would stop a chain
        // after one link and, worse, hide a cycle by ending the walk before it
        // could be seen.
        inherits_from: parent.inherits_from,

        main_class: child.main_class.or(parent.main_class),
        assets: child.assets.or(parent.assets),
        asset_index: child.asset_index.or(parent.asset_index),
        java_version: child.java_version.or(parent.java_version),
        minecraft_arguments: child.minecraft_arguments.or(parent.minecraft_arguments),

        // The client jar comes from the parent unless the child overrides it,
        // which only Forge-style installers do.
        downloads: if child.downloads.is_empty() {
            parent.downloads
        } else {
            child.downloads
        },

        libraries,
        arguments,
    }
}

/// Follow a chain of manifests to a complete version.
///
/// `fetch` is supplied by the caller so this stays testable and so mod loader
/// profiles - which live on disk or come from a loader's own API, not Mojang's
/// index - can be resolved through the same path.
pub async fn resolve<F, E, Fut>(
    start: VersionManifest,
    mut fetch: F,
) -> Result<VersionManifest, ResolveError<E>>
where
    F: FnMut(String) -> Fut,
    Fut: std::future::Future<Output = Result<VersionManifest, E>>,
{
    let origin = start.id.clone();
    let mut seen = vec![start.id.clone()];
    let mut current = start;

    while let Some(parent_id) = current.inherits_from.clone() {
        if seen.contains(&parent_id) {
            return Err(ResolveError::Inherit(InheritError::Cycle(parent_id)));
        }
        if seen.len() >= MAX_DEPTH {
            return Err(ResolveError::Inherit(InheritError::TooDeep(origin)));
        }
        seen.push(parent_id.clone());

        let parent = fetch(parent_id).await.map_err(ResolveError::Fetch)?;
        current = merge(current, parent);
    }

    Ok(current)
}

#[derive(Debug, thiserror::Error)]
pub enum ResolveError<E> {
    #[error(transparent)]
    Inherit(#[from] InheritError),

    #[error("could not fetch a parent version")]
    Fetch(#[source] E),
}

#[cfg(test)]
mod tests {
    use super::*;
    use ds_core::platform::{Arch, Os, Platform};
    use ds_core::rules::Features;

    fn parse(json: &str) -> VersionManifest {
        VersionManifest::parse(json).expect("test manifest should parse")
    }

    fn vanilla() -> VersionManifest {
        parse(
            r#"{
                "id":"1.21.11","type":"release","releaseTime":"2026-01-01T00:00:00+00:00",
                "mainClass":"net.minecraft.client.main.Main",
                "assets":"29",
                "assetIndex":{"id":"29","sha1":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa","size":1,"totalSize":2,"url":"http://x/29.json"},
                "javaVersion":{"component":"java-runtime-delta","majorVersion":21},
                "downloads":{"client":{"sha1":"bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb","size":1,"url":"http://x/client.jar"}},
                "libraries":[
                    {"name":"org.ow2.asm:asm:9.6"},
                    {"name":"com.mojang:logging:1.0.0"}
                ],
                "arguments":{"game":["--username","${auth_player_name}"],"jvm":["-cp","${classpath}"]}
            }"#,
        )
    }

    fn fabric() -> VersionManifest {
        parse(
            r#"{
                "id":"fabric-loader-0.18.5-1.21.11",
                "inheritsFrom":"1.21.11",
                "type":"release",
                "mainClass":"net.fabricmc.loader.impl.launch.knot.KnotClient",
                "libraries":[
                    {"name":"org.ow2.asm:asm:9.8"},
                    {"name":"net.fabricmc:fabric-loader:0.18.5"}
                ],
                "arguments":{"game":[],"jvm":["-DFabricMcEmu=net.minecraft.client.main.Main"]}
            }"#,
        )
    }

    #[test]
    fn the_child_main_class_wins() {
        let merged = merge(fabric(), vanilla());
        assert_eq!(
            merged.main_class.as_deref(),
            Some("net.fabricmc.loader.impl.launch.knot.KnotClient"),
            "the loader's entry point is the reason it exists"
        );
    }

    #[test]
    fn everything_the_child_omits_comes_from_the_parent() {
        let merged = merge(fabric(), vanilla());
        assert_eq!(merged.assets.as_deref(), Some("29"));
        assert_eq!(
            merged.java_version.as_ref().map(|j| j.major_version),
            Some(21),
            "the loader profile has no javaVersion and must inherit it"
        );
        assert!(merged.asset_index.is_some());
        assert!(merged.client_download().is_some());
    }

    #[test]
    fn the_merged_version_keeps_the_childs_id_and_is_fully_resolved() {
        let merged = merge(fabric(), vanilla());
        assert_eq!(merged.id, "fabric-loader-0.18.5-1.21.11");
        assert!(merged.inherits_from.is_none());
    }

    /// The asymmetry that matters. Reversing it silently loads the wrong ASM.
    #[test]
    fn child_libraries_come_first_so_the_loader_overrides() {
        let merged = merge(fabric(), vanilla());
        let names: Vec<&str> = merged.libraries.iter().map(|l| l.name.as_str()).collect();

        let asm_9_8 = names.iter().position(|n| *n == "org.ow2.asm:asm:9.8");
        let asm_9_6 = names.iter().position(|n| *n == "org.ow2.asm:asm:9.6");
        assert!(
            asm_9_8 < asm_9_6,
            "the loader's asm must precede vanilla's: {names:?}"
        );
    }

    /// End to end with the deduplication that gives the ordering its meaning.
    #[test]
    fn only_the_loaders_version_of_a_shared_library_reaches_the_classpath() {
        let merged = merge(fabric(), vanilla());
        let platform = Platform::new(Os::Windows, Arch::X86_64, "10.0");

        let entries =
            ds_core::classpath::entries(&merged, &platform, &Features::new(), None).unwrap();

        let asm: Vec<&String> = entries.iter().filter(|e| e.contains("/asm/")).collect();
        assert_eq!(asm.len(), 1, "both asm versions on the classpath: {asm:?}");
        assert!(asm[0].contains("9.8"), "the wrong asm won: {asm:?}");
    }

    /// Arguments accumulate rather than override - the loader is adding to
    /// vanilla's command line, not replacing it.
    #[test]
    fn arguments_are_concatenated_with_the_parents_first() {
        let merged = merge(fabric(), vanilla());
        let arguments = merged.arguments.expect("merged arguments");

        assert_eq!(
            arguments.jvm.len(),
            3,
            "expected vanilla's two plus fabric's one"
        );
        assert_eq!(
            arguments.game.len(),
            2,
            "vanilla's game arguments must survive"
        );
    }

    #[test]
    fn a_parent_with_arguments_and_a_child_without_keeps_the_parents() {
        let child = parse(r#"{"id":"c","inheritsFrom":"p","type":"release"}"#);
        let merged = merge(child, vanilla());
        assert!(merged.arguments.is_some());
    }

    #[tokio::test]
    async fn resolve_walks_a_two_link_chain() {
        let merged = resolve(fabric(), |id| async move {
            assert_eq!(id, "1.21.11");
            Ok::<_, String>(vanilla())
        })
        .await
        .expect("resolution should succeed");

        assert!(merged.inherits_from.is_none());
        assert_eq!(merged.id, "fabric-loader-0.18.5-1.21.11");
        assert_eq!(merged.java_version.map(|j| j.major_version), Some(21));
    }

    /// Regression: merge() used to clear inherits_from outright, so a chain
    /// stopped dead after one link and a cycle was never seen because the walk
    /// ended before it could repeat an id.
    #[tokio::test]
    async fn a_three_link_chain_is_followed_to_the_end() {
        let top =
            parse(r#"{"id":"top","inheritsFrom":"middle","type":"release","mainClass":"Top"}"#);

        let merged = resolve(top, |id| async move {
            Ok::<_, String>(match id.as_str() {
                "middle" => parse(
                    r#"{"id":"middle","inheritsFrom":"1.21.11","type":"release",
                        "libraries":[{"name":"mid.dle:lib:1"}]}"#,
                ),
                "1.21.11" => vanilla(),
                other => panic!("unexpected fetch for {other}"),
            })
        })
        .await
        .expect("a three link chain should resolve");

        assert!(merged.inherits_from.is_none(), "chain not fully resolved");
        assert_eq!(merged.id, "top");
        assert_eq!(merged.main_class.as_deref(), Some("Top"));
        assert_eq!(
            merged.java_version.as_ref().map(|j| j.major_version),
            Some(21),
            "the grandparent's Java requirement did not reach the top"
        );

        let names: Vec<&str> = merged.libraries.iter().map(|l| l.name.as_str()).collect();
        assert!(names.contains(&"mid.dle:lib:1"), "{names:?}");
        assert!(names.contains(&"org.ow2.asm:asm:9.6"), "{names:?}");
    }

    #[tokio::test]
    async fn a_version_that_inherits_from_itself_is_rejected() {
        let looping = parse(r#"{"id":"a","inheritsFrom":"a","type":"release"}"#);
        let err = resolve(looping, |_| async { Ok::<_, String>(vanilla()) })
            .await
            .expect_err("a self-reference must be caught");

        assert!(
            matches!(err, ResolveError::Inherit(InheritError::Cycle(_))),
            "{err:?}"
        );
    }

    /// Two manifests pointing at each other must terminate, not spin.
    #[tokio::test]
    async fn a_two_step_cycle_terminates() {
        let a = parse(r#"{"id":"a","inheritsFrom":"b","type":"release"}"#);
        let err = resolve(a, |id| async move {
            Ok::<_, String>(match id.as_str() {
                "b" => parse(r#"{"id":"b","inheritsFrom":"a","type":"release"}"#),
                _ => parse(r#"{"id":"a","inheritsFrom":"b","type":"release"}"#),
            })
        })
        .await
        .expect_err("a cycle must be caught");

        assert!(
            matches!(err, ResolveError::Inherit(InheritError::Cycle(_))),
            "{err:?}"
        );
    }

    /// A chain that never repeats an id but never ends either.
    #[tokio::test]
    async fn an_unbounded_chain_is_cut_off() {
        let start = parse(r#"{"id":"link-0","inheritsFrom":"link-1","type":"release"}"#);
        let mut next = 2;

        let err = resolve(start, |_| {
            let id = next;
            next += 1;
            async move {
                Ok::<_, String>(parse(&format!(
                    r#"{{"id":"link-{}","inheritsFrom":"link-{id}","type":"release"}}"#,
                    id - 1
                )))
            }
        })
        .await
        .expect_err("an endless chain must be cut off");

        assert!(
            matches!(err, ResolveError::Inherit(InheritError::TooDeep(_))),
            "{err:?}"
        );
    }

    #[tokio::test]
    async fn a_fetch_failure_is_reported_not_swallowed() {
        let err = resolve(fabric(), |_| async {
            Err::<VersionManifest, _>("parent is offline".to_owned())
        })
        .await
        .expect_err("a failed fetch must fail resolution");

        assert!(matches!(err, ResolveError::Fetch(_)), "{err:?}");
    }

    #[tokio::test]
    async fn a_complete_version_resolves_to_itself_without_fetching() {
        let merged = resolve(vanilla(), |_| async {
            Err::<VersionManifest, _>("fetch must not be called".to_owned())
        })
        .await
        .expect("a version with no inheritsFrom must not fetch anything");

        assert_eq!(merged.id, "1.21.11");
    }
}
