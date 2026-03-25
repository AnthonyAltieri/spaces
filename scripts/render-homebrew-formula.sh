#!/usr/bin/env bash

set -euo pipefail

if [ "$#" -ne 4 ]; then
  echo "usage: $0 <output-path> <version> <archive-url> <sha256>" >&2
  exit 1
fi

output_path="$1"
version="$2"
archive_url="$3"
sha256="$4"

mkdir -p "$(dirname "${output_path}")"

cat > "${output_path}" <<EOF
class Spaces < Formula
  desc "Create and manage coordinated multi-repo git workspaces"
  homepage "https://github.com/AnthonyAltieri/spaces"
  url "${archive_url}"
  version "${version}"
  sha256 "${sha256}"
  license "MIT"

  depends_on "rust" => :build
  uses_from_macos "git"

  def install
    system "cargo", "install", *std_cargo_args
  end

  test do
    output = shell_output("#{bin}/spaces list --base-dir #{testpath} --json")
    assert_match "\\"workspaces\\": []", output
    assert_match "\\"registry_path\\":", output
  end
end
EOF
