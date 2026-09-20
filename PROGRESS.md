# Progress

Running state of the build. Updated as work lands, so picking this up cold costs nothing.

**Last updated:** 2026-09-20, end of the first session.

---

## Where things stand

| Milestone | State |
|---|---|
| M0 — foundation, budgets | **Done.** Window runs, budgets measured and passing |
| M1 — Microsoft sign-in | **Code complete, not verified live.** 58 tests, all HTTP mocked |
| M2 — vanilla launch | Not started. **Next.** |
| M3 — design system + UI | Not started |
| M4 — mod loaders | Not started |
| M5 — content browser (Modrinth) | Not started |
| M6 — modpacks | Not started |
| M7 — performance & FPS tuning | Not started |
| M8 — customization | Not started |
| M9 — CurseForge, Linux, teardown | Not started |

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
app registration is approved for the Minecraft API. Submitted via
<https://aka.ms/mce-reviewappid>; they review weekly.

Until then `login_with_xbox` returns HTTP 403 and the launcher reports
`AuthError::AppNotApproved`, which is correct behaviour, not a bug. There are reports
through late 2026 of this approval being slow or refused, so it may not arrive at all —
see `docs/microsoft-login-setup.md`.

**Nothing else depends on it.** M2 onwards can be built and tested in full; only the final
"press play with a real account" step needs approval.

---

## Next actions, in order

1. **`ds` dev CLI** — a second `[[bin]]` on `ds-app` exposing `ds login`, `ds accounts`,
   `ds logout`, so M1 is exercisable without any UI. This is the last piece of M1.
2. **M2: version resolution** — `ds-core` (pure) for library rule evaluation, argument
   templating and classpath assembly; `ds-mc` for manifest fetch, `inheritsFrom`
   resolution, asset indexes and natives.
3. **M2: downloads** — `ds-net` (pooling, retry, resumable range requests) and `ds-store`
   (the content-addressed store).
4. **M2: Java** — discovery including Mojang's own runtime directories, then download when
   nothing on the machine satisfies the version's requirement.
5. **M2: launch** — build the argument vector and spawn. Done when 1.21.11, 26.2 and one
   `pre-1.6`-era version all reach the main menu.

---

## Decisions already made, do not re-litigate

Full reasoning in `ARCHITECTURE.md`. The short list:

- **Six crates, not twelve.** An earlier draft split by concern; that was planning
  structure rather than discovering it. `ds-mc` is a deliberate lump and is expected to
  split as M2/M4/M5 land.
- **Version IDs are opaque strings.** `1.21.11` and `26.2` do not compare. Ordering comes
  from `releaseTime` and `type`.
- **Java requirements are data**, read from the resolved version JSON after `inheritsFrom`.
  Never a hardcoded table — `java-runtime-epsilon` (25) did not exist a year ago.
- **Ownership is decided by `/minecraft/profile`**, not `/entitlements/mcstore`. The
  entitlements endpoint returns empty for some Game Pass subscribers who genuinely own the
  game.
- **No device-code auth flow.** Cut as speculative scope; this is a GUI app and a browser
  is always present.
- **Idle RAM is private working set**, never summed working set. WebView2's six processes
  share one runtime image, so working set double-counts about 4.3x.

## Traps already hit, do not repeat

- `winget` reports exit 0 while the installer it ran failed (watch for installer exit
  **1602**, meaning a UAC prompt was dismissed). Same class of bug: piping `cargo` through
  `grep` returns grep's exit code, not cargo's.
- **w64devkit cannot build this.** Its GCC ships no `libgcc_eh.a`, which Rust's
  `windows-gnu` target hardcodes. Full detail in `docs/toolchain-setup.md`.
- **Calling a Solid resource accessor after it errors re-throws**, freezing the UI on its
  last render. Match `resource.error` first. See `ARCHITECTURE.md` §11.1.
- A leaked app instance makes the next memory measurement silently wrong, because WebView2
  shares one browser process per user-data folder. `scripts/measure-rss.ps1` refuses to run
  when a stale instance exists.

---

## Running it

```bash
source ./env.sh
cargo test          # 58 tests
./scripts/check.sh  # fmt, clippy -D warnings, test, tsc
./scripts/build.sh  # release binary + installer
```

The toolchain is portable and deliberately not on `PATH`; `env.sh` sets it up.
`TEARDOWN.md` removes it all when the project is finished.
