#!/bin/bash
set -e

# read current version from package.json
CURRENT=$(grep '"version"' package.json | head -1 | sed 's/.*"\([0-9]*\.[0-9]*\.[0-9]*\)".*/\1/')
MAJOR=$(echo "$CURRENT" | cut -d. -f1)
MINOR=$(echo "$CURRENT" | cut -d. -f2)
PATCH=$(echo "$CURRENT" | cut -d. -f3)
NEW="${MAJOR}.${MINOR}.$((PATCH + 1))"

echo "${CURRENT} → ${NEW}"

# package.json
sed -i '' "s/\"version\": \"${CURRENT}\"/\"version\": \"${NEW}\"/" package.json

# Cargo.toml (only the first version line — the package version)
sed -i '' "1,/^version = \"${CURRENT}\"/ s/^version = \"${CURRENT}\"/version = \"${NEW}\"/" src-tauri/Cargo.toml

# tauri.conf.json
sed -i '' "s/\"version\": \"${CURRENT}\"/\"version\": \"${NEW}\"/" src-tauri/tauri.conf.json
