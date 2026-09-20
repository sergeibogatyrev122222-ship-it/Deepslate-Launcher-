# Deepslate — Architecture

> Status: **draft for review**. Nothing in M1+ gets implemented until this is signed off.

## 1. Purpose

A native desktop Minecraft launcher: fast, deeply customizable, performance-focused.
Original work only — no Lunar Client branding, assets, or code.

**Non-goals, permanently:** anything that bypasses Mojang authentication, supports cracked
accounts, or circumvents entitlement checks. Official Microsoft accounts only.

**Non-goals, for now:** a plugin/extension API, an account-less "offline mode", server
hosting, a mod *development* workflow. None of these are scaffolded ahead of need.

## 2. Stack

| Layer | Choice | Version |
|---|---|---|
| Shell | Tauri | 2.11.x (pinned; 3.0 is alpha) |
| Backend | Rust | 1.98.1 stable, `x86_64-pc-windows-msvc` |
| Frontend | SolidJS + TypeScript + Vite | Solid 1.x |
| IPC types | `tauri-specta` | generated, never hand-written |

**Why a webview at all.** The requirement for user-supplied CSS / theme files is what
decides this. `egui` and `Slint` have no CSS; Avalonia has XAML styling, not CSS; Electron
misses every budget. Tauri is the only option where "drop in a theme file" is a one-line
feature rather than a plugin system.

**Why not `x86_64-pc-windows-gnu`.** Evaluated and rejected during M0. w64devkit's GCC
merges exception handling into `libgcc` and ships no `libgcc_eh.a`, which Rust's gnu target
hardcodes; every link fails. A different MinGW distribution could be patched in, but gnu is
not a target Tauri supports or tests, and shipping a daily-driver app on an untested
toolchain makes every future WebView2 bug ambiguous. MSVC uninstalls cleanly (see
`TEARDOWN.md`), so the only cost is transient disk.

## 3. Performance budgets

Hard budgets. Measured in CI, asserted, and failing the build on regression — never assumed.

| Budget | Target | How measured |
|---|---|---|
| Cold start → interactive | < 2000 ms | Frontend emits `app:ready` on first paint; stamped against process start time captured in `main()` before any init |
| Installer size | < 30 MB | Bundle artifact byte size |
| Idle RAM | < 150 MB | **Private working set** summed across the process tree, sampled at t+60s, window open and idle |
| Click → JVM start (warm) | < 1000 ms | Timestamp delta from IPC command receipt to child spawn returning |

**On the RAM metric.** Do not sum `WorkingSetSize` across the tree. WebView2 runs 6
processes sharing one large runtime image, and working set counts shared pages in every
process that maps them — measured on the M0 release build, that inflates the figure ~4.3x
(356.9 MB working set vs 82.1 MB private). Private working set is what Task Manager's
Memory column reports and what a user means by "this app uses X MB". It slightly
understates true cost by excluding shared runtime pages, but those are genuinely shared
with any other WebView2 app on the machine, so charging them wholly to us would overstate
it. Private commit is tracked alongside as a leak signal.

**Measured M0 baseline** (`measurements/m0-release.json`), release build, empty window:

| Component | Private WS |
|---|---|
| `msedgewebview2.exe` x6 | 76.8 MB |
| `deepslate.exe` | 5.3 MB |
| **total** | **82.1 MB** — 67.9 MB headroom |

So ~77 MB is fixed WebView2 overhead we cannot reduce, and the entire application — DOM, JS
heap, Rust-side state and caches — has to fit in the remaining ~68 MB. That is workable but
not generous, and it is why virtualizing every long list (section 11) is a hard requirement
rather than an optimization. The honesty policy stands: a miss gets reported with numbers,
not quietly absorbed.

Measured alongside it: cold start **517 ms** / 2000 ms, installer **1.38 MB** / 30 MB.

## 4. Crate topology

Dependency direction is strictly downward. Nothing below depends on anything above it.
**Crates are created when a milestone needs them, never ahead of need.**

