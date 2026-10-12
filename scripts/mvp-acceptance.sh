#!/usr/bin/env bash
#
# MAN-15 — one-command end-to-end MVP acceptance run.
#
# Cold start (nothing running) to evidence, headless:
#
#   1. boot the tiny test/echo server on localhost:3000 (scripts/test-server.mjs)
#   2. run the cross-stack acceptance harness  (--example mvp_e2e):
#        curl -> public HTTPS URL (cloudflared) -> interceptor proxy -> localhost:3000
#        then replay directly to localhost:3000 (no tunnel)
#   3. run the UI evidence harness (scripts/ui-evidence.sh) over the *real*
#      captured payloads and measure the event -> render latency against the
#      200 ms budget
#
# Usage:
#   scripts/mvp-acceptance.sh [outDir] [--target-port N] [--no-ui]
#
# Env:
#   CLOUDFLARED_BIN   path to cloudflared (default: app-data dir or PATH)
#   VITE_PORT, CDP_PORT  forwarded to ui-evidence.sh
#
set -euo pipefail

cd "$(dirname "$0")/.."
REPO="$(pwd)"
export PATH="$HOME/.cargo/bin:$PATH"

OUT_DIR="evidence/mvp"
UI_OUT_DIR="evidence/out"
TARGET_PORT="${TARGET_PORT:-3000}"
RUN_UI=1
while [[ $# -gt 0 ]]; do
  case "$1" in
    --target-port) TARGET_PORT="$2"; shift 2 ;;
    --no-ui) RUN_UI=0; shift ;;
    --*) echo "unknown flag: $1" >&2; exit 2 ;;
    *) OUT_DIR="$1"; shift ;;
  esac
done
mkdir -p "$OUT_DIR"

CLOUDFLARED_BIN="${CLOUDFLARED_BIN:-}"
if [[ -z "$CLOUDFLARED_BIN" ]]; then
  for candidate in \
    "$HOME/.local/share/com.paperclip.portforward/cloudflared" \
    "$(command -v cloudflared 2>/dev/null || true)"; do
    [[ -x "$candidate" ]] && CLOUDFLARED_BIN="$candidate" && break
  done
fi
if [[ -z "$CLOUDFLARED_BIN" ]]; then
  echo "error: cloudflared not found; run scripts/install-cloudflared.sh or set CLOUDFLARED_BIN" >&2
  exit 1
fi

LOCAL_LOG="$OUT_DIR/local-server.log"
SERVER_PID=""
cleanup() {
  if [[ -n "$SERVER_PID" ]] && kill -0 "$SERVER_PID" 2>/dev/null; then
    kill "$SERVER_PID" 2>/dev/null || true
    wait "$SERVER_PID" 2>/dev/null || true
  fi
}
trap cleanup EXIT

echo "############ MAN-15 cold start — $(date -u +%Y-%m-%dT%H:%M:%SZ) ############"
echo "repo          : $REPO"
echo "cloudflared   : $CLOUDFLARED_BIN"
echo "target port   : $TARGET_PORT"
echo "evidence dir  : $OUT_DIR"
echo

echo "==> [cold start] no port-forward processes before we begin"
(pgrep -af 'cloudflared|test-server.mjs' || echo "    (none)") | sed 's/^/    /'

echo
echo "==> [1/3] starting the local test server on 127.0.0.1:${TARGET_PORT}"
node scripts/test-server.mjs --port "$TARGET_PORT" --log "$LOCAL_LOG" >"$OUT_DIR/test-server.stdout.log" 2>&1 &
SERVER_PID=$!
for _ in $(seq 1 40); do
  curl -sf "http://127.0.0.1:${TARGET_PORT}/health" >/dev/null 2>&1 && break
  sleep 0.25
done
curl -sf "http://127.0.0.1:${TARGET_PORT}/health" >/dev/null || {
  echo "error: test server did not become healthy; see $OUT_DIR/test-server.stdout.log" >&2
  exit 1
}
echo "    test server READY (pid $SERVER_PID, log $LOCAL_LOG)"
head -n 1 "$OUT_DIR/test-server.stdout.log" | sed 's/^/    /'

echo
echo "==> [2/3] running the cross-stack acceptance harness (curl -> tunnel -> proxy -> local)"
TARGET_PORT="$TARGET_PORT" \
LOCAL_SERVER_LOG="$LOCAL_LOG" \
CLOUDFLARED_BIN="$CLOUDFLARED_BIN" \
  cargo run -p interceptor-core --example mvp_e2e -- "$OUT_DIR" | tee "$OUT_DIR/acceptance-harness.log"
HARNESS_STATUS="${PIPESTATUS[0]}"

UI_STATUS="skipped"
if [[ "$RUN_UI" -eq 1 ]]; then
  echo
  echo "==> [3/3] running the UI evidence harness over the real captures"
  # The Vite dev server serves the repo root, so the harness can fetch the
  # captures the acceptance run just wrote.
  MVP_CAPTURES="/${OUT_DIR#./}/captured-requests.json" \
    bash scripts/ui-evidence.sh "$UI_OUT_DIR" | tee "$UI_OUT_DIR/ui-evidence.log"
  UI_STATUS="${PIPESTATUS[0]}"
  UI_STATUS=$([[ "$UI_STATUS" -eq 0 ]] && echo "ok" || echo "failed($UI_STATUS)")
fi

echo
echo "############ summary ############"
echo "harness exit  : $HARNESS_STATUS"
echo "ui exit       : $UI_STATUS"
echo "artifacts     :"
for f in "$OUT_DIR/summary.json" "$OUT_DIR/captured-requests.json" "$OUT_DIR/curl-transcript.txt" \
         "$OUT_DIR/replay-output.json" "$OUT_DIR/webhook-events.jsonl" "$OUT_DIR/local-server.log" \
         "$UI_OUT_DIR/summary.json" "$UI_OUT_DIR/07-real-capture-latency.png" "$UI_OUT_DIR/06-real-capture-replay.png"; do
  [[ -f "$f" ]] && printf "  %-52s %s bytes\n" "$f" "$(stat -c%s "$f")"
done

[[ "$HARNESS_STATUS" -eq 0 ]] || exit "$HARNESS_STATUS"
