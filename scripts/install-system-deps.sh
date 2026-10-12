#!/usr/bin/env bash
# install-system-deps.sh — the CORRECT way to install the GTK/WebKitGTK dev
# packages Tauri needs on Linux. Requires sudo/root.
#
# If you cannot get root in a sandbox, use scripts/dev-deps-shim.sh instead
# (it only supplies the missing pkg-config metadata from the runtime libs).

set -euo pipefail

if command -v apt-get >/dev/null 2>&1; then
  sudo apt-get update
  sudo apt-get install -y \
    libwebkit2gtk-4.1-dev \
    build-essential \
    curl \
    wget \
    file \
    libxdo-dev \
    libssl-dev \
    libayatana-appindicator3-dev \
    librsvg2-dev
elif command -v dnf >/dev/null 2>&1; then
  sudo dnf install -y webkit2gtk4.1-devel openssl-devel curl wget file libappindicator-gtk3-devel librsvg2-devel
  sudo dnf group install -y "C Development Tools and Libraries"
elif command -v pacman >/dev/null 2>&1; then
  sudo pacman -S --needed webkit2gtk-4.1 base-devel curl wget file openssl appmenu-gtk-module libappindicator-gtk3 librsvg
else
  echo "Unsupported package manager. See https://tauri.app/start/prerequisites/" >&2
  exit 1
fi

echo "System dev dependencies installed."
