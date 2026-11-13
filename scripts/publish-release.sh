#!/bin/bash
set -e

APP_NAME="LocalCue"
VERSION=$(grep '^version' src-tauri/Cargo.toml | head -1 | sed 's/.*= *"\(.*\)"/\1/')
TAG="v${VERSION}"
OUT="releases"

APP_DIR="${OUT}/${APP_NAME}-${VERSION}.app"
DMG="${OUT}/${APP_NAME}-${VERSION}.dmg"
PKG="${OUT}/${APP_NAME}-${VERSION}.pkg"
APP_ZIP="${OUT}/${APP_NAME}-${VERSION}.app.zip"
PKG_STABLE="${OUT}/${APP_NAME}.pkg"   # fixed name for stable download URL

# ── checks ───────────────────────────────────────────────────────────────────

gh auth status >/dev/null 2>&1 || { echo "error: not logged in — run 'gh auth login'"; exit 1; }

for f in "$APP_DIR" "$DMG" "$PKG"; do
  [ -e "$f" ] || { echo "error: $f not found — run 'npm run release' first"; exit 1; }
done

cp "$PKG" "$PKG_STABLE"

# ── zip .app ──────────────────────────────────────────────────────────────────

echo "==> zipping ${APP_NAME}-${VERSION}.app..."
rm -f "$APP_ZIP"
cd "$OUT"
zip -qr "${APP_NAME}-${VERSION}.app.zip" "${APP_NAME}-${VERSION}.app"
cd ..

# ── tag ───────────────────────────────────────────────────────────────────────

TAG_EXISTS=false
if git tag --list "$TAG" | grep -q "$TAG"; then
  TAG_EXISTS=true
  echo "==> tag $TAG already exists, skipping tag creation"
else
  echo "==> creating tag ${TAG}..."
  git tag "$TAG"
  git push origin "$TAG"
fi

# ── build release notes ───────────────────────────────────────────────────────

# find last published release on github (not just tag)
LAST_RELEASE=$(gh release list --limit 20 --json tagName,isDraft \
  | grep -v '"isDraft":true' \
  | grep -o '"tagName":"[^"]*"' \
  | sed 's/"tagName":"\(.*\)"/\1/' \
  | grep -vx "$TAG" \
  | head -1)

echo "==> building release notes..."

if [ -z "$LAST_RELEASE" ]; then
  # no previous release — take all commits
  COMMIT_RANGE="HEAD"
  NOTES_HEADER="Initial release"
else
  COMMIT_RANGE="${LAST_RELEASE}..HEAD"
  NOTES_HEADER="Changes since ${LAST_RELEASE}"

  # warn about any skipped versions between last release and this one
  SKIPPED=$(git tag --list 'v*' --sort=version:refname \
    | awk -v last="$LAST_RELEASE" -v cur="$TAG" \
      'found && $0 != cur {print} $0 == last {found=1}')
  if [ -n "$SKIPPED" ]; then
    echo "    (includes commits from skipped versions: $(echo $SKIPPED | tr '\n' ' '))"
  fi
fi

# Collect only app commits (feat/fix/perf/refactor/ui with version), skip repo/infra ones.
# The subject becomes the top-level bullet; the body is folded into nested ones —
# every "- " line, and every prose paragraph, each joined back from its wrapped
# lines. Trailers (Co-Authored-By and friends) are dropped.
# Commits are NUL-separated because each record spans multiple lines.
APP_COMMITS=$(git log "$COMMIT_RANGE" \
  -z \
  --pretty=format:"%s%n%b" \
  --no-merges \
  | tr '\0' '\036' \
  | awk -v RS='\036' '
      {
        n = split($0, line, "\n")
        subject = line[1]
        if (subject !~ /^(feat|fix|perf|refactor|ui)\(v[0-9]+\.[0-9]+\.[0-9]+\): /) next

        type = subject
        sub(/\(.*$/, "", type)
        icon = "✨"
        if      (type == "fix")      icon = "🐛"
        else if (type == "perf")     icon = "⚡"
        else if (type == "refactor") icon = "♻️"
        else if (type == "ui")       icon = "🎨"

        text = subject
        sub(/^[a-z]+\([^)]*\): */, "", text)
        print "- " icon " " text

        item = ""
        for (i = 2; i <= n; i++) {
          l = line[i]
          if (l ~ /^([A-Za-z][A-Za-z-]*-[Bb]y|Claude-Session): /) continue  # trailers

          if (l ~ /^[ \t]*[-*] /) {                       # bullet, indented or not
            if (item != "") print "  - " item
            item = l
            sub(/^[ \t]*[-*] +/, "", item)
          } else if (l ~ /^[ \t]*$/) {                    # blank line ends the item
            if (item != "") print "  - " item
            item = ""
          } else if (item != "") {                        # wrapped continuation
            sub(/^[ \t]+/, "", l)
            item = item " " l
          } else {                                        # start of a prose paragraph
            item = l
          }
        }
        if (item != "") print "  - " item
      }' \
  || true)

if [ -z "$APP_COMMITS" ]; then
  APP_COMMITS="_no app changes in this release_"
fi

RELEASE_NOTES="${NOTES_HEADER}

${APP_COMMITS}"

if [ -n "$SKIPPED" ]; then
  RELEASE_NOTES="${RELEASE_NOTES}

> includes commits from unpublished versions: ${SKIPPED}"
fi

# ── create or update github release ──────────────────────────────────────────

RELEASE_EXISTS=$(gh release view "$TAG" --json tagName -q .tagName 2>/dev/null || echo "")

if [ -n "$RELEASE_EXISTS" ]; then
  echo "==> release $TAG already exists, uploading assets..."
  gh release upload "$TAG" "$APP_ZIP" "$DMG" "$PKG" "$PKG_STABLE" --clobber
  gh release edit "$TAG" --notes "$RELEASE_NOTES"
else
  echo "==> creating GitHub release ${TAG}..."
  gh release create "$TAG" \
    --title "${APP_NAME} ${VERSION}" \
    --notes "$RELEASE_NOTES" \
    "$APP_ZIP" \
    "$DMG" \
    "$PKG" \
    "$PKG_STABLE"
fi

echo ""
echo "done: https://github.com/$(gh repo view --json nameWithOwner -q .nameWithOwner)/releases/tag/${TAG}"
