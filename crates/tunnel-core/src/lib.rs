//! `tunnel-core` — cloudflared quick-tunnel lifecycle for Port Forward.
//!
//! This crate deliberately has **no Tauri / GUI dependencies** so the process
//! lifecycle can be unit- and integration-tested in a headless CI sandbox where
//! webkit2gtk dev headers are unavailable. The Tauri layer (`src-tauri`) is a
//! thin `#[tauri::command]` wrapper around [`TunnelManager`].
//!
//! ## Responsibilities
//! - Locate the `cloudflared` binary on `PATH` or in a caller-supplied app-data
//!   directory, returning a typed [`TunnelError::BinaryNotFound`] when absent.
//! - `start` a quick tunnel (`cloudflared tunnel --url http://127.0.0.1:<port>`),
//!   parse the assigned `https://<id>.trycloudflare.com` URL from stdout/stderr.
//! - `stop` the child process and reap it so no orphan is left behind.

use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

use regex::Regex;
use thiserror::Error;

/// Default time we wait for cloudflared to print its public URL.
pub const DEFAULT_URL_TIMEOUT: Duration = Duration::from_secs(30);

/// Regex cloudflared uses for quick-tunnel hostnames.
pub const PUBLIC_URL_PATTERN: &str = r"https://[a-zA-Z0-9-]+\.trycloudflare\.com";

/// Typed errors for every failure mode of the tunnel lifecycle.
///
/// The crate never panics on a missing binary or a failed spawn: callers always
/// receive a `Result`.
#[derive(Debug, Error)]
pub enum TunnelError {
    /// `cloudflared` could not be found on PATH or in the app-data directory.
    #[error("cloudflared binary not found (searched PATH and {app_data_dir:?}); install it or drop the binary in the app-data directory")]
    BinaryNotFound { app_data_dir: Option<PathBuf> },

    /// The OS refused to spawn the process.
    #[error("failed to spawn cloudflared at {path:?}: {source}")]
    SpawnFailed {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    /// cloudflared started but never printed a public URL before the timeout.
    #[error("cloudflared did not report a public URL within {timeout:?} (last output: {last_output:?})")]
    UrlNotDetected {
        timeout: Duration,
        last_output: String,
    },

    /// A tunnel is already running for this manager.
    #[error("a tunnel is already running (pid {pid}, url {public_url})")]
    AlreadyRunning { pid: u32, public_url: String },

    /// `stop` was called when no tunnel was running.
    #[error("no tunnel is currently running")]
    NotRunning,

    /// The child process could not be killed or reaped.
    #[error("failed to stop tunnel (pid {pid}): {source}")]
    StopFailed {
        pid: u32,
        #[source]
        source: std::io::Error,
    },

    /// Building the URL regex failed (should be unreachable).
    #[error("internal regex error: {0}")]
    Regex(String),
}

/// Public information about a live tunnel.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct TunnelInfo {
    /// The assigned `https://<id>.trycloudflare.com` hostname.
    pub public_url: String,
    /// OS process id of the spawned cloudflared child.
    pub pid: u32,
    /// The local port being forwarded.
    pub local_port: u16,
}

/// Where a `cloudflared` binary was found, for diagnostics.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BinaryLocation {
    pub path: PathBuf,
    pub source: BinarySource,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BinarySource {
    /// Found via a `PATH` entry.
    Path,
    /// Found in the supplied app-data directory.
    AppData,
}

const BINARY_NAME: &str = if cfg!(windows) {
    "cloudflared.exe"
} else {
    "cloudflared"
};

/// Locate `cloudflared` on `PATH` or inside `app_data_dir`.
///
/// Search order:
/// 1. `<app_data_dir>/cloudflared` (so an app can ship/vendor the binary)
/// 2. every directory in `PATH`
pub fn find_cloudflared(app_data_dir: Option<&Path>) -> Result<BinaryLocation, TunnelError> {
    let path_env = std::env::var_os("PATH");
    find_cloudflared_in(app_data_dir, path_env.as_deref())
}