```
                    ds-app  (Tauri binary + `ds` dev CLI as a second [[bin]])
                       |
        +--------------+--------------+
     ds-auth                       ds-mc
        |                             |
        +------+---------------+------+
               |               |
            ds-net         ds-store
               |               |
               +-------+-------+
                       |
                    ds-core     (pure, zero I/O)
```

| Crate | Owns | Explicitly does NOT |
|---|---|---|
| `ds-core` | Domain types, library rule evaluation, argument templating, classpath assembly. **Zero I/O.** | Touch the network or filesystem |
| `ds-net` | HTTP client, connection pool, retry/backoff, resumable range requests, progress reporting | Know what it is downloading |
| `ds-store` | Content-addressed store: hashing, dedupe, linking into instances, GC | Know about Minecraft |
| `ds-auth` | MSA to XBL to XSTS to MC chain, keychain, account store | Persist tokens outside the keychain |
| `ds-mc` | Version resolution, Java discovery, loaders, instances, launch, content providers | Contain pure rule logic (that is `ds-core`) |
| `ds-app` | Tauri commands, events, app state, config, and the `ds` dev CLI | Contain domain logic |

**Why six and not twelve.** An earlier draft split this eleven ways by concern. That was
planning a structure rather than discovering one. Rust's module system already gives
encapsulation inside a crate; crate boundaries additionally buy enforced acyclicity and
parallel compilation, which are real but not worth guessing eleven seams before the code
exists. Splitting a crate that is already written is a mechanical refactor — guessing the
seam wrong first and living with it is not.

Two splits earn their place on day one:

- **`ds-core`** is the load-bearing one. Everything worth testing heavily — rule
  evaluation, classpath ordering, argument substitution — is pure, so it needs no HTTP
  mocking at all.
- **`ds-auth`** is genuinely independent of everything else and carries a large test
  surface of its own.

`ds-mc` is a deliberate lump and is expected to split two or three ways as M2, M4 and M5
land. That is planned, not a compromise.

## 5. State model

**Rust owns all persistent state. The frontend holds view state only** — scroll position,
which panel is open, in-flight form values. It never becomes a second source of truth.

There is **no database**. A content-addressed store keyed by hash needs no index because the
path *is* the key, and instance config is plain files so it stays hand-editable and
exportable.

```
%APPDATA%/Deepslate/
  config.toml                     app settings, theme, layout
  accounts.json                   uuid, username, avatar URL, keychain ref - NEVER tokens
  instances/
    <slug>/
      instance.toml               version, loader, JVM args, memory, window, java override
      minecraft/                  THE isolated game dir
        saves/ config/ mods/ resourcepacks/ shaderpacks/ options.txt logs/
      natives/                    per-instance; the JVM locks these DLLs so they cannot be shared

%LOCALAPPDATA%/Deepslate/cache/
  objects/<aa>/<full-sha1-or-sha512>    the CAS - one copy of every jar, ever
  meta/versions/<id>.json               cached manifests, with ETag + Last-Modified
  meta/providers/<provider>/...         cached search/index responses
  java/<component>/                     managed runtimes (e.g. java-runtime-delta)
  assets/
    indexes/<id>.json
    objects/<aa>/<sha1>                 shared across all instances by design
```

### 5.1 The content-addressed store

Every downloaded artifact lands at `objects/<first-2-hex>/<full-hash>`, verified against the
hash the manifest declared *before* being moved into place (download to a temp file, verify,
then atomic rename). A hash that is already present is never re-downloaded. This is the
mechanism behind "the same jar is never downloaded or stored twice across instances."

Instances reference CAS objects by **hardlink** on the same volume, **reflink** where the
filesystem supports it, and **copy** as a last resort. Classpath entries point at CAS paths
directly — libraries are never copied into an instance at all.

Two deliberate exceptions:

- **Natives** are extracted per-instance. A running JVM holds an open handle on its native
  DLLs; sharing them across concurrently-running instances causes file-locking failures.
