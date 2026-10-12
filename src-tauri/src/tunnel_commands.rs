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

/// The complete Tauri command surface for the tunnel lifecycle.
///
/// Kept in one place so the real app (`lib::run`) and the headless command
/// tests register the exact same handlers.
pub fn command_handler<R: tauri::Runtime>(
) -> impl Fn(tauri::ipc::Invoke<R>) -> bool + Send + Sync + 'static {
    tauri::generate_handler![start_tunnel, stop_tunnel, tunnel_status]
}

/// Candidate app-data directories, most specific first.
///
/// Tauri's own `app_data_dir` is authoritative, but a binary installed by
/// `scripts/install-cloudflared.sh` lands in the crate's documented fallback
/// directory. Search both so vendoring works regardless of which one wins.
fn app_data_dirs<R: tauri::Runtime>(app: &tauri::AppHandle<R>) -> Vec<std::path::PathBuf> {
    let mut dirs = Vec::new();
    if let Ok(dir) = app.path().app_data_dir() {
        dirs.push(dir);
    }
    if let Some(dir) = TunnelManager::default_app_data_dir() {
        if !dirs.contains(&dir) {
            dirs.push(dir);
        }
    }
    dirs
}

/// Resolve cloudflared from the app-data candidates, then from `PATH`.
fn resolve_cloudflared<R: tauri::Runtime>(
    app: &tauri::AppHandle<R>,
    mgr: &mut TunnelManager,
) -> Result<std::path::PathBuf, TunnelCommandError> {
    let dirs = app_data_dirs(app);
    let mut last: Option<TunnelError> = None;
    for dir in dirs.iter().map(|d| Some(d.as_path())).chain(std::iter::once(None)) {
        match mgr.resolve_binary(dir) {
            Ok(loc) => return Ok(loc.path.clone()),
            Err(err) => last = Some(err),
        }
    }
    Err(last.unwrap_or(TunnelError::BinaryNotFound { app_data_dir: None }).into())
}

/// `start_tunnel(local_port)` — spawn cloudflared and return its public URL.
///
/// Generic over the runtime so the command can also be exercised headlessly
/// through `tauri::test`'s mock IPC (see `tests/tauri_commands.rs`).
#[tauri::command]
pub async fn start_tunnel<R: tauri::Runtime>(
    app: tauri::AppHandle<R>,
    state: State<'_, TunnelState>,
    local_port: u16,
) -> Result<TunnelInfo, TunnelCommandError> {
    let mut mgr = state.0.lock().expect("tunnel mutex poisoned");
    let binary = resolve_cloudflared(&app, &mut mgr)?;
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
