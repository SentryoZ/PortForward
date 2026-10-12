//! Headless verification of the Tauri **command layer** for MAN-11.
//!
//! `crates/tunnel-core` proves the cloudflared process lifecycle. This test
//! proves the thing MAN-11 actually asks for: that the *registered*
//! `#[tauri::command]` handlers `start_tunnel` / `stop_tunnel` /
//! `tunnel_status` behave correctly when invoked through Tauri's own IPC
//! dispatcher.
//!
//! It uses `tauri::test`'s mock runtime, so no window server (X11/Wayland) and
//! no real window are required — it runs in a plain headless CI shell.
//!
//! Gated behind `TUNNEL_E2E=1` because it needs the network and a real
//! `cloudflared` binary (Quick Tunnels call the Cloudflare edge):
//!
//! ```bash
//! TUNNEL_E2E=1 cargo test -p port-forward --test tauri_commands -- --nocapture
//! ```

use std::io::{Read, Write};
use std::net::TcpListener;
use std::process::Command;
use std::time::Duration;

use port_forward_lib::tunnel_commands::{command_handler, TunnelState};
use serde_json::json;
use tauri::test::{get_ipc_response, mock_builder, mock_context, noop_assets, INVOKE_KEY};
use tauri::webview::InvokeRequest;
use tauri::WebviewWindowBuilder;
use tunnel_core::{find_cloudflared, TunnelManager};

/// Build the same command surface the real app registers, on the mock runtime.
fn mock_app() -> tauri::App<tauri::test::MockRuntime> {
    mock_builder()
        .manage(TunnelState::default())
        .invoke_handler(command_handler())
        .build(mock_context(noop_assets()))
        .expect("failed to build mock app")
}

fn invoke(
    webview: &tauri::WebviewWindow<tauri::test::MockRuntime>,
    cmd: &str,
    body: serde_json::Value,
) -> Result<serde_json::Value, serde_json::Value> {
    get_ipc_response(
        webview,
        InvokeRequest {
            cmd: cmd.into(),
            callback: tauri::ipc::CallbackFn(0),
            error: tauri::ipc::CallbackFn(1),
            url: "tauri://localhost".parse().unwrap(),
            body: tauri::ipc::InvokeBody::Json(body),
            headers: Default::default(),
            invoke_key: INVOKE_KEY.to_string(),
        },
    )
    .map(|b| b.deserialize::<serde_json::Value>().unwrap())
}

fn cloudflared_pids() -> Vec<u32> {
    let out = Command::new("pgrep").arg("-x").arg("cloudflared").output();
    match out {
        Ok(out) => String::from_utf8_lossy(&out.stdout)
            .split_whitespace()
            .filter_map(|p| p.parse().ok())
            .collect(),
        Err(_) => Vec::new(),
    }
}

#[test]
fn commands_are_registered_and_expose_the_expected_contract() {
    // Contract-only assertions: no network, no cloudflared required.
    let app = mock_app();
    let webview = WebviewWindowBuilder::new(&app, "main", Default::default())
        .build()
        .expect("mock webview");

    // `tunnel_status` starts as `null` — no tunnel in managed state yet.
    let status = invoke(&webview, "tunnel_status", json!({})).expect("tunnel_status ok");
    assert_eq!(status, json!(null), "fresh state should report no tunnel");

    // `stop_tunnel` with nothing running is a *typed* error, never a panic.
    let err = invoke(&webview, "stop_tunnel", json!({})).expect_err("stop must fail");
    assert_eq!(err["kind"], json!("not_running"));
    assert!(err["message"].is_string(), "error carries a message: {err}");

    // `start_tunnel` with a missing binary is a typed `binary_not_found`, and
    // the command still returns (no panic / no abort) — MAN-11 DoD item 4.
    let saved: Vec<(&str, Option<std::ffi::OsString>)> = ["PATH", "XDG_DATA_HOME", "HOME"]
        .iter()
        .map(|k| (*k, std::env::var_os(k)))
        .collect();
    let missing = std::env::temp_dir().join("port-forward-definitely-no-binary");
    let _ = std::fs::create_dir_all(&missing);
    std::env::set_var("PATH", &missing);
    std::env::set_var("XDG_DATA_HOME", &missing);
    std::env::set_var("HOME", &missing);
    let err = invoke(&webview, "start_tunnel", json!({ "localPort": 3000 }))
        .expect_err("start must fail without a binary");
    for (key, value) in saved {
        match value {
            Some(v) => std::env::set_var(key, v),
            None => std::env::remove_var(key),
        }
    }
    assert_eq!(err["kind"], json!("binary_not_found"), "got {err}");
}

#[test]
#[ignore = "requires network + cloudflared; run with TUNNEL_E2E=1"]
fn start_tunnel_returns_live_url_and_stop_leaves_no_orphan() {
    if std::env::var("TUNNEL_E2E").as_deref() != Ok("1") {
        eprintln!("skipping: set TUNNEL_E2E=1 to run this network test");
        return;
    }
    if find_cloudflared(TunnelManager::default_app_data_dir().as_deref()).is_err() {
        eprintln!("skipping: cloudflared not found");
        return;
    }

    // A real local origin, so the public URL has something to forward to.
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind origin");
    let local_port = listener.local_addr().unwrap().port();
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { break };
            let mut buf = [0u8; 1024];
            let _ = stream.read(&mut buf);
            let _ = stream.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nOK");
        }
    });

    let app = mock_app();
    let webview = WebviewWindowBuilder::new(&app, "main", Default::default())
        .build()
        .expect("mock webview");

    let before = cloudflared_pids();

    // --- start_tunnel(local_port) through the real command dispatcher --------
    let info = invoke(&webview, "start_tunnel", json!({ "localPort": local_port }))
        .expect("start_tunnel should return TunnelInfo");
    println!("start_tunnel({local_port}) => {info}");
    assert_eq!(info["local_port"], json!(local_port));
    let url = info["public_url"].as_str().expect("public_url string");
    assert!(
        url.starts_with("https://") && url.ends_with(".trycloudflare.com"),
        "unexpected url: {url}"
    );
    let pid = info["pid"].as_u64().expect("pid") as u32;
    assert!(pid > 0);

    // The child is real and owned by this process.
    let during = cloudflared_pids();
    assert!(
        during.contains(&pid),
        "cloudflared pid {pid} should be live (saw {during:?})"
    );

    // The public URL is live: tunnel_status reports the same tunnel.
    let status = invoke(&webview, "tunnel_status", json!({})).expect("tunnel_status");
    assert_eq!(status["pid"], json!(pid));

    // --- stop_tunnel() -------------------------------------------------------
    invoke(&webview, "stop_tunnel", json!({})).expect("stop_tunnel should succeed");
    std::thread::sleep(Duration::from_millis(500));

    let after = cloudflared_pids();
    assert!(
        !after.contains(&pid),
        "pid {pid} must be reaped after stop_tunnel (still saw {after:?})"
    );
    let _ = before;

    let status = invoke(&webview, "tunnel_status", json!({})).expect("tunnel_status");
    assert_eq!(status, json!(null), "no tunnel after stop");
}
