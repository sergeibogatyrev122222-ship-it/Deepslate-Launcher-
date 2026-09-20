# Progress

Running state of the build. Updated as work lands, so picking this up cold costs nothing.

**Last updated:** 2026-09-20, end of second session.

---

## Where things stand

| Milestone | State |
|---|---|
| M0 — foundation, budgets | **Done.** Window runs, budgets measured and passing |
| M1 — Microsoft sign-in | **Code complete, not verified live.** Blocked on Mojang approval |
| M2 — vanilla launch | **Nearly done.** Files download, Java is detected, instances are isolated, and the exact launch command builds and spawns. Only natives extraction, Java download, and the live launch remain |
| M3 — design system + UI | Not started |
| M4 — mod loaders | Not started |
| M5 — content browser (Modrinth) | Not started |
| M6 — modpacks | Not started |
| M7 — performance & FPS tuning | Not started |
| M8 — customization | Not started |
| M9 — CurseForge, Linux, teardown | Not started |

**214 tests passing, zero clippy warnings, `tsc` clean.**

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

Whole versions download end to end:

| Version | Files | Size | Time | Layout |
|---|---|---|---|---|
| 1.5.2 | 479 | 51.7 MB | 3.5s | `MapToResources`, 749 objects but 468 distinct |
| 1.21.11 | 4667 | 526.3 MB | 39.1s | `Hashed`, 4591 objects, 76 classpath entries |
| 1.5.2 again | 0 | 0 MB | 0.1s | everything already cached |

Java detection on this machine finds all four installed runtimes and picks Adoptium 21.0.11
for Java 21, Mojang's `java-runtime-epsilon` for 25, and correctly refuses to substitute
anything for a Java 8 requirement.

The exact launch command builds correctly for both format generations:

- **1.21.11** — Java 21 selected, 76 classpath entries, working directory inside the
  instance, `--gameDir` isolated and `--assetsDir` shared. The Windows-only
  `-XX:HeapDumpPath` argument appears; the macOS-only `-XstartOnFirstThread` and the
  x86-only `-Xss1M` correctly do not.
- **1.5.2** — the pre-1.13 positional `minecraftArguments` form, with the launcher
  supplying `-cp` and `-Djava.library.path` itself.

The spawn path is exercised in tests by starting a real JVM and reading its piped stderr.

Try it: `ds versions`, `ds resolve 1.21.11`, `ds prepare 1.5.2`, `ds java 21`,
`ds new Test 1.21.11`, `ds dry-run test`.

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

Version catalogue, `inheritsFrom` resolution with cycle detection and a depth cap, asset
indexes across all three historical layouts, whole-version preparation, and Java discovery.

Java selection requires an **exact** major version. Minecraft declares a precise
requirement and substituting a "close enough" runtime turns a clear "needs Java 8" into an
unexplained crash.

### ds-auth — sign-in

The full Microsoft chain, plus account storage with refresh tokens in the OS keychain.

---

## Next actions, in order

1. **Natives extraction** — unpack the classified jars per-instance, honouring
   `extract.exclude`. Per-instance rather than shared because a running JVM holds file
   locks on its native DLLs. Needed for pre-1.19 versions only.
2. **Java download** — fetch a runtime when nothing installed matches, via Mojang's runtime
   manifest with an Adoptium fallback. Detection works; only the download is missing.
   Blocks 1.5.2 (needs Java 8, not installed here); 21 and 25 are already satisfied.
3. **Materialise legacy assets** into the instance for `virtual` and `map_to_resources`
   layouts. The code exists in `assets::materialise`; it is not yet called from prepare.
4. **Wire `ds launch`** — prepare, then build, then spawn, with a real session from
   `ds-auth`.

**M2's completion criterion is blocked.** "Reaches the main menu" needs a real session
token, which needs Mojang approval. Starting the game with a placeholder token is exactly
what a cracked launcher does and is out of scope permanently, so that last step waits.
Everything up to it is verifiable now via `ds dry-run`.

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
- **Java version strings must compare numerically.** Lexicographically `21.0.7` beats
  `21.0.11`, which silently selects an older patch while appearing to work. This is the
  opposite case to Minecraft version ids: Java versions genuinely have numeric components.

---

## Running it

```bash
source ./env.sh
cargo test          # 214 tests
./scripts/check.sh  # fmt, clippy -D warnings, test, tsc
./scripts/build.sh  # release binary + installer

./target/debug/ds versions        # live version list from Mojang
./target/debug/ds resolve 26.2    # resolve a version end to end
./target/debug/ds prepare 1.5.2   # download everything a version needs
./target/debug/ds java 21         # detected runtimes, and which one would be used
./target/debug/ds new Test 1.21.11
./target/debug/ds dry-run test    # the exact command that would launch
```

The toolchain is portable and deliberately not on `PATH`; `env.sh` sets it up.
`TEARDOWN.md` removes it all when the project is finished.
