# Release Checklist

## Pre-Release
- Ensure `main` is clean and up to date (`git status -sb`, `git pull --rebase`).
- Run quality gates: `cargo fmt --check`, `cargo test -q`, `cargo clippy -- -D warnings`.
- Confirm changelog/release notes are updated (`docs/changelog.md`, `docs/release/vX.Y.Z-draft.md`).

## Tag and Publish
- Create annotated tag: `git tag -a vX.Y.Z -m "vX.Y.Z: <summary>"`.
- Push tag: `git push origin vX.Y.Z`.
- Publish release: `gh release create vX.Y.Z --title "vX.Y.Z" --notes-file docs/release/vX.Y.Z-draft.md`.
- Verify release metadata: `gh release view vX.Y.Z --json name,tagName,url,publishedAt`.

## Post-Release
- Bump `Cargo.toml` to next development version.
- Run post-release tmux smoke: `make smoke-post-release`.
- Optional overrides: `LOOPMUX_SMOKE_SESSION`, `LOOPMUX_SMOKE_TARGET`, `LOOPMUX_SMOKE_PROMPT`, `LOOPMUX_SMOKE_LOG`.
- Log smoke results and release references in `docs/changelog.md`.
