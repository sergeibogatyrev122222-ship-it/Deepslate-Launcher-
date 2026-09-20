#!/usr/bin/env bash
# Release build: frontend bundle, then Tauri binary + installer.
#
# The two steps are explicit rather than wired through tauri.conf.json's
# beforeBuildCommand, because that hook's working directory is not the config
# directory in a cargo-workspace layout and silently resolved to the wrong path.
set -euo pipefail

cd "$(dirname "$0")/.."
root="$PWD"
# shellcheck source=../env.sh
source ./env.sh

echo "== frontend =="
npm --prefix "$root/ui" run build

echo "== tauri =="
cd "$root/crates/ds-app"
"$root/ui/node_modules/.bin/tauri" build "$@"

echo
echo "== artifacts =="
find "$root/target/release/bundle" -type f \( -name '*.exe' -o -name '*.msi' -o -name '*.deb' -o -name '*.AppImage' \) \
  -printf '%10s  %p\n' 2>/dev/null | sort -rn || true
