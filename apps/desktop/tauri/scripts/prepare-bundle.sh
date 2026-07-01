#!/usr/bin/env bash
#
# Tauri `beforeBuildCommand` hook (see ../tauri.conf.json).
#
# Builds the `micracode-api` backend in release and stages it into the Tauri
# bundle's `resources/backend/` dir, then builds the Next.js web frontend. The
# staged binary is picked up by `bundle.resources` in tauri.conf.json and, at
# runtime, by `backend_binary_path()` in src/lib.rs.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
TAURI_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"
REPO_ROOT="$(cd "$TAURI_DIR/../../.." && pwd)"
API_DIR="$REPO_ROOT/desktop/api"
WEB_DIR="$REPO_ROOT/apps/web"

BIN_NAME="micracode-api"
case "$(uname -s)" in
  MINGW* | MSYS* | CYGWIN*) BIN_NAME="micracode-api.exe" ;;
esac

echo "[prepare-bundle] building backend ($BIN_NAME) in release…"
(cd "$API_DIR" && cargo build --release --bin micracode-api)

DEST_DIR="$TAURI_DIR/resources/backend"
mkdir -p "$DEST_DIR"
cp "$API_DIR/target/release/$BIN_NAME" "$DEST_DIR/$BIN_NAME"
echo "[prepare-bundle] staged backend → $DEST_DIR/$BIN_NAME"

echo "[prepare-bundle] building web frontend…"
(cd "$WEB_DIR" && bun run build)

echo "[prepare-bundle] done."
