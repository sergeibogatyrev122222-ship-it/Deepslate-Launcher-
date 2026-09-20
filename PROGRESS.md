# Progress

Running state of the build. Updated as work lands, so picking this up cold costs nothing.

**Last updated:** 2026-09-20.

---

## Where things stand

| Milestone | State |
|---|---|
| M0 — foundation, budgets | **Done.** Window runs, budgets measured and passing |
| M1 — Microsoft sign-in | **Code complete, not verified live.** Blocked on Mojang approval |
| M2 — vanilla launch | **In progress.** Resolution and downloading work against live Mojang servers; assets, Java and spawn remain |
| M3 — design system + UI | Not started |
| M4 — mod loaders | Not started |
| M5 — content browser (Modrinth) | Not started |
| M6 — modpacks | Not started |
| M7 — performance & FPS tuning | Not started |
| M8 — customization | Not started |
| M9 — CurseForge, Linux, teardown | Not started |

**161 tests passing, zero clippy warnings, `tsc` clean.**

### Measured budgets (M0 release build)

| Budget | Target | Actual |
|---|---|---|
| Installer | < 30 MB | **1.38 MB** |
| Cold start | < 2000 ms | **517 ms** |
| Idle RAM (private working set) | < 150 MB | **82.1 MB** |

Raw samples in `measurements/`. Re-measure with `scripts/measure-rss.ps1`.

---

## Blocked on someone else

**Mojang app approval.** Sign-in cannot be tested against a real account until the Azure
app registration is approved for the Minecraft API. Requested via
<https://aka.ms/mce-reviewappid>; they review weekly.

Until then `login_with_xbox` returns HTTP 403 and the launcher reports
`AuthError::AppNotApproved`, which is correct behaviour, not a bug. There are reports
through late 2026 of this approval being slow or refused, so it may not arrive at all —
see `docs/microsoft-login-setup.md`.

**Azure side is done:** app `046bf82e-b3bf-4a42-866d-4c3a772de68a`, personal Microsoft
accounts, public client, loopback redirect URIs registered.

**Nothing else is blocked.** Everything below was built and verified without it.

---

## Verified against live Mojang servers

Not fixtures — the real version list, 915 versions, fetched and resolved:

| Version | Java | Libraries to classpath | Notes |
|---|---|---|---|
| 26.2 | `java-runtime-epsilon` (25) | 88 to 88 | |
| 1.21.11 | `java-runtime-delta` (21) | 75 to 75 | |
| 1.12.2 | `jre-legacy` (8) | 34 to 32 | |
| 1.5.2 | `jre-legacy` (8) | 10 to 7 | `pre-1.6` assets, `launchwrapper` main class |
| b1.7.3 | `jre-legacy` (8) | 10 to 7 | `pre-1.6` assets |

Gaps on legacy versions are correct: natives-only libraries are excluded because they are
extracted rather than loaded, and 1.8.9 genuinely ships LWJGL twice at different versions.

Try it: `./target/debug/ds versions` and `./target/debug/ds resolve 1.21.11`.

---

## What is built

### ds-core — pure, zero I/O

- **`rules`** — library and argument rule evaluation. Semantics stated exactly in the
  module docs, because getting them wrong yields a missing native library rather than an
  error.
- **`args`** — argument templating for both format generations, with an unfilled
  `${placeholder}` treated as an error rather than passed to the JVM.
- **`version`** — the manifest model, checked against a real trimmed 1.21.11 fixture.
- **`classpath`** — Maven coordinates and classpath assembly.
- **`platform`** — OS/arch as an injectable value, so rules can be evaluated for every
  platform from one test run.

### ds-store — the content-addressed store

`objects/<aa>/<hash>`; the filesystem is the index, so there is no database. Verifies
before admitting, stages writes through a rename, hard links into instances with a copy
fallback.

### ds-net — downloading

One pooled client, concurrency bounded by a semaphore, bodies streamed to disk, resume via
range requests, retry narrowed to 5xx/408/429.

