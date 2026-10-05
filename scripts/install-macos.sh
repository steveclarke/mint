#!/usr/bin/env bash
# Builds mint.app (window, menu bar, hotkey) with the mint CLI inside it,
# installs it to /Applications, links the CLI onto PATH and turns on
# launch at login. Ad hoc signed: for the machine it is built on.
#
#   scripts/install-macos.sh [--no-login]
set -euo pipefail

root="$(cd "$(dirname "$0")/.." && pwd)"
app_dir="$root/crates/mint-app"
triple="$(rustc -vV | awk '/^host:/ {print $2}')"
bin_dir="${MINT_BIN_DIR:-$HOME/.local/bin}"
login=on
[[ "${1:-}" == "--no-login" ]] && login=off

echo "==> Building the mint CLI"
cargo build --release --manifest-path "$root/Cargo.toml" -p mint-cli
mkdir -p "$app_dir/binaries"
cp "$root/target/release/mint" "$app_dir/binaries/mint-$triple"

echo "==> Building mint.app"
(cd "$app_dir" && npx --yes @tauri-apps/cli@2 build --bundles app \
  --config '{"bundle":{"externalBin":["binaries/mint"],"macOS":{"signingIdentity":"-"}}}')
built="$root/target/release/bundle/macos/mint.app"

echo "==> Installing to /Applications"
pkill -x mint-app 2>/dev/null || true
# Wait (bounded) for the old app to exit; launching before then hands the
# arguments to the dying instance and drops --launch-at-login.
for _ in $(seq 1 50); do
  pgrep -x mint-app >/dev/null || break
  sleep 0.2
done
if pgrep -x mint-app >/dev/null; then
  echo "mint-app did not exit within 10 s; quit it from the menu bar and run again." >&2
  exit 1
fi
if [[ -d /Applications/mint.app ]]; then
  # `trash` ships with macOS 14+; the bundle supports 12.
  if command -v trash >/dev/null; then
    trash /Applications/mint.app
  else
    mv /Applications/mint.app "$HOME/.Trash/mint.app.$(date +%s)"
  fi
fi
ditto "$built" /Applications/mint.app
codesign --verify --deep --strict /Applications/mint.app

echo "==> Linking the CLI into $bin_dir"
mkdir -p "$bin_dir"
ln -sf /Applications/mint.app/Contents/MacOS/mint "$bin_dir/mint"

echo "==> Starting mint (launch at login: $login)"
open -a /Applications/mint.app --args --launch-at-login "$login"

echo "Installed. Hotkey and settings: ~/.config/mint/config.toml"
