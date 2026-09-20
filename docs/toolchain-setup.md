# Toolchain setup

Exact record of what the build environment needs, so it can be rebuilt from scratch after
`TEARDOWN.md` removes it.

Nothing here touches the system `PATH`. Rust lives entirely under
`C:\Users\serge\tools\rust`, matching the portable Maven/Gradle layout already in that
folder, which is why removing it is a folder delete rather than an uninstaller.

## 1. Rust (portable)

```powershell
$env:RUSTUP_HOME = 'C:\Users\serge\tools\rust\rustup'
$env:CARGO_HOME  = 'C:\Users\serge\tools\rust\cargo'
Invoke-WebRequest https://static.rust-lang.org/rustup/dist/x86_64-pc-windows-msvc/rustup-init.exe -OutFile rustup-init.exe
.\rustup-init.exe -y --no-modify-path --profile minimal -c clippy -c rustfmt
```

`--no-modify-path` is what keeps the install self-contained. Every shell that builds this
project sources `env.sh` (or sets the two variables) instead.

Installed: Rust 1.98.1 stable, host `x86_64-pc-windows-msvc`.

## 2. MSVC build tools

Rust's `windows-msvc` target needs the MSVC linker and the Windows SDK. **This step needs
elevation** - it is the only part of the toolchain that does.

```powershell
winget install --id Microsoft.VisualStudio.2022.BuildTools -e `
  --override "--quiet --wait --norestart `
    --add Microsoft.VisualStudio.Component.VC.Tools.x86.x64 `
    --add Microsoft.VisualStudio.Component.Windows11SDK.26100"
```

The explicit component list matters: the `VCTools` workload with `--includeRecommended`
pulls roughly 5 GB of extras that Rust never uses. The two components above are ~3 GB and
are sufficient.

Installed: MSVC 14.44.35207, Windows SDK 10.0.26100.

### If the UAC prompt does not appear

`winget` run detached cannot surface an elevation prompt; it fails with installer exit code
**1602** (`ERROR_INSTALL_USEREXIT`) while `winget` itself still reports success. Run it from
an interactive shell, or launch the bootstrapper directly:

```powershell
Invoke-WebRequest https://aka.ms/vs/17/release/vs_BuildTools.exe -OutFile vs_BuildTools.exe
Start-Process .\vs_BuildTools.exe -Verb RunAs -ArgumentList `
  '--passive','--wait','--norestart',
  '--add','Microsoft.VisualStudio.Component.VC.Tools.x86.x64',
  '--add','Microsoft.VisualStudio.Component.Windows11SDK.26100'
```

## 3. Node

Node 24.15.0 / npm 11.12.1, already present system-wide. Frontend dependencies install with
`npm --prefix ui install`.

## Verifying

```bash
source ./env.sh
rustc -vV          # host must read x86_64-pc-windows-msvc
cargo --version
./scripts/check.sh # fmt, clippy, test, tsc
```

## Rejected: the MinGW / `windows-gnu` route

Evaluated during M0 specifically to avoid the 3 GB elevated MSVC install, and rejected.

- **w64devkit 2.10.0** gets as far as compiling but every link fails with
  `cannot find -lgcc_eh`. Its GCC merges exception handling into `libgcc` and ships no
  separate `libgcc_eh.a`, which Rust's `windows-gnu` target hardcodes.
- A MSVCRT-flavoured MinGW-w64 (WinLibs) would likely clear that, but `windows-gnu` is not a
  target Tauri builds or tests against. Shipping a daily-driver app on an unverified target
  makes every future WebView2 problem ambiguous between our code and the toolchain.
- Note for anyone retrying: the current WinLibs releases are **UCRT**-based, while Rust's
  `windows-gnu` links `msvcrt`. The MSVCRT variant is the one to fetch, not the newest.

MSVC uninstalls cleanly, so the cost of the supported path is transient disk rather than a
permanent footprint.
