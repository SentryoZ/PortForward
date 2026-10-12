#!/usr/bin/env bash
# install-cloudflared.sh — download the cloudflared binary into the app-data dir
# Port Forward searches (see `TunnelManager::default_app_data_dir`).
#
#   Linux  : $XDG_DATA_HOME/com.paperclip.portforward  (or ~/.local/share/...)
#   macOS  : ~/Library/Application Support/com.paperclip.portforward
#
# If cloudflared is already on PATH, this is unnecessary.

set -euo pipefail

OS="$(uname -s)"
ARCH="$(uname -m)"

case "$ARCH" in
  x86_64|amd64) ARCH_SLUG="amd64" ;;
  aarch64|arm64) ARCH_SLUG="arm64" ;;
  *) echo "Unsupported arch: $ARCH" >&2; exit 1 ;;
esac

case "$OS" in
  Linux)  ASSET="cloudflared-linux-${ARCH_SLUG}";  DIR="${XDG_DATA_HOME:-$HOME/.local/share}/com.paperclip.portforward" ;;
  Darwin) ASSET="cloudflared-darwin-${ARCH_SLUG}"; DIR="$HOME/Library/Application Support/com.paperclip.portforward" ;;
  *) echo "Unsupported OS: $OS (install cloudflared manually)" >&2; exit 1 ;;
esac

mkdir -p "$DIR"
URL="https://github.com/cloudflare/cloudflared/releases/latest/download/${ASSET}"
echo "Downloading $URL"
curl -fsSL -o "$DIR/cloudflared" "$URL"
chmod +x "$DIR/cloudflared"
"$DIR/cloudflared" --version
echo "Installed cloudflared at $DIR/cloudflared"
