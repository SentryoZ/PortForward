# Port Forward

A lightweight desktop app that exposes a local port through a **cloudflared quick
tunnel**, intercepts the incoming webhooks, and lets you replay them. Tauri v2
(Rust core + React/TypeScript frontend + Tailwind CSS).

This repository is **MAN-11 (MVP step 1)** — the app shell plus the cloudflared
tunnel lifecycle — and **MAN-12 (MVP step 2)** — the local request interceptor
proxy, capture persistence and `webhook-received` events. Replay (MAN-13) and
the full request-list/detail UI (MAN-14) build on top of it.

## Layout

```
port-forward/
├── crates/tunnel-core/          # Pure-Rust cloudflared lifecycle (no GUI deps → unit-testable)
│   ├── src/lib.rs               # TunnelManager, typed TunnelError, URL parsing
│   ├── examples/lifecycle.rs    # End-to-end evidence harness (start → url → stop)
│   └── tests/tunnel_e2e.rs      # Gated integration test against the real binary
├── crates/interceptor-core/     # Local proxy + capture store (no GUI deps)
│   ├── src/proxy.rs             # axum proxy: forward, capture, persist, emit
│   ├── src/store.rs             # SQLite RequestStore (TEXT for UTF-8, BLOB for binary)
│   ├── src/schema.rs            # MAN-10 §3 CapturedRequest types + wire projection
│   ├── migrations/0001_init.sql # canonical `requests` schema (shared with tauri-plugin-sql)
│   ├── examples/intercept_curl.rs # curl-based evidence harness (all DoD checks)
│   └── tests/interceptor_e2e.rs # proxy ↔ store ↔ emit integration test
├── src-tauri/                   # Tauri v2 app (thin wrapper over the two crates)
│   ├── src/lib.rs               # Builder: plugins, managed state, command registration
│   ├── src/main.rs              # Binary entry point
│   ├── src/tunnel_commands.rs   # #[tauri::command] start_tunnel / stop_tunnel / tunnel_status
│   ├── src/interceptor_commands.rs # #[tauri::command] start_interceptor / stop_interceptor / …
│   ├── capabilities/default.json
│   └── tauri.conf.json
├── src/                         # React + TypeScript + Tailwind frontend
└── scripts/                     # install-system-deps.sh, install-cloudflared.sh, dev-deps-shim.sh
```

## Prerequisites

- Rust **1.77+** (Tauri v2 MSRV; developed here against 1.99).
  ```bash
  curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y
  ```
- Node 18+ and npm.
- **Linux GUI dev packages** (gtk3 + webkit2gtk-4.1 **dev**):
  ```bash
  ./scripts/install-system-deps.sh
  ```
  > No root? See *Headless / deps-less environments* below.
- `cloudflared` on `PATH`, or vendored into the app-data dir:
  ```bash
  ./scripts/install-cloudflared.sh
  ```

## Build & run

```bash
npm install                       # frontend deps
./scripts/install-cloudflared.sh  # once, if cloudflared is not on PATH

npm run tauri dev                 # dev: hot reload, opens the Tauri window
npm run tauri build               # production bundle

npm run build                     # frontend only (tsc --noEmit && vite build → dist/)
```

## Verify the tunnel lifecycle (no GUI required)

`tunnel-core` is deliberately GUI-free, so the process lifecycle can be proven in
a headless environment:

```bash
cargo test -p tunnel-core                                   # 5 unit tests
cargo test -p port-forward --test tauri_commands            # command contract (headless, no network)
CLOUDFLARED_BIN=$(command -v cloudflared) \
  cargo run -p tunnel-core --example lifecycle 3000         # live URL + no orphan
TUNNEL_E2E=1 CLOUDFLARED_BIN=$(command -v cloudflared) \
  cargo test -p tunnel-core -- --ignored                    # real binary e2e (library)
TUNNEL_E2E=1 cargo test -p port-forward --test tauri_commands -- --ignored   # real binary e2e (commands)
```

The example prints `pgrep -x cloudflared` **before / during / after**
`stop_tunnel()` plus the public URL — the evidence asked for in MAN-11.

`src-tauri/tests/tauri_commands.rs` drives the **registered `#[tauri::command]`
handlers** through Tauri's own IPC dispatcher on a mock runtime, so the
`start_tunnel` → `TunnelInfo` → `stop_tunnel` contract (and the typed
`binary_not_found` / `not_running` errors) is verified without a window server.

## Verify the interceptor (MAN-12, no GUI required)

`interceptor-core` is GUI-free too, so the whole forward → capture → persist →
emit path is provable headlessly:

```bash
cargo test -p interceptor-core                       # unit + loopback integration tests
cargo run -p interceptor-core --example intercept_curl -- /tmp/interceptor-evidence
```

`intercept_curl` starts a local origin and the real proxy, drives it with real
`curl` calls (JSON, non-JSON form, a 418 passthrough and a 0x00–0xFF binary
body), then dumps the SQLite store with `sqlite3` and prints the captured
`webhook-received` event log. It ends with `RESULT: PASS` only if every MAN-12
Definition-of-Done check holds. Artifacts: `portforward.db`,
`webhook-events.jsonl`, `binary-payload.bin`.

In the app the same core is wired through Tauri commands:

| command | purpose |
| --- | --- |
| `start_interceptor(target_port)` | bind a free `127.0.0.1:<port>` proxy → `InterceptorInfo` |
| `stop_interceptor()` | shut the proxy down |
| `interceptor_status()` | running proxy info, or `null` |
| `get_captured_request(id)` | fetch a stored record by uuid |
| `captured_request_count()` | number of stored records |

Each captured request also fires `emit("webhook-received", payload)` exactly
once, which the shell UI listens to.

## Data model

`requests` (canonical SQL in `crates/interceptor-core/migrations/0001_init.sql`,
embedded by both `interceptor-core` and the `tauri-plugin-sql` migration),
matching MAN-10 §3:

| column | notes |
| --- | --- |
| `id` | uuid-v4 |
| `timestamp` | ISO-8601 UTC |
| `method`, `path`, `body` | request line + raw payload |
| `headers`, `query_params` | JSON-encoded TEXT |
| `response_status`, `response_headers`, `response_body`, `response_latency_ms` | flattened `response{…}` |
| `response_json` | original nested response object |

Indexes: `timestamp DESC`, `method`, `path`.

`body` / `response_body` are byte-exact: UTF-8 payloads are stored as TEXT,
non-UTF-8 (binary) payloads as BLOBs. The emitted `webhook-received` payload
keeps the §3 shape — `body` is the decoded string, and for binary payloads an
extra `body_encoding: "base64"` marks the base64 encoding so nothing is lost.

## Headless / deps-less environments

Some CI/sandbox images ship the GTK/WebKit **runtime** shared objects but not the
`-dev` packages that provide `.pc` files and unversioned linker symlinks. Without
root you cannot `apt install` them. `scripts/dev-deps-shim.sh` generates a
user-local pkg-config shim pointing at the runtime libraries:

```bash
bash scripts/dev-deps-shim.sh
export PKG_CONFIG_PATH="$(pwd)/.dev-shim/lib/pkgconfig"
export LIBRARY_PATH="$(pwd)/.dev-shim/lib"
cargo build -p port-forward
```

Fallback only; prefer the real `-dev` packages in production/CI.