/// Testable variant of [`find_cloudflared`] that takes an explicit `PATH` value.
pub fn find_cloudflared_in(
    app_data_dir: Option<&Path>,
    path_env: Option<&std::ffi::OsStr>,
) -> Result<BinaryLocation, TunnelError> {
    if let Some(dir) = app_data_dir {
        let candidate = dir.join(BINARY_NAME);
        if is_executable_file(&candidate) {
            return Ok(BinaryLocation {
                path: candidate,
                source: BinarySource::AppData,
            });
        }
    }

    if let Some(path_env) = path_env {
        for dir in std::env::split_paths(path_env) {
            let candidate = dir.join(BINARY_NAME);
            if is_executable_file(&candidate) {
                return Ok(BinaryLocation {
                    path: candidate,
                    source: BinarySource::Path,
                });
            }
        }
    }

    Err(TunnelError::BinaryNotFound {
        app_data_dir: app_data_dir.map(Path::to_path_buf),
    })
}

fn is_executable_file(path: &Path) -> bool {
    let Ok(meta) = std::fs::metadata(path) else {
        return false;
    };
    if !meta.is_file() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        meta.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    {
        true
    }
}

/// Extract the first `https://<id>.trycloudflare.com` URL from a blob of text.
pub fn parse_public_url(text: &str) -> Option<String> {
    let re = Regex::new(PUBLIC_URL_PATTERN).expect("static regex is valid");
    re.find(text).map(|m| m.as_str().to_string())
}

/// Manages at most one `cloudflared` child process at a time.
#[derive(Default)]
pub struct TunnelManager {
    child: Option<Child>,
    info: Option<TunnelInfo>,
    binary: Option<BinaryLocation>,
}

impl TunnelManager {
    pub fn new() -> Self {
        Self::default()
    }

    /// The app-data directory the manager searches for a vendored binary.
    pub fn default_app_data_dir() -> Option<PathBuf> {
        // Mirrors Tauri's `app_data_dir` for the `com.paperclip.portforward`
        // identifier on Linux (`$XDG_DATA_HOME/<identifier>`), and keeps a
        // generic fallback for other platforms.
        if let Some(xdg) = std::env::var_os("XDG_DATA_HOME") {
            return Some(PathBuf::from(xdg).join("com.paperclip.portforward"));
        }
        if let Some(home) = std::env::var_os("HOME") {
            return Some(PathBuf::from(home).join(".local/share/com.paperclip.portforward"));
        }
        None
    }

    /// Resolve (and cache) the cloudflared binary location.
    pub fn resolve_binary(&mut self, app_data_dir: Option<&Path>) -> Result<&BinaryLocation, TunnelError> {
        if self.binary.is_none() {
            self.binary = Some(find_cloudflared(app_data_dir)?);
        }
        Ok(self.binary.as_ref().expect("just set"))
    }

    /// True when a child is currently tracked (may already have exited).
    pub fn is_running(&mut self) -> bool {
        match self.child.as_mut() {
            Some(child) => matches!(child.try_wait(), Ok(None)),
            None => false,
        }
    }

    /// Current tunnel info, if a tunnel is tracked.
    pub fn info(&self) -> Option<&TunnelInfo> {
        self.info.as_ref()
    }

    /// Spawn `cloudflared` for `local_port` and wait for its public URL.
    ///
    /// `public_url_override` is a test seam: when set, the URL is not read from
    /// the child's output (used by the process-lifecycle test on slow CI).
    pub fn start(
        &mut self,
        binary: &Path,
        local_port: u16,
        url_timeout: Duration,
    ) -> Result<TunnelInfo, TunnelError> {
        if self.is_running() {
            let info = self.info.as_ref().expect("running implies info");
            return Err(TunnelError::AlreadyRunning {
                pid: info.pid,
                public_url: info.public_url.clone(),
            });
        }

        let mut child = Command::new(binary)
            .arg("tunnel")
            .arg("--no-autoupdate")
            .arg("--url")
            .arg(format!("http://127.0.0.1:{local_port}"))
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|source| TunnelError::SpawnFailed {
                path: binary.to_path_buf(),
                source,
            })?;

        let pid = child.id();
        let stdout = child.stdout.take();
        let stderr = child.stderr.take();

        let (tx, rx) = mpsc::channel::<String>();
        let collectors = [
            stdout.map(|s| spawn_url_scanner(s, tx.clone())),
            stderr.map(|s| spawn_url_scanner(s, tx.clone())),
        ];
        drop(tx);

