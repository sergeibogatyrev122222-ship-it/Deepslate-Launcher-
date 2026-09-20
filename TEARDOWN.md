# Teardown — removing the build toolchain

You asked for the build toolchain to be fully removed once the launcher is built. This file
is the record of everything installed for that purpose, and how to undo it.

**Read this first:** after teardown you can still *run* the built launcher, but you cannot
*rebuild* it without reinstalling the toolchain. Ship artifacts are produced and copied
somewhere safe **before** running any of this.

## What was installed, and where

| # | Item | Location | Approx. size | Elevation needed to remove |
|---|---|---|---|---|
| 1 | Rust toolchain (portable) | `C:\Users\serge\tools\rust\` | 2.0 GB | No |
| 2 | VS 2022 Build Tools + Windows 11 SDK | `C:\Program Files (x86)\Microsoft Visual Studio\2022\BuildTools\` and `C:\Program Files (x86)\Windows Kits\10\` | ~3 GB | **Yes** |
| 3 | VS installer package cache | `C:\ProgramData\Microsoft\VisualStudio\Packages\` | ~1–2 GB | **Yes** |
| 4 | Node modules | `deepslate\ui\node_modules\` | 94 MB | No |
| 5 | Rust build artifacts | `deepslate\target\` | 4.6 GB and growing | No |
| 6 | Downloaded game files | `%LOCALAPPDATA%\Deepslate\` | grows per version prepared (1.21.11 alone is ~530 MB) | No |

w64devkit (the failed MinGW spike, 708 MB) was installed during M0 and **already removed** —
it is listed here only so the record is complete.

Nothing was added to the system `PATH`. Rust lives entirely under `tools\rust` via
`RUSTUP_HOME` / `CARGO_HOME` (see `env.sh`), matching the portable Maven/Gradle layout
already in `C:\Users\serge\tools`. That is why items 1, 4, 5 and 6 are folder deletes.

**Item 6 is a cache and nothing in it is unique** — every file is re-downloadable from
Mojang and verified by hash. Your saves, configs and mods are not in there; those live in
instance directories under `%APPDATA%\Deepslate\instances\`, which this teardown does
**not** touch.

## Before you tear down

```powershell
# Confirm the shipped artifacts exist and are somewhere outside the repo
Get-ChildItem "C:\Claude plugins\deepslate\target\release\bundle" -Recurse -File |
  Select-Object FullName, @{n='MB';e={[math]::Round($_.Length/1MB,2)}}
```

Copy the installer and the portable executable somewhere permanent. Once item 6 is deleted
they are gone.

## Teardown

### Step 1 — build artifacts and node modules (no elevation, biggest win)

```powershell
Remove-Item -Recurse -Force "C:\Claude plugins\deepslate\target"
Remove-Item -Recurse -Force "C:\Claude plugins\deepslate\ui\node_modules"
Remove-Item -Recurse -Force "$env:LOCALAPPDATA\Deepslate"
```

That last line is the downloaded game cache. Safe to delete at any time, not only at
teardown — it costs a re-download and nothing else.

### Step 2 — Rust (no elevation)

```powershell
Remove-Item -Recurse -Force "C:\Users\serge\tools\rust"
```

There is no `rustup self uninstall` step and no registry state to clean, because the install
was portable and used `--no-modify-path`. Verify nothing leaked into the default locations:

```powershell
Test-Path "$env:USERPROFILE\.cargo"; Test-Path "$env:USERPROFILE\.rustup"
```

Both should print `False`.

### Step 3 — Visual Studio Build Tools (needs elevation)

```powershell
winget uninstall --id Microsoft.VisualStudio.2022.BuildTools -e
```

If that fails, use the VS Installer UI directly:

```powershell
& "${env:ProgramFiles(x86)}\Microsoft Visual Studio\Installer\setup.exe" uninstall --productId Microsoft.VisualStudio.Product.BuildTools --channelId VisualStudio.17.Release --quiet
```

### Step 4 — VS installer package cache (needs elevation)

The uninstall above leaves a multi-gigabyte download cache behind. Remove it:

```powershell
Remove-Item -Recurse -Force "C:\ProgramData\Microsoft\VisualStudio\Packages"
```

### Step 5 — Windows SDK, only if you want it gone

The Windows 11 SDK is a separate product and other tooling may depend on it. Leave it
unless you are sure:

```powershell
winget uninstall --id Microsoft.WindowsSDK.10.0.26100 -e
```

## Verify

```powershell
foreach ($p in @(
  "C:\Users\serge\tools\rust",
  "C:\Claude plugins\deepslate\target",
  "C:\Program Files (x86)\Microsoft Visual Studio\2022\BuildTools",
  "C:\ProgramData\Microsoft\VisualStudio\Packages"
)) { "{0,-70} {1}" -f $p, (Test-Path $p) }
```

All should report `False`.

## Rebuilding later

If you ever want to build again, the toolchain is reproducible from scratch:

1. `winget install Microsoft.VisualStudio.2022.BuildTools` with
   `--add Microsoft.VisualStudio.Component.VC.Tools.x86.x64`
   `--add Microsoft.VisualStudio.Component.Windows11SDK.26100`
2. Portable rustup, per the commands recorded in `docs/toolchain-setup.md`.
3. `source ./env.sh && cargo build --release`
