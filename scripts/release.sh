#!/bin/bash
set -e

APP_NAME="LocalCue"
BUNDLE_DIR="src-tauri/target/release/bundle"
OUT="releases"

# ── build ────────────────────────────────────────────────────────────────────

echo "==> building app..."
# Only the .app is built by Tauri; the .dmg is made below with hdiutil (Tauri's DMG step
# scripts Finder via AppleScript, which fails in headless / sandboxed shells).
CI=true npm run tauri build -- --bundles app

VERSION=$(grep '^version' src-tauri/Cargo.toml | head -1 | sed 's/.*= *"\(.*\)"/\1/')
echo "==> version: ${VERSION}"

# ── build pkg ────────────────────────────────────────────────────────────────

echo "==> building pkg installer..."
bash scripts/build-pkg.sh "$OUT"

# ── collect artifacts ────────────────────────────────────────────────────────

echo "==> collecting artifacts into ${OUT}/..."
mkdir -p "$OUT"

# .app (directory bundle — copy with versioned name)
APP_SRC="${BUNDLE_DIR}/macos/${APP_NAME}.app"
APP_DST="${OUT}/${APP_NAME}-${VERSION}.app"
rm -rf "$APP_DST"
cp -R "$APP_SRC" "$APP_DST"
echo "    ${APP_NAME}-${VERSION}.app"

# .dmg: the app plus an Applications shortcut
DMG_DST="${OUT}/${APP_NAME}-${VERSION}.dmg"
DMG_STAGE=$(mktemp -d)
cp -R "$APP_SRC" "$DMG_STAGE/"
ln -s /Applications "$DMG_STAGE/Applications"
rm -f "$DMG_DST"
hdiutil create -quiet -volname "$APP_NAME" -srcfolder "$DMG_STAGE" -ov -format UDZO "$DMG_DST"
rm -rf "$DMG_STAGE"
echo "    ${APP_NAME}-${VERSION}.dmg"

# .pkg already written to $OUT by build-pkg.sh
echo "    ${APP_NAME}-${VERSION}.pkg"

# ── summary ──────────────────────────────────────────────────────────────────

echo ""
echo "done — ${OUT}/"
ls -lh "$OUT" | grep "LocalCue-${VERSION}"
