#!/usr/bin/env bash
set -euo pipefail

usage() {
	cat <<'EOF'
Usage:
  ./release/ship.sh <version> [--repo owner/name] [--tap-repo owner/name] [--no-brew]

Examples:
  ./release/ship.sh 0.1.25
  ./release/ship.sh 0.1.25 --repo dmoliveira/loopmux --tap-repo dmoliveira/homebrew-tap

Notes:
  - Runs non-interactively end-to-end: bump -> PR -> merge -> tag -> release -> tap update -> local brew verify.
  - Requires: gh, git, cargo, curl, python3, shasum; brew is optional with --no-brew.
  - Must run from a clean local main branch.
EOF
}

require_cmd() {
	local cmd="$1"
	command -v "$cmd" >/dev/null 2>&1 || {
		printf "error: required command not found: %s\n" "$cmd" >&2
		exit 1
	}
}

repo_from_origin() {
	local url
	url="$(git remote get-url origin)"
	if [[ "$url" =~ github.com[:/]([^/]+/[^/.]+)(\.git)?$ ]]; then
		printf "%s\n" "${BASH_REMATCH[1]}"
		return
	fi
	printf "error: unable to parse GitHub repo from origin URL: %s\n" "$url" >&2
	exit 1
}

if [[ "${1:-}" == "-h" || "${1:-}" == "--help" || $# -lt 1 ]]; then
	usage
	exit 0
fi

VERSION="$1"
shift

REPO=""
TAP_REPO="dmoliveira/homebrew-tap"
NO_BREW=0

while [[ $# -gt 0 ]]; do
	case "$1" in
	--repo)
		REPO="$2"
		shift 2
		;;
	--tap-repo)
		TAP_REPO="$2"
		shift 2
		;;
	--no-brew)
		NO_BREW=1
		shift
		;;
	*)
		printf "error: unknown argument: %s\n" "$1" >&2
		usage
		exit 1
		;;
	esac
done

if [[ ! "$VERSION" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]]; then
	printf "error: version must look like X.Y.Z (got: %s)\n" "$VERSION" >&2
	exit 1
fi

TAG="v$VERSION"
BRANCH="loopmux-release-${VERSION//./-}"
TAP_BRANCH="loopmux-${VERSION//./-}-release"

require_cmd git
require_cmd gh
require_cmd cargo
require_cmd curl
require_cmd python3
require_cmd shasum
if [[ "$NO_BREW" -eq 0 ]]; then
	require_cmd brew
fi

if [[ -z "$REPO" ]]; then
	REPO="$(repo_from_origin)"
fi

if [[ "$(git rev-parse --abbrev-ref HEAD)" != "main" ]]; then
	printf "error: run this script from main branch\n" >&2
	exit 1
fi

if [[ -n "$(git status --porcelain)" ]]; then
	printf "error: working tree is dirty; commit or stash changes first\n" >&2
	exit 1
fi

if git rev-parse -q --verify "refs/tags/$TAG" >/dev/null; then
	printf "error: tag already exists locally: %s\n" "$TAG" >&2
	exit 1
fi

printf "==> Sync main\n"
git pull --rebase

if git ls-remote --exit-code --tags origin "refs/tags/$TAG" >/dev/null 2>&1; then
	printf "error: tag already exists on origin: %s\n" "$TAG" >&2
	exit 1
fi

if git show-ref --verify --quiet "refs/heads/$BRANCH"; then
	printf "error: local branch already exists: %s\n" "$BRANCH" >&2
	exit 1
fi

if git ls-remote --exit-code --heads origin "$BRANCH" >/dev/null 2>&1; then
	printf "error: remote branch already exists: %s\n" "$BRANCH" >&2
	exit 1
fi

printf "==> Create release branch %s\n" "$BRANCH"
git checkout -b "$BRANCH"

printf "==> Bump version to %s\n" "$VERSION"
VERSION_TO_SET="$VERSION" python3 - <<'PY'
from pathlib import Path
import os
import re

version = os.environ["VERSION_TO_SET"]
for file_name in ["Cargo.toml", "Cargo.lock"]:
    path = Path(file_name)
    text = path.read_text()
    text = re.sub(r'version = "[0-9]+\.[0-9]+\.[0-9]+"', f'version = "{version}"', text, count=1)
    path.write_text(text)