- **Legacy assets** (`assets: "legacy"` and `"pre-1.6"`) must be materialized into a real
  directory tree with human-readable names, because those game versions read them by path
  rather than by hash. Modern versions take `--assetsDir` pointing at the shared store and
  need no materialization whatsoever.

GC is reference-counted by scanning instance manifests, never by mtime heuristics.

### 5.2 Instance isolation

Isolation is enforced by construction, not convention: the game's working directory is
`instances/<slug>/minecraft/`, and `-Duser.home` plus the game's own `--gameDir` are set so
nothing resolves outside it. Two instances sharing a version share *CAS objects* — which are
immutable and hash-verified — and share nothing mutable. A mod writing to its config
directory in instance A is physically incapable of reaching instance B.

## 6. Data flow

```
 UI (Solid)                      ds-app                         domain crates
 ---------                       ------                         -------------
 invoke(cmd, typed args) ----->  #[tauri::command] async  ---->  pure/IO work
                                       |                              |
                                       |  spawns on tokio             |
 listen("download:progress") <--  coalesced @30Hz  <--------------  progress channel
 listen("launch:log")        <--  line-buffered    <--------------  stdout/stderr pipe
                                       |
 typed Result<T, AppError>   <---------+
```

Rules:

- Commands are `async` and **never block**. Anything long-running spawns on tokio and
  returns a handle immediately.
- Progress events are **coalesced to ~30 Hz in Rust**. Emitting one event per completed
  chunk across hundreds of concurrent downloads is the easiest way to lose 60fps, and it is
  cheapest to prevent at the source.
- Every event carries a monotonic sequence number so the UI can detect drops and reorder.
- `tauri-specta` generates `ui/src/ipc/bindings.ts` from the command signatures at build
  time. It is gitignored and never hand-edited — a Rust signature change that the frontend
  has not adapted to becomes a `tsc` failure, not a runtime surprise.

## 7. Error strategy

- Every crate defines its own `thiserror` enum. **`anyhow` appears only in tests and
  `ds-cli`**, never in a library.
- Errors cross the IPC boundary as:

```rust
pub struct AppError {
    pub code: ErrorCode,        // stable enum - the UI branches on this, never on strings
    pub message: String,        // human-readable, already actionable
    pub detail: Option<String>, // technical context for the log view
    pub retryable: bool,        // drives whether the UI offers a retry affordance
}
```

- Enforced by the toolchain rather than by discipline, in `[workspace.lints.clippy]`:
  `unwrap_used`, `expect_used`, `panic`, `todo`, `unimplemented`, `let_underscore_must_use`,
  `dbg_macro` — all `deny`, relaxed only under `#[cfg(test)]`.
- No error is ever logged and discarded. It either propagates or is converted into a typed
  outcome the caller handles.

## 8. Two rules derived from real manifest data

**Version IDs are opaque strings.** `1.21.11` and `26.2` are not comparable under any
version-parsing scheme. All ordering and filtering comes from the manifest's `releaseTime`
and `type` fields. No crate may parse a version ID into numeric components.

**Java requirements are data, never a table.** The required major version is read from
`javaVersion.majorVersion` in the *resolved* version JSON — after `inheritsFrom` is applied,
because loader profiles omit the field and must inherit it. When absent entirely (pre-2021
manifests), the default is Java 8. Observed components: `jre-legacy` (8), `alpha` (16),
`beta`/`gamma` (17), `delta` (21), `epsilon` (25). `epsilon` did not exist a year ago — a
hardcoded table rots silently on every Mojang bump.

## 9. Authentication

Authorization Code + PKCE in the system browser, redirecting to an ephemeral loopback
listener on `127.0.0.1`. Public client, no secret shipped.

**No device-code flow.** An earlier draft carried it as a fallback. It is a second complete
flow to build, test and maintain, and it exists for headless environments — this is a GUI
game launcher, where a browser is always present. Cut as speculative scope; add it if a real
need appears.

