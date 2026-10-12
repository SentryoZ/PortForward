//! Tauri commands for the request interceptor (MAN-12).
//!
//! Thin, stateful wrappers around [`interceptor_core`]. The proxy itself is
//! GUI-free; this module only:
//!
//! * resolves the SQLite file in the app-config dir (the same database
//!   `tauri-plugin-sql` exposes to the frontend);
//! * wires the capture emitter to `emit("webhook-received", payload)`, exactly
//!   once per request;
//! * exposes `start_interceptor` / `stop_interceptor` / `interceptor_status`
//!   and a `get_captured_request` lookup.
//!
//! Errors are converted to a serializable `{ "kind": ..., "message": ... }`
//! payload, matching the tunnel commands.

use std::path::PathBuf;
use std::sync::Mutex;

use interceptor_core::{
    replay, CapturedRequest, CapturedRequestJson, Interceptor, InterceptorInfo, ProxyError,
    ReplayError, RequestStore, ResponsePayload, StoreError,
};
use serde::Serialize;
use tauri::{Emitter, Manager, State};

use crate::DB_FILE_NAME;

/// Serializable error returned to the frontend for every interceptor failure.
#[derive(Debug, Serialize)]
pub struct InterceptorCommandError {
    /// Stable machine-readable discriminant (e.g. `already_running`).
    pub kind: String,
    /// Human-readable message.
    pub message: String,
}

impl InterceptorCommandError {
    fn new(kind: &str, message: impl Into<String>) -> Self {
        Self {
            kind: kind.to_string(),
            message: message.into(),
        }
    }
}

impl From<ProxyError> for InterceptorCommandError {
    fn from(err: ProxyError) -> Self {
        let kind = match err {
            ProxyError::Bind(_) => "bind_failed",
            ProxyError::Store(_) => "store_failed",
        };
        Self::new(kind, err.to_string())
    }
}

impl From<StoreError> for InterceptorCommandError {
    fn from(err: StoreError) -> Self {
        Self::new("store_failed", err.to_string())
    }
}

impl From<ReplayError> for InterceptorCommandError {
    fn from(err: ReplayError) -> Self {
        let kind = match &err {
            ReplayError::UnknownRequest(_) => "unknown_request",
            ReplayError::InvalidMethod(_) => "invalid_method",
            ReplayError::InvalidHeader(_) => "invalid_header",
            ReplayError::Request(_) => "replay_failed",
            ReplayError::Store(_) => "store_failed",
        };
        Self::new(kind, err.to_string())
    }
}

/// A running interceptor plus the store handle used for lookups.
struct Running {
    interceptor: Interceptor,
    store: RequestStore,
}

/// Managed state holding the single active interceptor.
#[derive(Default)]
pub struct InterceptorState(Mutex<Option<Running>>);

/// Resolve the SQLite store path used by both the frontend and the proxy.
fn store_path(app: &tauri::AppHandle) -> Result<PathBuf, InterceptorCommandError> {
    let dir = app.path().app_config_dir().map_err(|err| {
        InterceptorCommandError::new("no_config_dir", format!("no app config dir: {err}"))
    })?;
    Ok(dir.join(DB_FILE_NAME))
}

/// Open a store handle: reuse the running one, otherwise open the app DB.
async fn store_for(
    app: &tauri::AppHandle,
    state: &State<'_, InterceptorState>,
) -> Result<RequestStore, InterceptorCommandError> {
    if let Some(running) = state.0.lock().expect("interceptor mutex poisoned").as_ref() {
        return Ok(running.store.clone());
    }
    Ok(RequestStore::open(store_path(app)?).await?)
}

/// `start_interceptor(target_port)` — bind the proxy and begin capturing.
#[tauri::command]
pub async fn start_interceptor(
    app: tauri::AppHandle,
    state: State<'_, InterceptorState>,
    target_port: u16,
) -> Result<InterceptorInfo, InterceptorCommandError> {
    if target_port == 0 {
        return Err(InterceptorCommandError::new(
            "invalid_target",
            "target port must be between 1 and 65535",
        ));
    }

    {
        let guard = state.0.lock().expect("interceptor mutex poisoned");
        if let Some(running) = guard.as_ref() {
            return Err(InterceptorCommandError::new(
                "already_running",
                format!(
                    "interceptor already running on port {}",
                    running.interceptor.info().proxy_port
                ),
            ));
        }
    }

    let store = RequestStore::open(store_path(&app)?).await?;

    // Exactly-once live event for the UI.
    let emitter_app = app.clone();
    let emitter: interceptor_core::Emitter = std::sync::Arc::new(move |record: &CapturedRequest| {
        let payload: CapturedRequestJson = record.to_wire();
        if let Err(err) = emitter_app.emit("webhook-received", payload) {
            eprintln!("[interceptor] failed to emit webhook-received: {err}");
        }
    });

    let interceptor = Interceptor::start(target_port, store.clone(), Some(emitter)).await?;
    let info = interceptor.info().clone();

    *state.0.lock().expect("interceptor mutex poisoned") = Some(Running { interceptor, store });
    Ok(info)
}

/// `stop_interceptor()` — shut the proxy down and free its port.
#[tauri::command]
pub async fn stop_interceptor(state: State<'_, InterceptorState>) -> Result<(), InterceptorCommandError> {
    let running = state
        .0
        .lock()
        .expect("interceptor mutex poisoned")
        .take();
    if let Some(running) = running {
        running.interceptor.stop().await?;
    }
    Ok(())
}

/// `interceptor_status()` — info for the running proxy, or `null`.
#[tauri::command]
pub async fn interceptor_status(
    state: State<'_, InterceptorState>,
) -> Result<Option<InterceptorInfo>, InterceptorCommandError> {
    let guard = state.0.lock().expect("interceptor mutex poisoned");
    Ok(guard.as_ref().map(|r| r.interceptor.info().clone()))
}

/// `get_captured_request(id)` — fetch a stored record by its uuid.
#[tauri::command]
pub async fn get_captured_request(
    app: tauri::AppHandle,
    state: State<'_, InterceptorState>,
    id: String,
) -> Result<Option<CapturedRequestJson>, InterceptorCommandError> {
    let store = store_for(&app, &state).await?;
    Ok(store.get(&id).await?.map(|record| record.to_wire()))
}

/// `captured_request_count()` — number of persisted records.
#[tauri::command]
pub async fn captured_request_count(
    app: tauri::AppHandle,
    state: State<'_, InterceptorState>,
) -> Result<i64, InterceptorCommandError> {
    let store = store_for(&app, &state).await?;
    Ok(store.count().await?)
}

/// `replay_request(request_id)` — re-issue a captured request directly to the
/// local target server (MAN-13).
///
/// The request is sent straight to `http://localhost:<target_port><path>` with
/// the preserved method/headers/body, bypassing the interceptor proxy and the
/// public tunnel. The target port comes from the running interceptor; if none is
/// running the replay cannot know where to send the request and fails with the
/// typed `not_running` error. An unknown id yields `unknown_request`.
#[tauri::command]
pub async fn replay_request(
    state: State<'_, InterceptorState>,
    request_id: String,
) -> Result<ResponsePayload, InterceptorCommandError> {
    let (store, target_port) = {
        let guard = state.0.lock().expect("interceptor mutex poisoned");
        match guard.as_ref() {
            Some(running) => (
                running.store.clone(),
                running.interceptor.info().target_port,
            ),
            None => {
                return Err(InterceptorCommandError::new(
                    "not_running",
                    "start the interceptor before replaying a captured request",
                ))
            }
        }
    };

    Ok(replay(&store, &request_id, target_port).await?)
}
