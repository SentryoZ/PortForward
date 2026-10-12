//! End-to-end proof for the Port Forward tunnel lifecycle (MAN-11 evidence).
//!
//! Run explicitly (network required):
//! ```bash
//! CLOUDFLARED_BIN=/path/to/cloudflared cargo run -p tunnel-core --example lifecycle 3000
//! ```
//!
//! It prints, in order:
//!   1. `pgrep cloudflared` BEFORE start  (expected: empty)
//!   2. the `TunnelInfo` returned by `start_tunnel(3000)`
//!   3. `pgrep cloudflared` WHILE running (expected: one pid = child pid)
//!   4. `pgrep cloudflared` AFTER `stop_tunnel()` (expected: empty)
//!
//! It also exercises the "missing binary" path and prints the typed error.

use std::process::Command;
use std::time::Duration;

use tunnel_core::{find_cloudflared, TunnelError, TunnelManager};

fn pgrep_cloudflared() -> String {
    // `-x` matches the process *name* exactly, so a shell whose command line
    // merely contains the word "cloudflared" is not reported as an orphan.
    match Command::new("pgrep").arg("-x").arg("cloudflared").output() {
        Ok(out) => {
            let text = String::from_utf8_lossy(&out.stdout).trim().to_string();
            if text.is_empty() {
                "(none)".to_string()
            } else {
                text.replace('\n', ", ")
            }
        }
        Err(e) => format!("(pgrep failed: {e})"),
    }
}

fn main() {
    let port: u16 = std::env::args()
        .nth(1)
        .and_then(|p| p.parse().ok())
        .unwrap_or(3000);

    println!("== MAN-11 tunnel lifecycle evidence ==");
    println!("target local port: {port}");

    // --- 1. missing-binary path (typed error, never a panic) ------------------
    {
        let empty = std::env::temp_dir().join("port-forward-no-binary-here");
        let _ = std::fs::create_dir_all(&empty);
        match tunnel_core::find_cloudflared_in(Some(&empty), None) {
            Err(TunnelError::BinaryNotFound { .. }) => {
                println!("[ok] missing cloudflared -> typed TunnelError::BinaryNotFound (no panic)")
            }
            other => println!("[!!] unexpected result for missing binary: {other:?}"),
        }
    }

    // --- 2. resolve the real binary -------------------------------------------
    let app_data = TunnelManager::default_app_data_dir();
    let location = match std::env::var("CLOUDFLARED_BIN") {
        Ok(p) => tunnel_core::BinaryLocation {
            path: p.into(),
            source: tunnel_core::BinarySource::Path,
        },
        Err(_) => match find_cloudflared(app_data.as_deref()) {
            Ok(loc) => loc,
            Err(e) => {
                eprintln!("[FAIL] could not find cloudflared: {e}");
                std::process::exit(1);
            }
        },
    };
    println!(
        "cloudflared: {} (source: {:?})",
        location.path.display(),
        location.source
    );

    println!("pgrep cloudflared BEFORE  start_tunnel: {}", pgrep_cloudflared());

    let mut mgr = TunnelManager::new();
    let info = match mgr.start(&location.path, port, Duration::from_secs(45)) {
        Ok(info) => info,
        Err(e) => {
            eprintln!("[FAIL] start_tunnel error: {e}");
            std::process::exit(1);
        }
    };
    println!(
        "start_tunnel({port}) => {{\"public_url\":\"{}\",\"pid\":{},\"local_port\":{}}}",
        info.public_url, info.pid, info.local_port
    );
    println!("pgrep cloudflared DURING  start_tunnel: {}", pgrep_cloudflared());

    // Prove the URL is live by hitting it through the tunnel. The quick-tunnel
    // edge route takes a few seconds to propagate, so retry briefly. A non-000
    // code proves the public hostname resolved and reached the local origin.
    let mut code = String::from("000");
    for attempt in 1..=8 {
        match Command::new("curl")
            .args(["-s", "-o", "/dev/null", "-w", "%{http_code}", "--max-time", "10"])
            .arg(&info.public_url)
            .output()
        {
            Ok(out) => {
                code = String::from_utf8_lossy(&out.stdout).trim().to_string();
                println!("curl attempt {attempt}: {} => http_code {}", info.public_url, code);
                if code != "000" {
                    break;
                }
            }
            Err(e) => {
                println!("curl attempt {attempt} skipped: {e}");
                break;
            }
        }
        std::thread::sleep(Duration::from_secs(3));
    }
    if code == "000" {
        println!("[warn] public URL not reachable within retry window (edge may be slow)");
    }

    match mgr.stop() {
        Ok(()) => println!("stop_tunnel() => Ok(())"),
        Err(e) => println!("[FAIL] stop_tunnel error: {e}"),
    }

    // Give the OS a moment to reap before the final pgrep observation.
    std::thread::sleep(Duration::from_millis(500));
    let after = pgrep_cloudflared();
    println!("pgrep cloudflared AFTER   stop_tunnel: {after}");

    if after == "(none)" {
        println!("[ok] no orphan cloudflared process after stop_tunnel");
    } else {
        println!("[FAIL] orphan cloudflared process(es) remain: {after}");
        std::process::exit(2);
    }
}
