#!/usr/bin/env bash
set -euo pipefail

# Generate status bar PNGs and .icns from SVG sources.
# Requires: rsvg-convert (from librsvg), iconutil (macOS built-in)

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
PROJECT_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
RESOURCES="$PROJECT_ROOT/resources"

echo "Generating status bar template images..."
rsvg-convert -w 18 -h 18 "$RESOURCES/statusbar-template.svg" -o "$RESOURCES/statusbar.png"
rsvg-convert -w 36 -h 36 "$RESOURCES/statusbar-template.svg" -o "$RESOURCES/statusbar@2x.png"
echo "  statusbar.png (18x18)"
echo "  statusbar@2x.png (36x36)"

echo "Generating app icon (mcpsm.icns)..."
ICONSET_DIR=$(mktemp -d)/mcpsm.iconset
mkdir -p "$ICONSET_DIR"
for size in 16 32 128 256 512; do
    rsvg-convert -w "$size" -h "$size" "$PROJECT_ROOT/docs/resources/logo.svg" \
        -o "$ICONSET_DIR/icon_${size}x${size}.png"
    size2=$((size * 2))
    rsvg-convert -w "$size2" -h "$size2" "$PROJECT_ROOT/docs/resources/logo.svg" \
        -o "$ICONSET_DIR/icon_${size}x${size}@2x.png"
done
iconutil -c icns "$ICONSET_DIR" -o "$RESOURCES/mcpsm.icns"
rm -rf "$(dirname "$ICONSET_DIR")"
echo "  mcpsm.icns"

echo "Done."
