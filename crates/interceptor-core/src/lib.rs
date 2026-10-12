//! `interceptor-core` — the Port Forward request interceptor.
//!
//! A GUI-free crate that owns the two pieces MAN-12 adds on top of the MAN-11
//! app shell:
//!
//! * [`proxy::Interceptor`] — a local `axum` proxy on `127.0.0.1:<free port>`
//!   that forwards requests to the user's target port, captures them and
//!   returns the upstream response unaltered;
//! * [`store::RequestStore`] — SQLite persistence for the captured records,
//!   byte-exact for both UTF-8 and binary bodies.
//!
//! [`schema`] holds the MAN-10 §3 record types. The Tauri layer is a thin
//! wrapper: it opens the store in the app-config dir, wires the emitter to
//! `emit("webhook-received", …)`, and registers `start_interceptor` /
//! `stop_interceptor` / `interceptor_status` commands.

pub mod proxy;
pub mod schema;
pub mod store;

pub use proxy::{Emitter, Interceptor, InterceptorInfo, ProxyError};
pub use schema::{
    is_iso8601_utc, is_uuid, CapturedRequest, CapturedRequestJson, CapturedResponse, ResponseJson,
};
pub use store::{RequestStore, StoreError};
