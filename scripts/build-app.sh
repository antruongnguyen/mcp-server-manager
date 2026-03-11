#!/usr/bin/env bash
set -euo pipefail

# Build MCPSM.app — a macOS .app bundle for the MCP Server Manager.
#
# Usage:
#   ./scripts/build-app.sh
#
# Output:
#   target/release/MCPSM.app/

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
PROJECT_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"

APP_NAME="MCPSM"
APP_DIR="$PROJECT_ROOT/target/release/$APP_NAME.app"
CONTENTS="$APP_DIR/Contents"

echo "Building mcpsm in release mode..."
cargo build --release

echo "Assembling $APP_NAME.app..."
rm -rf "$APP_DIR"
mkdir -p "$CONTENTS/MacOS"
mkdir -p "$CONTENTS/Resources"

cp "$PROJECT_ROOT/target/release/mcpsm" "$CONTENTS/MacOS/mcpsm"
cp "$PROJECT_ROOT/Info.plist"            "$CONTENTS/Info.plist"
cp "$PROJECT_ROOT/resources/mcpsm.icns"  "$CONTENTS/Resources/mcpsm.icns"

echo "Built $APP_DIR"
