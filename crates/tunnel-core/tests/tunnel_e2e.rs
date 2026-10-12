//! Gated end-to-end integration test: spawns the real `cloudflared` binary.
//!
//! Disabled by default so `cargo test` stays fast and offline. Enable with:
//! ```bash
//! TUNNEL_E2E=1 CLOUDFLARED_BIN=/path/to/cloudflared cargo test -p tunnel-core -- --ignored
//! ```

use std::collections::HashSet;
use std::io::Read;
use std::net::TcpListener;
use std::path::Path;
use std::process::Command;
use std::time::{Duration, Instant};

use tunnel_core::{find_cloudflared, TunnelManager};

fn has_cloudflared() -> Option<std::path::PathBuf> {
    if let Ok(p) = std::env::var("CLOUDFLARED_BIN") {
        return Some(p.into());
    }
    find_cloudflared(TunnelManager::default_app_data_dir().as_deref())
        .ok()
        .map(|l| l.path)
}

/// Process ids whose process *name* is exactly `cloudflared`.
///
/// Uses `pgrep -x` (exact name match). The previous `pgrep -f` matched the whole
/// command line, so any invoking shell/editor whose command line merely
/// contained the string "cloudflared" (e.g.
/// `CLOUDFLARED_BIN=.../cloudflared cargo test …`) was reported as a false
/// orphan. See MAN-22.
fn cloudflared_pids() -> HashSet<u32> {
    let out = Command::new("pgrep")
        .arg("-x")
        .arg("cloudflared")
        .output()
        .expect("pgrep should be runnable");
    String::from_utf8_lossy(&out.stdout)
        .split_whitespace()
        .filter_map(|s| s.parse::<u32>().ok())
        .collect()
}

/// Whether a live process with `pid` exists (unix `/proc`).
fn pid_exists(pid: u32) -> bool {
    Path::new("/proc").join(pid.to_string()).exists()
}

/// Wait (bounded) until `pid` is gone and no `cloudflared`-named process newer
/// than `baseline` remains.
///
/// Reaping is asynchronous, so poll instead of sleeping a fixed amount. Returns
/// `None` once clean, or a diagnostic string on timeout.
fn wait_until_orphan_free(baseline: &HashSet<u32>, pid: u32, budget: Duration) -> Option<String> {
    let deadline = Instant::now() + budget;
    loop {
        let leaked: Vec<u32> = cloudflared_pids().difference(baseline).copied().collect();
        let child_alive = pid_exists(pid);
        if leaked.is_empty() && !child_alive {
            return None;
        }
        if Instant::now() >= deadline {
            return Some(format!(
                "child pid {pid} still alive: {child_alive}; new cloudflared pids: {leaked:?}"
            ));
        }
        std::thread::sleep(Duration::from_millis(100));
    }
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

    // BEFORE: snapshot cloudflared processes by exact name so a pre-existing
    // tunnel is not mistaken for one this test leaks.
    let before = cloudflared_pids();

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

    // DURING: the child is alive and identifiable by its exact process name.
    assert!(
        pid_exists(info.pid),
        "cloudflared child pid {} should be running",
        info.pid
    );
    let during = cloudflared_pids();
    assert!(
        during.contains(&info.pid),
        "pgrep -x cloudflared should include child pid {}; saw {during:?}",
        info.pid
    );

    mgr.stop().expect("stop should succeed");

    // AFTER: bounded poll because reap can be asynchronous.
    if let Some(diagnostic) = wait_until_orphan_free(&before, info.pid, Duration::from_secs(5)) {
        panic!("no orphan cloudflared expected, but {diagnostic}");
    }
}

/// Negative case: a live process whose name *is* `cloudflared` must be flagged.
///
/// `stop()` cannot be made to orphan a real tunnel on demand, so simulate an
/// orphan by copying a long-running binary to a file named `cloudflared` and
/// spawning it. This proves the name-based check (and the bounded wait) catches
/// a real orphan instead of silently passing.
#[test]
#[cfg(unix)]
fn orphan_check_detects_a_real_named_orphan() {
    let tmp = tempfile::tempdir().unwrap();
    let sleep_src = ["/bin/sleep", "/usr/bin/sleep"]
        .iter()
        .map(Path::new)
        .find(|p| p.exists())
        .expect("sleep binary not found");
    let decoy = tmp.path().join("cloudflared");
    std::fs::copy(sleep_src, &decoy).expect("copy sleep -> cloudflared");
    let mut child = Command::new(&decoy)
        .arg("30")
        .spawn()
        .expect("spawn decoy cloudflared");

    // Wait until the OS has exec'd it and `pgrep -x` can see the exact name.
    let seen = {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            if cloudflared_pids().contains(&child.id()) {
                break true;
            }
            if Instant::now() >= deadline {
                break false;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
    };
    assert!(seen, "pgrep -x cloudflared should see the decoy process");

    // A real orphan must be reported, not treated as clean.
    let verdict = wait_until_orphan_free(&HashSet::new(), child.id(), Duration::from_millis(300));
    assert!(
        verdict.is_some(),
        "a live cloudflared-named process must be flagged as an orphan"
    );

    let _ = child.kill();
    let _ = child.wait();
}
