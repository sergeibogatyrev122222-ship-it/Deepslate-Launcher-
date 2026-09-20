#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

//! Deepslate application shell.
//!
//! This crate owns the Tauri window, the IPC surface and app state. Domain logic
//! lives in the `ds-*` crates below it; nothing here should grow business rules.

use std::time::Instant;

use serde::Serialize;
use specta::Type;
use tauri::{Manager, State};
use tauri_specta::{collect_commands, Builder};

/// Captured as the first statement of `main`, before any initialisation, so the
/// cold-start figure includes everything the user waits through.
struct Startup(Instant);

/// Milliseconds as a `u32`, saturating.
///
/// Specta refuses to export `u64` to TypeScript because JS numbers lose
/// precision above 2^53, and it is right to - these values cross into
/// JavaScript, where every number is an f64. `u32` covers 49.7 days of
/// milliseconds, and saturating means a pathologically long session reports a
/// clamped figure rather than silently wrapping to a small one.
fn millis_u32(duration: std::time::Duration) -> u32 {
    u32::try_from(duration.as_millis()).unwrap_or(u32::MAX)
}

/// Static facts about the running app.
///
/// Deliberately the first thing the UI asks for: it is the end-to-end proof that
/// the generated-type IPC boundary actually works, not just that it compiles.
#[derive(Debug, Clone, Serialize, Type)]
#[serde(rename_all = "camelCase")]
struct AppInfo {
    name: String,
    version: String,
    target: String,
    uptime_ms: u32,
}

#[tauri::command]
#[specta::specta]
fn app_info(startup: State<'_, Startup>) -> AppInfo {
    AppInfo {
        name: env!("CARGO_PKG_NAME").to_owned(),
        version: env!("CARGO_PKG_VERSION").to_owned(),
        target: std::env::consts::OS.to_owned(),
        uptime_ms: millis_u32(startup.0.elapsed()),
    }
}

/// Called by the frontend on first paint.
///
/// Does two things: reveals the window (it is created hidden so the user never
/// sees an unstyled white flash), and records cold-start-to-interactive. When
/// `DEEPSLATE_STARTUP_LOG` names a file the figure is written there, which is how
/// the budget harness reads it from a windowed build that has no stdout.
#[tauri::command]
#[specta::specta]
fn mark_ready(app: tauri::AppHandle, startup: State<'_, Startup>) -> u32 {
    let elapsed = millis_u32(startup.0.elapsed());

    if let Some(window) = app.get_webview_window("main") {
        if let Err(error) = window.show() {
            eprintln!("deepslate: failed to show main window: {error}");
        }
    }

    if let Ok(path) = std::env::var("DEEPSLATE_STARTUP_LOG") {
        if let Err(error) = std::fs::write(&path, elapsed.to_string()) {
            eprintln!("deepslate: failed to write startup log to {path}: {error}");
        }
    }

    elapsed
}

/// The whole IPC surface, declared once.
///
/// `main` and the binding-export test both go through here, so the generated
/// types cannot drift from the commands actually registered.
fn ipc_builder() -> Builder<tauri::Wry> {
    Builder::<tauri::Wry>::new().commands(collect_commands![app_info, mark_ready])
}

fn main() {
    let startup = Startup(Instant::now());

    let builder = ipc_builder();

    let result = tauri::Builder::default()
        .invoke_handler(builder.invoke_handler())
        .setup(move |app| {
            builder.mount_events(app);
            app.manage(startup);
            Ok(())
        })
        .run(tauri::generate_context!());

    if let Err(error) = result {
        eprintln!("deepslate: fatal error while running application: {error}");
        std::process::exit(1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Where the generated TypeScript bindings land, relative to this crate.
    /// Scoped to the test module: it is only used here, and at crate level it
    /// would be dead code in every non-test build.
    const BINDINGS_PATH: &str = "../../ui/src/ipc/bindings.ts";

    /// Regenerates `ui/src/ipc/bindings.ts`.
    ///
    /// Deliberately a test rather than a step inside `main`: `cargo test` is then
    /// enough to refresh the bindings, with no window and no display server, so
    /// CI and `scripts/check.sh` can run `tsc` against types that are guaranteed
    /// current. A command added without the frontend adapting becomes a `tsc`
    /// failure instead of a runtime surprise.
    #[test]
    #[allow(clippy::expect_used, reason = "a failure here should fail the test")]
    fn exports_typescript_bindings() {
        ipc_builder()
            .export(specta_typescript::Typescript::default(), BINDINGS_PATH)
            .expect("failed to export TypeScript bindings");
    }
}
