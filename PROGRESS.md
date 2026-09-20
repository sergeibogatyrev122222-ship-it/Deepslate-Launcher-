# Progress

Running state of the build. Updated as work lands, so picking this up cold costs nothing.

**Last updated:** 2026-09-20, overnight session.

---

## Where things stand

| Milestone | State |
|---|---|
| M0 — foundation, budgets | **Done.** Window runs, budgets measured and passing |
| M1 — Microsoft sign-in | **Code complete, not verified live.** Blocked on Mojang approval |
| M2 — vanilla launch | **In progress.** Pure half (`ds-core`) done; I/O half not started |
| M3 — design system + UI | Not started |
| M4 — mod loaders | Not started |
| M5 — content browser (Modrinth) | Not started |
| M6 — modpacks | Not started |
| M7 — performance & FPS tuning | Not started |
| M8 — customization | Not started |
| M9 — CurseForge, Linux, teardown | Not started |

**129 tests passing, zero clippy warnings, `tsc` clean.**

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

**Nothing else depends on it.** M2 onwards can be built and tested in full; only the final
"press play with a real account" step needs approval.

---

## What `ds-core` covers (M2, pure half)

Zero I/O, so all of it is tested directly with no mock server.

- **`rules`** — library and argument rule evaluation. Semantics stated exactly in the
  module docs, because getting them wrong yields a missing native library rather than an
  error.
- **`args`** — argument templating for both format generations, with an unfilled
  `${placeholder}` treated as an error rather than passed through to the JVM.
- **`version`** — the manifest model, checked against a real trimmed 1.21.11 fixture in
  `crates/ds-core/tests/fixtures/`.
- **`classpath`** — Maven coordinate parsing and classpath assembly, including the
  coordinate-to-path derivation that mod loader libraries depend on.
- **`platform`** — OS/arch as an injectable value, so rules can be evaluated for every
  platform from one test run.

---

## Next actions, in order

1. **`ds-net`** — HTTP client with pooling, retry/backoff and resumable range requests.
   The only remaining piece before real files can be fetched.
3. **`ds-mc`** — manifest fetch and cache, `inheritsFrom` resolution (with cycle detection),
   asset index handling including the `legacy` and `pre-1.6` layouts.
   **Note the contract in `classpath::entries`:** whoever merges an inheritance chain must
   place the overriding manifest's libraries first, because first occurrence of a
   `group:artifact` wins.
4. **Java** — discovery including Mojang's own runtime directories
   (`%LOCALAPPDATA%/Packages/Microsoft.4297127D64EC6_*/LocalCache/Local/runtime` already
   holds `java-runtime-delta` and `java-runtime-epsilon` on this machine), then download
   when nothing satisfies the version's requirement.
5. **Launch** — build the argument vector and spawn. Done when 1.21.11, 26.2 and one
   `pre-1.6`-era version all reach the main menu.

---

## Decisions already made, do not re-litigate

Full reasoning in `ARCHITECTURE.md`. The short list:

- **Six crates, not twelve.** `ds-mc` is a deliberate lump, expected to split as M2/M4/M5
  land.
- **Version IDs are opaque strings.** `1.21.11` and `26.2` do not compare. Ordering comes
  from `releaseTime` and `type`.
- **Java requirements are data**, read from the resolved version JSON after `inheritsFrom`.
  Never a hardcoded table — `java-runtime-epsilon` (25) did not exist a year ago.
- **Ownership is decided by `/minecraft/profile`**, not `/entitlements/mcstore`, which
  returns empty for some Game Pass subscribers who genuinely own the game.
- **No device-code auth flow.** Cut as speculative scope.
- **Idle RAM is private working set**, never summed working set.
- **No open-source licence file**, deliberately — the project may be sold later, and
  adding one is a one-way door.

## Facts checked against real manifests, not assumed

- Modern versions carry **no** `natives`/`classifiers`/`extract` keys. Native libraries are
  ordinary entries gated by an OS rule; the old mechanism is pre-1.19 only.
- An argument's `value` is **sometimes a string and sometimes an array** — the macOS JVM
  entry in 1.21.11 uses an array while the Windows entry beside it uses a string.
- `${arch}` in a natives classifier expands to the **bit width**, not the architecture
  name: `natives-windows-${arch}` is `natives-windows-64`.
- `os.version` is a **regex**, not a literal prefix.

## Traps already hit, do not repeat

- `winget` reports exit 0 while the installer it ran failed (watch for installer exit
  **1602**, meaning a UAC prompt was dismissed). Same class of bug: piping `cargo` through
  `grep` returns grep's exit code, not cargo's.
- **w64devkit cannot build this.** Its GCC ships no `libgcc_eh.a`, which Rust's
  `windows-gnu` target hardcodes. Detail in `docs/toolchain-setup.md`.
- **Calling a Solid resource accessor after it errors re-throws**, freezing the UI on its
  last render. Match `resource.error` first. `ARCHITECTURE.md` §11.1.
- A leaked app instance makes the next memory measurement silently wrong, because WebView2
  shares one browser process per user-data folder. `scripts/measure-rss.ps1` refuses to run
  when a stale instance exists.
- A `serde` struct missing `rename_all = "camelCase"` fails silently, not loudly — it just
  yields a default value. This already bit `AssetIndexRef.totalSize`, caught only because
  a test asserted against real data.

---

## Running it

```bash
source ./env.sh
cargo test          # 129 tests
./scripts/check.sh  # fmt, clippy -D warnings, test, tsc
./scripts/build.sh  # release binary + installer
./target/debug/ds   # the dev CLI
```

The toolchain is portable and deliberately not on `PATH`; `env.sh` sets it up.
`TEARDOWN.md` removes it all when the project is finished.
