#!/usr/bin/env bash
# Build a bundled Pi binary + extensions for Hyvemind.
#
# Reads the pinned version from scripts/pi-version.txt, bun-installs Pi and
# the required extensions into a sandbox under build/pi-build/, bun-compiles
# Pi into a single executable named with the host's Tauri target triple (so
# Tauri externalBin picks it up), copies Pi's runtime support files
# (package.json, theme/, dist/, etc.) next to the binary, and snapshots the
# extension packages into app/src-tauri/binaries/pi-extensions/.
#
# Re-running is cheap: if the stamp file matches the requested version, the
# script exits immediately. Bump scripts/pi-version.txt to force a rebuild.

set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
PI_VERSION="$(tr -d '[:space:]' < "$REPO_ROOT/scripts/pi-version.txt")"
BUILD_DIR="$REPO_ROOT/build/pi-build"
EXT_BUILD_DIR="$REPO_ROOT/build/ext-build"
BIN_DIR="$REPO_ROOT/app/src-tauri/binaries"
EXT_DIR="$BIN_DIR/pi-extensions"
STAMP_FILE="$BIN_DIR/.pi-version"

EXTENSIONS=(pi-web-access pi-subagents pi-mcp-adapter)
LOCAL_EXTENSIONS=(hyvemind-providers hyvemind-handoff)
LOCAL_EXT_SRC_DIR="$REPO_ROOT/app/src-tauri/pi-extensions"

copy_local_extensions() {
  mkdir -p "$EXT_DIR"
  for ext in "${LOCAL_EXTENSIONS[@]}"; do
    src="$LOCAL_EXT_SRC_DIR/$ext"
    if [ ! -d "$src" ]; then
      echo "[build-pi] missing local extension source: $src" >&2
      exit 1
    fi
    rm -rf "$EXT_DIR/$ext"
    cp -R "$src" "$EXT_DIR/$ext"
  done
}

case "$(uname -s)-$(uname -m)" in
  Darwin-arm64)   TAURI_TARGET="aarch64-apple-darwin";       BUN_TARGET="bun-darwin-arm64"; BIN_EXT="" ;;
  Darwin-x86_64)  TAURI_TARGET="x86_64-apple-darwin";        BUN_TARGET="bun-darwin-x64";   BIN_EXT="" ;;
  Linux-x86_64)   TAURI_TARGET="x86_64-unknown-linux-gnu";   BUN_TARGET="bun-linux-x64";    BIN_EXT="" ;;
  Linux-aarch64)  TAURI_TARGET="aarch64-unknown-linux-gnu";  BUN_TARGET="bun-linux-arm64";  BIN_EXT="" ;;
  MINGW64_NT*-x86_64|MSYS_NT*-x86_64) TAURI_TARGET="x86_64-pc-windows-msvc"; BUN_TARGET="bun-windows-x64"; BIN_EXT=".exe" ;;
  *) echo "[build-pi] unsupported host: $(uname -s)-$(uname -m)" >&2; exit 1 ;;
esac

OUT_BINARY="$BIN_DIR/pi-$TAURI_TARGET$BIN_EXT"

if [ -f "$OUT_BINARY" ] && [ -f "$STAMP_FILE" ] && [ "$(cat "$STAMP_FILE")" = "$PI_VERSION" ]; then
  copy_local_extensions
  echo "[build-pi] $OUT_BINARY already at v$PI_VERSION — local extensions refreshed"
  exit 0
fi

command -v bun >/dev/null || {
  echo "[build-pi] 'bun' is required to build Pi. Install from https://bun.sh" >&2
  exit 1
}

echo "[build-pi] building Pi v$PI_VERSION for $TAURI_TARGET"

rm -rf "$BUILD_DIR" "$EXT_BUILD_DIR"
mkdir -p "$BUILD_DIR" "$EXT_BUILD_DIR" "$BIN_DIR"