```
MSA token --> XBL user.authenticate --> XSTS xsts.authorize --> MC login_with_xbox
                                        (rp://api.minecraftservices.com/)
                                                  |
                                  +---------------+---------------+
                          /minecraft/profile            /entitlements/mcstore
                          AUTHORITATIVE for ownership   diagnostic detail only
```

**Ownership is decided by `/minecraft/profile`, not by entitlements.** This is the
correction that matters most in this section. `/entitlements/mcstore` can return empty for
a Game Pass subscriber who genuinely owns and can play the game; gating on it produces
"the launcher says I don't own Minecraft" for a class of legitimate users. A 200 from the
profile endpoint is the trustworthy signal. Entitlements are still fetched, but only to
enrich the error message when the profile call fails.

Failures are typed variants with actionable messages:

| Condition | Variant |
|---|---|
| XSTS `XErr 2148916233` | `NoXboxAccount` — account exists but has no Xbox profile |
| XSTS `XErr 2148916238` | `ChildAccount` — needs to be added to a Family by an adult |
| XSTS `XErr 2148916235` | `RegionUnavailable` |
| **any other `XErr`** | **`XstsUnknown { xerr: u64 }` — raw code surfaced verbatim** |
| Profile 404 | `NoProfile` — authenticated, but no Minecraft profile on this account |
| Profile 401/403 | `NotEntitled` — enriched with the entitlements response when available |
| Refresh rejected | `RefreshExpired` — silent retry, then interactive re-auth |
| Network failure mid-chain | `Transport` — each step idempotent; no half-written account |

The `XstsUnknown` catch-all is deliberate. The mapped codes come from documentation rather
than from responses observed in the wild, so a closed enum would silently swallow anything
Microsoft returns that is not on the list. Surfacing the raw `XErr` keeps an unmapped
failure actionable — the user can search the number, and we learn which variant to add.

**Refresh is proactive, not reactive.** Tokens are renewed on a timer ahead of expiry rather
than on a 401, so a user never eats a failed launch because a token lapsed moments earlier.
A reactive refresh still exists as the backstop for a token invalidated server-side.

**Token storage.** Refresh tokens go to the OS keychain via `keyring` (Windows Credential
Manager / libsecret / macOS Keychain) and nowhere else. Access tokens live in memory for
their lifetime and are never written to disk. `accounts.json` holds only non-secret
metadata plus the keychain reference. Tokens are never logged, never included in error
`detail`, and are redacted in any diagnostic bundle.

## 10. Concurrency

Single multi-threaded tokio runtime owned by `ds-app`. Download parallelism is bounded by a
`Semaphore` (default = `min(cpus * 2, 16)`) rather than unbounded spawning, so a 3000-file
asset index does not open 3000 sockets. `ds-net` holds one `reqwest::Client` process-wide for
connection reuse and HTTP/2 multiplexing.

Launching uses `tokio::process::Command` directly — **argument vector, never a command
string, never a shell**. On Windows, `CREATE_NO_WINDOW` prevents a console flash.

**Launch pre-warm:** selecting an instance in the UI begins resolving its version JSON and
verifying CAS presence in the background, so the click-to-spawn path is only the spawn. This
is how the sub-second warm launch budget is met.

## 11. Frontend

Design system first, screens second. No component library, no CSS framework.

```
ui/src/
  design/     tokens.css (type scale, spacing, color, motion, elevation)
              primitives/ (Button, Field, Dialog, Menu, Tabs, VirtualList...)
  features/   instances/ accounts/ content/ settings/ logs/
  ipc/        bindings.ts (generated) + thin typed wrappers
  routes/
```

- **Theming is CSS custom properties, top to bottom.** Dark/light/custom accent are token
  overrides, which makes a user-supplied theme file the *same mechanism* the built-in themes
  use rather than a bolted-on extra.
- **Motion lives in tokens**, not scattered through components — durations and easings are
  named CSS variables, with a FLIP helper for list transitions. `prefers-reduced-motion` is
  honored globally.
