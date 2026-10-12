//! Gated end-to-end integration test: spawns the real `cloudflared` binary.
//!
//! Disabled by default so `cargo test` stays fast and offline. Enable with:
//! ```bash
//! TUNNEL_E2E=1 CLOUDFLARED_BIN=/path/to/cloudflared cargo test -p tunnel-core -- --ignored
//! ```

use std::io::Read;
use std::net::TcpListener;
use std::process::Command;
use std::time::Duration;

use tunnel_core::{find_cloudflared, TunnelManager};

fn has_cloudflared() -> Option<std::path::PathBuf> {
    if let Ok(p) = std::env::var("CLOUDFLARED_BIN") {
        return Some(p.into());
    }
    find_cloudflared(TunnelManager::default_app_data_dir().as_deref())
        .ok()
        .map(|l| l.path)
}

#[test]
#[ignore = "requires network + cloudflared; set TUNNEL_E2E=1 to run"]
fn start_returns_live_url_and_stop_leaves_no_orphan() {
    if std::env::var("TUNNEL_E2E").as_deref() != Ok("1") {
        eprintln!("skipping: set TUNNEL_E2E=1 to run this network test");
        return;
    }
    let Some(bin) = has_cloudflared() else {
        eprintln!("skipping: cloudflared not found");
        return;
    };

    // A real local origin so the tunnel has something to forward to.
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    std::thread::spawn(move || {
        if let Ok((mut stream, _)) = listener.accept() {
            let mut buf = [0u8; 512];
            let _ = stream.read(&mut buf);
            let _ = std::io::Write::write_all(
                &mut stream,
                b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nOK",
            );
        }
    });

    let mut mgr = TunnelManager::new();
    let info = mgr
        .start(&bin, port, Duration::from_secs(45))
        .expect("tunnel should start and report a URL");

    assert!(
        info.public_url.starts_with("https://") && info.public_url.ends_with(".trycloudflare.com"),
        "unexpected url: {}",
        info.public_url
    );
    assert_eq!(info.local_port, port);
    assert!(info.pid > 0);

    let during = Command::new("pgrep").arg("-x").arg("cloudflared").output().unwrap();
    assert!(
        !String::from_utf8_lossy(&during.stdout).trim().is_empty(),
        "cloudflared should be running"
    );

    mgr.stop().expect("stop should succeed");
    std::thread::sleep(Duration::from_millis(500));
    let after = Command::new("pgrep").arg("-f").arg("cloudflared").output().unwrap();
    let after_text = String::from_utf8_lossy(&after.stdout).trim().to_string();
    assert!(
        after_text.is_empty(),
        "no orphan cloudflared expected, found: {after_text}"
    );
}
