#!/usr/bin/env bash
# dev-deps-shim.sh — build a USER-LOCAL pkg-config shim for GTK3/WebKitGTK dev metadata.
#
# WHY THIS EXISTS
#   `tauri` on Linux links against gtk3 + webkit2gtk-4.1. Distributions ship the
#   *runtime* shared objects (libgtk-3.so.0, libwebkit2gtk-4.1.so.0, ...) but the
#   `-dev` packages that provide the `.pc` files and the unversioned `libX.so`
#   linker symlinks are often absent on minimal/CI images.
#
#   The correct production fix is the distro packages (see
#   `scripts/install-system-deps.sh`). This shim is a sandbox fallback that lets a
#   build proceed WITHOUT root when only the `.pc` metadata is missing.
#
# USAGE
#   source scripts/dev-deps-shim.sh        # exports PKG_CONFIG_PATH / LIBRARY_PATH
#   cargo build -p port-forward
#
# It touches nothing outside the output directory (default: ./.dev-shim).

set -euo pipefail

SHIM_DIR="${DEV_SHIM_DIR:-$(pwd)/.dev-shim}"
LIBDIR="/usr/lib/x86_64-linux-gnu"
OUT="$SHIM_DIR/lib/pkgconfig"
mkdir -p "$OUT" "$SHIM_DIR/lib"

# name -> "version|libname;libname"
declare -A SPECS=(
  [cairo]="1.18.0|cairo"
  [cairo-gobject]="1.18.0|cairo-gobject"
  [atk]="2.52.0|atk-1.0"
  [pango]="1.52.0|pango-1.0"
  [pangocairo]="1.52.0|pangocairo-1.0"
  [gdk-pixbuf-2.0]="2.42.10|gdk_pixbuf-2.0"
  [harfbuzz]="8.3.0|harfbuzz"
  [epoxy]="1.5.10|epoxy"
  [gdk-3.0]="3.24.41|gdk-3"
  [gdk-x11-3.0]="3.24.41|gdk-3"
  [gdk-wayland-3.0]="3.24.41|gdk-3"
  [gtk+-3.0]="3.24.41|gtk-3"
  [javascriptcoregtk-4.1]="2.48.0|javascriptcoregtk-4.1"
  [soup-3.0]="3.4.4|soup-3.0"
  [libsoup-3.0]="3.4.4|soup-3.0"
  [webkit2gtk-4.1]="2.48.0|webkit2gtk-4.1"
)

mkdir -p "$OUT"
for name in "${!SPECS[@]}"; do
  IFS='|' read -r version libnames <<<"${SPECS[$name]}"
  libs=""
  for l in ${libnames//;/ }; do
    # unversioned linker symlink for `-l<name>`
    real=$(ls "$LIBDIR/lib${l}.so".* 2>/dev/null | head -1 || true)
    if [[ -n "$real" ]]; then
      ln -sf "$real" "$SHIM_DIR/lib/lib${l}.so"
    fi
    libs="$libs -l$l"
  done
  cat >"$OUT/${name}.pc" <<PC
prefix=$SHIM_DIR
libdir=$SHIM_DIR/lib
Name: $name
Description: user-local pkg-config shim (runtime libs present, -dev metadata missing)
Version: $version
Libs: -L\${libdir}$libs
Cflags:
PC
done

export PKG_CONFIG_PATH="$OUT:${PKG_CONFIG_PATH:-}"
export LIBRARY_PATH="$SHIM_DIR/lib:${LIBRARY_PATH:-}"

echo "dev-deps-shim: wrote $(ls "$OUT" | wc -l) .pc files to $OUT"
echo "dev-deps-shim: PKG_CONFIG_PATH=$PKG_CONFIG_PATH"