### ds-mc — the Minecraft side

Version catalogue and `inheritsFrom` resolution with cycle detection and a depth cap.

### ds-auth — sign-in

The full Microsoft chain, plus account storage with refresh tokens in the OS keychain.

---

## Next actions, in order

1. **Assets** — fetch and parse the asset index, then the objects. Includes the `legacy`
   and `pre-1.6` layouts, which need materialising into a real directory tree because
   those versions read assets by path rather than by hash.
2. **Java** — discovery including Mojang's own runtime directories
   (`%LOCALAPPDATA%/Packages/Microsoft.4297127D64EC6_*/LocalCache/Local/runtime` already
   holds `java-runtime-delta` and `java-runtime-epsilon` on this machine), then download
   when nothing satisfies the version's requirement.
3. **Natives** — extract per-instance, honouring `extract.exclude`.
4. **Launch** — build the argument vector and spawn. M2 is done when 1.21.11, 26.2 and one
   `pre-1.6`-era version all reach the main menu.

---

## Decisions already made, do not re-litigate

Full reasoning in `ARCHITECTURE.md`.

- **Six crates, not twelve.** `ds-mc` is a deliberate lump, expected to split as M4/M5 land.
- **Version IDs are opaque strings.** Ordering comes from `releaseTime` and `type`.
- **Java requirements are data**, read from the resolved version JSON after `inheritsFrom`.
- **Ownership is decided by `/minecraft/profile`**, not `/entitlements/mcstore`.
- **No device-code auth flow.** Cut as speculative scope.
- **Idle RAM is private working set**, never summed working set.
- **No open-source licence file**, deliberately — the project may be sold later.
- **Library override contract:** first occurrence of an identity wins, so whoever merges an
  inheritance chain puts the overriding manifest's libraries first. `ds-mc::inherit` does.

## Facts checked against real manifests, not assumed

- Modern versions carry **no** `natives`/`classifiers`/`extract` keys. Native libraries are
  ordinary entries gated by an OS rule; the old mechanism is pre-1.19 only.
- An argument's `value` is **sometimes a string and sometimes an array**.
- `${arch}` in a natives classifier expands to the **bit width**: `natives-windows-64`.
- `os.version` is a **regex**, not a literal prefix.

## Traps already hit, do not repeat

- **A classifier is part of an artifact's identity, not an attribute of it.** Deduplicating
  on `group:artifact` alone silently dropped 25 of 1.21.11's 75 Windows libraries — every
  native jar. Only visible because resolution ran against real data.
- **`merge` must carry the *parent's* `inherits_from`.** Clearing it outright stops a chain
  after one link and hides cycles, because the walk ends before it can revisit an id.
- `winget` reports exit 0 while the installer it ran failed (watch for installer exit
  **1602**, a dismissed UAC prompt). Same class: piping `cargo` through `grep` returns
  grep's exit code.
- **w64devkit cannot build this.** No `libgcc_eh.a`. See `docs/toolchain-setup.md`.
- **Calling a Solid resource accessor after it errors re-throws**, freezing the UI on its
  last render. `ARCHITECTURE.md` §11.1.
- A leaked app instance makes the next memory measurement silently wrong.
  `scripts/measure-rss.ps1` refuses to run when a stale instance exists.
- A `serde` struct missing `rename_all = "camelCase"` fails silently by yielding a default.
  This bit `AssetIndexRef.totalSize`.
- **Bash eats backticks inside `node -e '...'`.** Patch scripts with backticks in them go
  to a file via a quoted heredoc first.

---

## Running it

```bash
source ./env.sh
cargo test          # 161 tests
./scripts/check.sh  # fmt, clippy -D warnings, test, tsc
./scripts/build.sh  # release binary + installer

./target/debug/ds versions        # live version list from Mojang
./target/debug/ds resolve 26.2    # resolve a version end to end
```

The toolchain is portable and deliberately not on `PATH`; `env.sh` sets it up.
`TEARDOWN.md` removes it all when the project is finished.