        let deadline = Instant::now() + url_timeout;
        let mut last_output = String::new();
        let url = loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                break None;
            }
            match rx.recv_timeout(remaining.min(Duration::from_millis(500))) {
                Ok(line) => {
                    if let Some(url) = parse_public_url(&line) {
                        break Some(url);
                    }
                    last_output = line;
                }
                Err(mpsc::RecvTimeoutError::Timeout) => {
                    // If the child died early, stop waiting.
                    if let Ok(Some(status)) = child.try_wait() {
                        let _ = status;
                        break None;
                    }
                }
                Err(mpsc::RecvTimeoutError::Disconnected) => break None,
            }
        };

        // Keep the scanner threads detached but alive to drain the pipes so the
        // child never blocks on a full pipe buffer.
        for handle in collectors.into_iter().flatten() {
            thread::spawn(move || {
                let _ = handle.join();
            });
        }

        match url {
            Some(public_url) => {
                let info = TunnelInfo {
                    public_url,
                    pid,
                    local_port,
                };
                self.child = Some(child);
                self.info = Some(info.clone());
                Ok(info)
            }
            None => {
                let _ = child.kill();
                let _ = child.wait();
                Err(TunnelError::UrlNotDetected {
                    timeout: url_timeout,
                    last_output,
                })
            }
        }
    }

    /// Kill and reap the child process. Idempotent-safe: errors if nothing runs.
    pub fn stop(&mut self) -> Result<(), TunnelError> {
        let mut child = self.child.take().ok_or(TunnelError::NotRunning)?;
        let pid = child.id();
        let kill_result = child.kill();
        let wait_result = child.wait();
        self.info = None;

        // `kill` on an already-exited process returns InvalidInput on unix; that
        // is fine as long as `wait` reaped it.
        if let Err(source) = wait_result {
            return Err(TunnelError::StopFailed { pid, source });
        }
        if let Err(source) = kill_result {
            if source.kind() != std::io::ErrorKind::InvalidInput {
                return Err(TunnelError::StopFailed { pid, source });
            }
        }
        Ok(())
    }
}

impl Drop for TunnelManager {
    fn drop(&mut self) {
        // Best-effort cleanup so a dropped manager never orphans cloudflared.
        if let Some(mut child) = self.child.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

fn spawn_url_scanner<R: std::io::Read + Send + 'static>(
    reader: R,
    tx: mpsc::Sender<String>,
) -> thread::JoinHandle<()> {
    thread::spawn(move || {
        let buf = BufReader::new(reader);
        for line in buf.lines() {
            match line {
                Ok(line) => {
                    if tx.send(line).is_err() {
                        // Receiver gone; keep draining silently.
                    }
                }
                Err(_) => break,
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_stderr_style_url_line() {
        let line = "2026-10-12T03:00:00Z INF |  https://brave-river-1234.trycloudflare.com    |";
        assert_eq!(
            parse_public_url(line).as_deref(),
            Some("https://brave-river-1234.trycloudflare.com")
        );
    }

    #[test]
    fn ignores_non_quick_tunnel_urls() {
        assert_eq!(parse_public_url("https://example.com/foo"), None);
        assert_eq!(parse_public_url("no url here"), None);
    }

    #[test]
    fn missing_binary_returns_typed_error_not_panic() {
        let tmp = tempfile::tempdir().unwrap();
        let err = find_cloudflared_in(Some(tmp.path()), None).unwrap_err();
        assert!(matches!(err, TunnelError::BinaryNotFound { .. }));
    }

    #[test]
    fn finds_binary_in_app_data_dir() {
        let tmp = tempfile::tempdir().unwrap();
        let bin = tmp.path().join(BINARY_NAME);
        std::fs::write(&bin, b"#!/bin/sh\nexit 0\n").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        let found = find_cloudflared_in(Some(tmp.path()), None).unwrap();
        assert_eq!(found.source, BinarySource::AppData);
        assert_eq!(found.path, bin);
    }

    #[test]
    fn stop_without_start_is_typed_error() {
        let mut mgr = TunnelManager::new();
        assert!(matches!(mgr.stop(), Err(TunnelError::NotRunning)));
    }
}