PY

printf "==> Validate\n"
cargo fmt --check
cargo test

git add Cargo.toml Cargo.lock
git commit -m "Bump version to $VERSION"
git push -u origin "$BRANCH"

printf "==> Open and merge release PR\n"
PR_NUMBER="$(gh api "repos/$REPO/pulls" \
	-f title="Bump version to $VERSION" \
	-f head="$BRANCH" \
	-f base="main" \
	-f body="## Summary
- bump package version to $VERSION for release

## Validation
- cargo fmt --check
- cargo test" \
	--jq '.number')"
gh api -X PUT "repos/$REPO/pulls/$PR_NUMBER/merge" -f merge_method=merge >/dev/null

printf "==> Tag and release %s\n" "$TAG"
git checkout main
git pull --rebase
git tag -a "$TAG" -m "Release $TAG"
git push origin "$TAG"
gh release create "$TAG" --title "$TAG" --notes "Automated release for $TAG"

printf "==> Compute source SHA256\n"
TARBALL_URL="https://github.com/$REPO/archive/refs/tags/$TAG.tar.gz"
SHA256="$(curl -fsSL "$TARBALL_URL" | shasum -a 256 | awk '{print $1}')"

printf "==> Update tap formula in %s\n" "$TAP_REPO"
TAP_MAIN_SHA="$(gh api "repos/$TAP_REPO/git/ref/heads/main" --jq '.object.sha')"
FORMULA_SHA="$(gh api "repos/$TAP_REPO/contents/Formula/loopmux.rb" --jq '.sha')"
if gh api "repos/$TAP_REPO/git/ref/heads/$TAP_BRANCH" >/dev/null 2>&1; then
	gh api -X DELETE "repos/$TAP_REPO/git/refs/heads/$TAP_BRANCH" >/dev/null
fi
gh api "repos/$TAP_REPO/git/refs" -f ref="refs/heads/$TAP_BRANCH" -f sha="$TAP_MAIN_SHA" >/dev/null

FORMULA_B64="$(
	python3 - <<PY
import base64

repo = "$REPO"
tag = "$TAG"
sha = "$SHA256"

formula = f'''class Loopmux < Formula
  desc "Loop prompts into tmux panes with triggers and delays"
  homepage "https://github.com/{repo}"
  url "https://github.com/{repo}/archive/refs/tags/{tag}.tar.gz"
  sha256 "{sha}"
  license "MIT"

  depends_on "rust" => :build

  def install
    system "cargo", "install", *std_cargo_args
  end

  test do
    assert_match "loopmux", shell_output("#{bin}/loopmux --help")
  end
end
'''
print(base64.b64encode(formula.encode()).decode())
PY
)"

gh api -X PUT "repos/$TAP_REPO/contents/Formula/loopmux.rb" \
	-f message="Update loopmux formula to $TAG" \
	-f content="$FORMULA_B64" \
	-f sha="$FORMULA_SHA" \
	-f branch="$TAP_BRANCH" >/dev/null

TAP_PR_NUMBER="$(gh api "repos/$TAP_REPO/pulls" \
	-f title="Update loopmux formula to $TAG" \
	-f head="$TAP_BRANCH" \
	-f base="main" \
	-f body="Automated tap update for $TAG" \
	--jq '.number')"
gh api -X PUT "repos/$TAP_REPO/pulls/$TAP_PR_NUMBER/merge" -f merge_method=merge >/dev/null
gh api -X DELETE "repos/$TAP_REPO/git/refs/heads/$TAP_BRANCH" >/dev/null

printf "==> Cleanup release branch\n"
git push origin --delete "$BRANCH"
git branch -D "$BRANCH"

if [[ "$NO_BREW" -eq 0 ]]; then
	printf "==> Verify local brew installation\n"
	brew update
	brew reinstall loopmux || true
	brew link --overwrite loopmux || true
	brew cleanup loopmux || true
	loopmux --version
	brew info loopmux
	brew list --versions loopmux
fi

printf "Release automation complete: %s\n" "$TAG"
