#!/bin/bash
set -e

APP_NAME="LocalCue"
BINARY_NAME="localcue"
APP_BUNDLE="src-tauri/target/release/bundle/macos/${APP_NAME}.app"
VERSION=$(grep '^version' src-tauri/Cargo.toml | head -1 | sed 's/.*= *"\(.*\)"/\1/')
OUTPUT_DIR="${1:-dist-pkg}"
PKG_OUTPUT="${OUTPUT_DIR}/${APP_NAME}-${VERSION}.pkg"

# ── check prerequisites ──────────────────────────────────────────────────────

if [ ! -d "$APP_BUNDLE" ]; then
  echo "error: app bundle not found at $APP_BUNDLE"
  echo "run 'npm run tauri build' first"
  exit 1
fi

command -v pkgbuild >/dev/null 2>&1 || { echo "error: pkgbuild not found (requires Xcode Command Line Tools)"; exit 1; }
command -v productbuild >/dev/null 2>&1 || { echo "error: productbuild not found (requires Xcode Command Line Tools)"; exit 1; }

echo "building ${APP_NAME} ${VERSION} installer..."

# ── prepare staging dirs ─────────────────────────────────────────────────────

STAGING=$(mktemp -d)
SCRIPTS_DIR=$(mktemp -d)
trap 'rm -rf "$STAGING" "$SCRIPTS_DIR"' EXIT

APP_STAGING="${STAGING}/Applications"
mkdir -p "$APP_STAGING"
cp -R "$APP_BUNDLE" "$APP_STAGING/"

# ── preinstall: remove old version ──────────────────────────────────────────

cat > "${SCRIPTS_DIR}/preinstall" << 'EOF'
#!/bin/bash
rm -rf "/Applications/LocalCue.app"
exit 0
EOF
chmod +x "${SCRIPTS_DIR}/preinstall"

# ── postinstall: fix permissions and quarantine ──────────────────────────────

cat > "${SCRIPTS_DIR}/postinstall" << 'EOF'
#!/bin/bash
APP="/Applications/LocalCue.app"
chmod +x "${APP}/Contents/MacOS/localcue"
xattr -cr "$APP"
exit 0
EOF
chmod +x "${SCRIPTS_DIR}/postinstall"

# ── build component pkg ──────────────────────────────────────────────────────

mkdir -p "$OUTPUT_DIR"
COMPONENT_PKG="${OUTPUT_DIR}/${APP_NAME}-component.pkg"

pkgbuild \
  --root "$STAGING" \
  --scripts "$SCRIPTS_DIR" \
  --identifier "io.github.localcue.desktop" \
  --version "$VERSION" \
  --install-location "/" \
  "$COMPONENT_PKG"

# ── wrap in product pkg with welcome screen ──────────────────────────────────

DIST_XML=$(mktemp -t localcue-dist).xml
trap 'rm -rf "$STAGING" "$SCRIPTS_DIR" "$DIST_XML"' EXIT

cat > "$DIST_XML" << EOF
<?xml version="1.0" encoding="utf-8"?>
<installer-gui-script minSpecVersion="1">
  <title>${APP_NAME} ${VERSION}</title>
  <options customize="never" require-scripts="false" rootVolumeOnly="true"/>
  <domains enable_localSystem="true"/>
  <pkg-ref id="io.github.localcue.desktop"/>
  <choices-outline>
    <line choice="io.github.localcue.desktop"/>
  </choices-outline>
  <choice id="io.github.localcue.desktop" visible="false">
    <pkg-ref id="io.github.localcue.desktop"/>
  </choice>
  <pkg-ref id="io.github.localcue.desktop" version="${VERSION}" onConclusion="none">${APP_NAME}-component.pkg</pkg-ref>
</installer-gui-script>
EOF

productbuild \
  --distribution "$DIST_XML" \
  --package-path "$OUTPUT_DIR" \
  "$PKG_OUTPUT"

rm -f "$COMPONENT_PKG"

echo ""
echo "done: $PKG_OUTPUT"