# Sandbox 1: Pi itself. Its node_modules is heavy but throwaway — the
# entire JS bundle ends up inside the bun-compiled executable.
cd "$BUILD_DIR"
cat > package.json <<EOF
{ "name": "hyvemind-pi-build", "private": true, "type": "module" }
EOF
bun add --exact "@earendil-works/pi-coding-agent@$PI_VERSION" >/dev/null
PI_ENTRY="$BUILD_DIR/node_modules/@earendil-works/pi-coding-agent/dist/cli.js"
[ -f "$PI_ENTRY" ] || { echo "[build-pi] expected entry not found: $PI_ENTRY" >&2; exit 1; }

# Sandbox 2: just the extensions. The flat node_modules here is what we
# ship — its only contents are the extensions + their transitive deps,
# nothing from Pi itself. Pi extensions are loaded via jiti at runtime
# (not bun-compiled), so they need real on-disk module resolution.
cd "$EXT_BUILD_DIR"
cat > package.json <<EOF
{ "name": "hyvemind-pi-extensions", "private": true, "type": "module" }
EOF
bun add --exact "${EXTENSIONS[@]}" >/dev/null

bun build --compile --target="$BUN_TARGET" \
  "$PI_ENTRY" --outfile "$OUT_BINARY"
chmod +x "$OUT_BINARY"

# Pi reads multiple files at runtime via fs.readFileSync(<path-relative-to-binary>):
# package.json (version), theme/dark.json + theme/light.json (theme init in
# main() before mode selection), dist/modes/interactive/assets/* (TUI assets),
# and potentially more. Bun-compile doesn't embed assets, so we ship the full
# Pi npm package contents (~12MB) next to the binary. Also flatten the theme
# subdir up to <bindir>/theme/ because the compiled binary's path rewriting
# expects it there rather than at its source location dist/modes/interactive/theme/.
PI_PKG="$BUILD_DIR/node_modules/@earendil-works/pi-coding-agent"
for entry in "$PI_PKG"/*; do
  name="$(basename "$entry")"
  # Don't clobber the compiled binary or its stamp file.
  case "$name" in
    pi-*|.pi-version|pi-extensions) continue ;;
  esac
  rm -rf "$BIN_DIR/$name"
  cp -R "$entry" "$BIN_DIR/$name"
done
if [ -d "$PI_PKG/dist/modes/interactive/theme" ]; then
  rm -rf "$BIN_DIR/theme"
  cp -R "$PI_PKG/dist/modes/interactive/theme" "$BIN_DIR/theme"
fi

rm -rf "$EXT_DIR"
mkdir -p "$EXT_DIR"
for ext in "${EXTENSIONS[@]}"; do
  src="$EXT_BUILD_DIR/node_modules/$ext"
  if [ ! -d "$src" ]; then
    echo "[build-pi] missing extension package after bun add: $ext" >&2
    exit 1
  fi
  cp -R "$src" "$EXT_DIR/$ext"
done
copy_local_extensions

# Ship the extensions' flat node_modules. Each extension's source resolves
# imports by walking up to <ext>/node_modules first (none — we don't copy
# per-package node_modules), then to pi-extensions/node_modules (this one).
# Exclude the extension packages themselves from the shared node_modules
# since they already live at the top level — keeps the bundle smaller.
cp -R "$EXT_BUILD_DIR/node_modules" "$EXT_DIR/node_modules"
for ext in "${EXTENSIONS[@]}"; do
  rm -rf "$EXT_DIR/node_modules/$ext"
done

# .bin/ holds relative symlinks that don't survive copying (Tauri's resource
# glob rejects broken symlinks) and isn't read at runtime by jiti-loaded
# extensions anyway.
rm -rf "$EXT_DIR/node_modules/.bin"

echo "$PI_VERSION" > "$STAMP_FILE"

bin_size=$(du -h "$OUT_BINARY" | cut -f1)
ext_size=$(du -sh "$EXT_DIR" | cut -f1)
echo "[build-pi] ok"
echo "  binary:     $OUT_BINARY ($bin_size)"
echo "  extensions: $EXT_DIR ($ext_size, $((${#EXTENSIONS[@]} + ${#LOCAL_EXTENSIONS[@]})) packages)"
