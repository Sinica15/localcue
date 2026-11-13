# LocalCue — agent instructions

## Commit format

**App changes** — include the version in parentheses after the type:

```
feat(v0.1.1): add drag & drop
fix(v0.1.2): crash on empty script
perf(v0.1.3): faster text measuring
refactor(v0.1.4): simplify hub routing
ui(v0.1.5): new toolbar design
```

**Repo changes** — no version, no parentheses:

```
chore: update .gitignore
build: add release scripts
ci: configure workflows
docs: update README
```

## Version bump rule

Only bump the patch version when the commit includes **app changes** (feat/fix/perf/refactor/ui).
For repo-only commits (chore/build/ci/docs) do NOT bump the version.

When a bump is needed, run:

```bash
bash scripts/bump-version.sh
```

Then stage the changed files before committing:

```bash
git add package.json src-tauri/Cargo.toml src-tauri/tauri.conf.json
```

## Version files

All three must always have the same version:
- `package.json` — `"version"` field
- `src-tauri/Cargo.toml` — `version =` field
- `src-tauri/tauri.conf.json` — `"version"` field

## Checks before committing app changes

```bash
npm run build && npm run check:mobile
cd src-tauri && cargo fmt --check && cargo clippy --all-targets -- -D warnings && cargo test
```

## Releasing

```bash
npm run release   # .app + .dmg + .pkg into releases/
npm run publish   # tag + GitHub release with notes built from app commits
```

## Notes

- The phone page (`mobile/`, `shared/`) is embedded into the binary with `include_str!`; its build id changes with its content, so open phone pages reload themselves after an update.
- Position messages are absolute character offsets (UTF-16), never deltas.
