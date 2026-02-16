#!/usr/bin/env bash
set -euo pipefail

repo="dmoliveira/loopmux"
tag="${1:-$(git describe --tags --abbrev=0)}"

if [[ ! "$tag" =~ ^v[0-9]+\.[0-9]+\.[0-9]+$ ]]; then
	echo "error: tag must look like vX.Y.Z (got: $tag)" >&2
	exit 1
fi

url="https://github.com/${repo}/archive/refs/tags/${tag}.tar.gz"
sha256="$(curl -fsSL "$url" | shasum -a 256 | awk '{print $1}')"
formula_path="release/loopmux.rb"

cat >"$formula_path" <<EOF
class Loopmux < Formula
  desc "Loop prompts into tmux panes with triggers and delays"
  homepage "https://github.com/${repo}"
  url "${url}"
  sha256 "${sha256}"
  license "MIT"

  depends_on "rust" => :build

  def install
    system "cargo", "install", *std_cargo_args
  end

  test do
    assert_match "loopmux", shell_output("#{bin}/loopmux --help")
  end
end
EOF

echo "Updated ${formula_path}"
echo "- tag: ${tag}"
echo "- sha256: ${sha256}"
