# Deepslate

A native Minecraft launcher: fast, deeply customizable, performance-focused.

Original work. Official Microsoft accounts only — nothing here bypasses Mojang
authentication or supports cracked accounts.

## Status

Milestone 0 — foundation. The window opens, the typed IPC boundary works end to end, and
the performance budgets are measured rather than assumed. No launching yet.

| Budget | Target | Measured (M0 release build) | |
|---|---|---|---|
| Idle RAM (private working set) | < 150 MB | **82.1 MB** | 67.9 MB headroom |
| Installer size | < 30 MB | **1.38 MB** | |
| Cold start → interactive | < 2000 ms | **517 ms** | |
| Click → JVM start (warm) | < 1000 ms | — | not applicable until M2 |

Raw samples in [`measurements/`](measurements). Idle RAM is private working set summed
across the process tree, **not** working set — WebView2's 6 processes share one runtime
image, and summing working set double-counts it about 4.5x (356.9 MB vs 82.1 MB).

Of the 82.1 MB, WebView2 is 76.8 MB and our Rust process is 5.3 MB. The fixed overhead is
not reducible, so the whole application has to fit in the remaining ~68 MB — which is why
every long list is virtualized.

## Layout

```
crates/
  ds-app/     Tauri shell: window, IPC commands, app state
ui/           SolidJS + TypeScript frontend
  src/design/ tokens, then primitives, then screens - in that order
  src/ipc/    bindings.ts, generated from Rust (gitignored, never hand-edited)
scripts/      check.sh (all gates), measure-rss.ps1 (memory budget)
docs/         toolchain-setup.md
```

Read [ARCHITECTURE.md](ARCHITECTURE.md) first — it covers module boundaries, the
content-addressed store, instance isolation, the auth failure taxonomy and the error
strategy.

## Building

The toolchain is deliberately portable and is not on the system `PATH`.

```bash
source ./env.sh
cargo build
```

Full setup, including the MSVC prerequisite, is in
[docs/toolchain-setup.md](docs/toolchain-setup.md). Removing it afterwards is
[TEARDOWN.md](TEARDOWN.md).

## Verifying

```bash
./scripts/check.sh
```

Runs `cargo fmt --check`, `cargo clippy -D warnings`, `cargo test`, and `tsc --noEmit`.

Memory budget, against a release build:

```powershell
.\scripts\measure-rss.ps1 -ExePath .\target\release\deepslate.exe
```

## Conventions

- `unwrap`, `expect`, `panic`, `todo` and `unimplemented` are **denied** by workspace lints
  outside tests. This is enforced by clippy, not by review.
- Version IDs are opaque strings. `1.21.11` and `26.2` do not compare — ordering comes from
  the manifest's `releaseTime` and `type`.
- Java requirements are read from the resolved version JSON, never from a hardcoded table.
- Every visual value resolves to a token in `ui/src/design/tokens.css`.
