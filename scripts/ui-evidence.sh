#!/usr/bin/env bash
#
# Headless UI evidence run (MAN-14).
#
# Boots the Vite dev server and a headless Chrome with the DevTools Protocol,
# then drives `scripts/ui-evidence.mjs` to capture screenshots + the
# event→render latency measurement into `evidence/out/`.
#
# Usage: scripts/ui-evidence.sh [outDir]
set -euo pipefail

cd "$(dirname "$0")/.."
OUT_DIR="${1:-evidence/out}"
VITE_PORT="${VITE_PORT:-1420}"
CDP_PORT="${CDP_PORT:-9222}"
CHROME_BIN="${CHROME_BIN:-$(command -v google-chrome || command -v chromium || command -v chromium-browser)}"

if [[ -z "${CHROME_BIN}" ]]; then
  echo "error: no Chrome/Chromium found (set CHROME_BIN)" >&2
  exit 1
fi

PROFILE="$(mktemp -d)"
cleanup() {
  [[ -n "${CHROME_PID:-}" ]] && kill "${CHROME_PID}" 2>/dev/null || true
  [[ -n "${VITE_PID:-}" ]] && kill "${VITE_PID}" 2>/dev/null || true
  wait "${CHROME_PID:-}" 2>/dev/null || true
  wait "${VITE_PID:-}" 2>/dev/null || true
  rm -rf "${PROFILE}" 2>/dev/null || true
}
trap cleanup EXIT

echo "==> starting vite dev server on :${VITE_PORT}"
npx vite --port "${VITE_PORT}" --strictPort >/tmp/pf-vite.log 2>&1 &
VITE_PID=$!

for _ in $(seq 1 40); do
  curl -sf "http://localhost:${VITE_PORT}/evidence/" >/dev/null && break
  sleep 0.25
done

echo "==> starting headless chrome (cdp :${CDP_PORT})"
"${CHROME_BIN}" --headless=new --no-sandbox --disable-gpu --disable-dev-shm-usage \
  --remote-debugging-port="${CDP_PORT}" --user-data-dir="${PROFILE}" about:blank \
  >/tmp/pf-chrome.log 2>&1 &
CHROME_PID=$!

for _ in $(seq 1 40); do
  curl -sf "http://127.0.0.1:${CDP_PORT}/json/version" >/dev/null && break
  sleep 0.25
done

echo "==> capturing evidence into ${OUT_DIR}"
node scripts/ui-evidence.mjs "${OUT_DIR}" "http://localhost:${VITE_PORT}/evidence/" "http://127.0.0.1:${CDP_PORT}"
