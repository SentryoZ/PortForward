# Port Forward

A lightweight desktop app that exposes a local port through a **cloudflared quick
tunnel**, intercepts the incoming webhooks, and lets you replay them. Tauri v2
(Rust core + React/TypeScript frontend + Tailwind CSS).

This repository is **MAN-11 (MVP step 1)**: the app shell plus the cloudflared
tunnel lifecycle. Proxy/interceptor (MAN-12), replay (MAN-13) and full UI
(MAN-14) build on top of it.

## Layout

```
port-forward/
├── crates/tunnel-core/          # Pure-Rust cloudflared lifecycle (no GUI deps → unit-testable)
│   ├── src/lib.rs               # TunnelManager, typed TunnelError, URL parsing
│   ├── examples/lifecycle.rs    # End-to-end evidence harness (start → url → stop)
│   └── tests/tunnel_e2e.rs      # Gated integration test against the real binary
├── src-tauri/                   # Tauri v2 app (thin wrapper over tunnel-core)
│   ├── src/lib.rs               # Builder: plugins, managed state, command registration
│   ├── src/main.rs              # Binary entry point
│   ├── src/tunnel_commands.rs   # #[tauri::command] start_tunnel / stop_tunnel / tunnel_status
│   ├── migrations/0001_init.sql # `requests` table (MAN-10 §3 schema)
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
CLOUDFLARED_BIN=$(command -v cloudflared) \
  cargo run -p tunnel-core --example lifecycle 3000         # live URL + no orphan
TUNNEL_E2E=1 CLOUDFLARED_BIN=$(command -v cloudflared) \
  cargo test -p tunnel-core -- --ignored                    # real binary e2e
```

The example prints `pgrep -x cloudflared` **before / during / after**
`stop_tunnel()` plus the public URL — the evidence asked for in MAN-11.

## Data model

`requests` (see `src-tauri/migrations/0001_init.sql`), matching MAN-10 §3:

| column | notes |
| --- | --- |
| `id` | uuid-v4 |
| `timestamp` | ISO-8601 UTC |
| `method`, `path`, `body` | request line + raw payload |
| `headers`, `query_params` | JSON-encoded TEXT |
| `response_status`, `response_headers`, `response_body`, `response_latency_ms` | flattened `response{…}` |
| `response_json` | original nested response object |

Indexes: `timestamp DESC`, `method`, `path`.

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
