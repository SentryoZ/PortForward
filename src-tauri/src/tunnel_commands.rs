//! Tauri commands for the cloudflared tunnel lifecycle.
//!
//! All heavy logic lives in `tunnel-core`; this module is a thin, stateful
//! `#[tauri::command]` wrapper. Errors are converted to a serializable shape so
//! the frontend receives a typed `{ "kind": ..., "message": ... }` payload
//! instead of a panic or an opaque string.

use std::sync::Mutex;
use std::time::Duration;

use serde::Serialize;
use tauri::{Manager, State};
use tunnel_core::{TunnelError, TunnelInfo, TunnelManager};

/// Serializable error returned to the frontend for every tunnel failure mode.
#[derive(Debug, Serialize)]
pub struct TunnelCommandError {
    /// Stable machine-readable discriminant (e.g. `binary_not_found`).
    pub kind: String,
    /// Human-readable message.
    pub message: String,
}

impl From<TunnelError> for TunnelCommandError {
    fn from(err: TunnelError) -> Self {
        let kind = match err {
            TunnelError::BinaryNotFound { .. } => "binary_not_found",
            TunnelError::SpawnFailed { .. } => "spawn_failed",
            TunnelError::UrlNotDetected { .. } => "url_not_detected",
            TunnelError::AlreadyRunning { .. } => "already_running",
            TunnelError::NotRunning => "not_running",
            TunnelError::StopFailed { .. } => "stop_failed",
            TunnelError::Regex(_) => "internal",
        };
        TunnelCommandError {
            kind: kind.to_string(),
            message: err.to_string(),
        }
    }
}

/// Managed state holding the single active tunnel.
pub struct TunnelState(pub Mutex<TunnelManager>);

impl Default for TunnelState {
    fn default() -> Self {
        Self(Mutex::new(TunnelManager::new()))
    }
}

/// Resolve the app-data directory Tauri provides (where a vendored
/// `cloudflared` may live), falling back to the crate default.
fn app_data_dir(app: &tauri::AppHandle) -> Option<std::path::PathBuf> {
    app.path()
        .app_data_dir()
        .ok()
        .or_else(TunnelManager::default_app_data_dir)
}

/// `start_tunnel(local_port)` — spawn cloudflared and return its public URL.
#[tauri::command]
pub async fn start_tunnel(
    app: tauri::AppHandle,
    state: State<'_, TunnelState>,
    local_port: u16,
) -> Result<TunnelInfo, TunnelCommandError> {
    let dir = app_data_dir(&app);
    let mut mgr = state.0.lock().expect("tunnel mutex poisoned");
    let binary = mgr.resolve_binary(dir.as_deref())?.path.clone();
    mgr.start(&binary, local_port, Duration::from_secs(45))
        .map_err(Into::into)
}

/// `stop_tunnel()` — terminate and reap the running cloudflared child.
#[tauri::command]
pub async fn stop_tunnel(state: State<'_, TunnelState>) -> Result<(), TunnelCommandError> {
    let mut mgr = state.0.lock().expect("tunnel mutex poisoned");
    mgr.stop().map_err(Into::into)
}

/// `tunnel_status()` — the current tunnel info, or `null` when none is running.
#[tauri::command]
pub async fn tunnel_status(state: State<'_, TunnelState>) -> Result<Option<TunnelInfo>, TunnelCommandError> {
    let mut mgr = state.0.lock().expect("tunnel mutex poisoned");
    Ok(if mgr.is_running() {
        mgr.info().cloned()
    } else {
        None
    })
}