- **Every long list is virtualized** (`@tanstack/solid-virtual`). The version list alone is
  ~900 entries with snapshots and legacy included; the content browser is unbounded. This is
  the primary lever on the RAM budget.
- **Keyboard navigation is a requirement, not a pass.** Every interactive element is
  reachable, focus is visibly tracked, dialogs trap and restore focus, and shortcuts are
  remappable from M8.

### 11.1 Two async conventions, both learned the hard way in M0

**Never call a Solid resource accessor on an error path.** Calling `resource()` after the
resource has errored *re-throws*, which breaks the reactive subtree and freezes the UI on
its last render — the observed symptom was a row stuck on "calling…" forever with the real
error only visible in the console. Match `resource.error` first, and never call the
accessor in that branch:

```tsx
<Switch fallback={<Pending />}>
  <Match when={info.error}>{/* error branch - does NOT call info() */}</Match>
  <Match when={info()}>{(loaded) => <Ready data={loaded()} />}</Match>
</Switch>
```

**Never `void` a promise.** An IPC call without a rejection handler is a swallowed error by
another name: the UI sits on its pending state indefinitely and nothing reaches the log.
Every `invoke` gets both arms, or an `ErrorBoundary` above it.

Both rules exist because the failure mode is identical and silent — a screen that looks
like it is still loading when it has actually already failed.

## 12. Testing

Tests where they earn their place, per the brief:

| Area | Approach |
|---|---|
| Library rule evaluation, arg templating, classpath order | Pure unit tests + `insta` snapshots — no mocking needed by design |
| Auth chain | `wiremock`, one test per failure variant in section 9 |
| Version/manifest parsing | Real manifests checked in as trimmed fixtures, **including ones harvested from a live `.minecraft`** so we test formats that actually shipped |
| Dependency resolution | `wiremock` + synthetic graphs (diamond deps, cycles, version conflicts) |
| CAS | Property-style tests for dedupe, corruption detection, interrupted-write recovery |

Gates on every commit: `cargo fmt --check`, `cargo clippy -- -D warnings`, `cargo test`,
`tsc --noEmit`, plus the budget assertions from section 3.

## 13. Security posture

- No secret ships in the binary. The Azure client ID is public by design (PKCE, no secret).
  The CurseForge key comes from env at build time / config at runtime and is gitignored.
- Tauri CSP is restrictive; the webview has no arbitrary filesystem or shell access. Every
  privileged operation is an explicit, typed command with a narrow signature.
- User-supplied themes are **CSS only** and are injected as stylesheet text, never as HTML
  or script. A theme cannot execute code.
- Downloads are verified against manifest-declared hashes before use. A hash mismatch is a
  hard failure, never a warning.

## 14. Cross-platform

Windows and Linux are first-class and both get built and verified. macOS is kept
*code-clean* — correct path handling, native classifiers, keychain backend — but is **not
claimed as supported**, because it cannot be built or signed from this machine and a platform
that has not been run is not a platform that works.

Linux's known weak spot is WebKitGTK: slower than WebView2 and subject to NVIDIA dmabuf
rendering bugs that need a documented environment workaround.

## 15. Open risks

1. **Idle RAM headroom is ~68 MB, and it is finite.** The budget passes today at 82.1 MB,
   but ~77 MB of that is fixed WebView2 cost we cannot reduce, so every future screen spends
   from a small remainder. The content browser and the log view are the two features most
   able to blow it. This is now a *watch* item rather than a *risk* — but it is the one
   budget where a careless feature can regress us in a single commit, which is why it is
   asserted in CI.
2. **Forge installer processors** are the hardest technical item in the project — executing
   the installer's own transformation steps offline, for both the 1.13+ processor model and
   the legacy 1.7–1.12 `install_profile` model.
3. **Mojang gates the Minecraft Services API** for third-party launchers. Approval may be
   slow or refused. Mocked tests keep development unblocked; live verification does not.
4. **Tauri 3.0 is in alpha.** We pin 2.11.x and keep the IPC surface thin and centralized so
   a future migration stays contained to `ds-app`.
