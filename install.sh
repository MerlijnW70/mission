#!/bin/sh
# Zero-friction installer for the `mission` CLI. No Rust toolchain required.
#   curl -fsSL https://raw.githubusercontent.com/MerlijnW70/mission/main/install.sh | sh
# Override the install dir with MISSION_INSTALL_DIR=/usr/local/bin.
set -eu

REPO="MerlijnW70/mission"

os="$(uname -s)"
arch="$(uname -m)"

case "$os" in
  Linux)
    case "$arch" in
      x86_64 | amd64) target="x86_64-unknown-linux-musl" ;;   # static — runs anywhere
      aarch64 | arm64) target="aarch64-unknown-linux-gnu" ;;
      *) echo "mission: unsupported architecture '$arch'"; exit 1 ;;
    esac ;;
  Darwin)
    case "$arch" in
      x86_64 | amd64) target="x86_64-apple-darwin" ;;
      arm64 | aarch64) target="aarch64-apple-darwin" ;;
      *) echo "mission: unsupported architecture '$arch'"; exit 1 ;;
    esac ;;
  *)
    echo "mission: '$os' has no install script — download a binary from"
    echo "  https://github.com/$REPO/releases/latest"
    exit 1 ;;
esac

# Resolve the latest release tag.
tag="$(curl -fsSL "https://api.github.com/repos/$REPO/releases/latest" \
  | grep '"tag_name"' | head -1 | sed -E 's/.*"tag_name": *"([^"]+)".*/\1/')"
if [ -z "$tag" ]; then
  echo "mission: could not find a release — see https://github.com/$REPO/releases"
  exit 1
fi

asset="mission-$tag-$target.tar.gz"
url="https://github.com/$REPO/releases/download/$tag/$asset"
dir="${MISSION_INSTALL_DIR:-$HOME/.local/bin}"
mkdir -p "$dir"

tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT
echo "mission: downloading $tag ($target)…"
curl -fsSL "$url" -o "$tmp/m.tar.gz"
tar -xzf "$tmp/m.tar.gz" -C "$tmp"

for bin in mission mission-mcp; do
  [ -f "$tmp/$bin" ] || continue
  cp "$tmp/$bin" "$dir/$bin"
  chmod +x "$dir/$bin"
done

echo "mission: installed $tag to $dir/mission"
case ":$PATH:" in
  *":$dir:"*) : ;;
  *) echo "mission: add $dir to your PATH  (e.g. export PATH=\"$dir:\$PATH\")" ;;
esac
