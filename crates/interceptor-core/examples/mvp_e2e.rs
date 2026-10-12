//! End-to-end MVP acceptance harness (MAN-15) — the full MAN-10 §5 stack, headless.
//!
//! Runs, from a cold start and with **no GUI**, every layer a user would touch:
//!
//! ```text
//!   curl ──► public HTTPS URL (cloudflared quick tunnel)
//!              └─► 127.0.0.1:<proxy>  interceptor proxy  (capture + persist + emit)
//!                        └─► localhost:<TARGET_PORT>   the test server (scripts/test-server.mjs)
//! ```
//!
//! then replays the stored capture **directly** to `localhost:<TARGET_PORT>`,
//! bypassing the proxy and the tunnel, and prints `RESULT: PASS` only when every
//! MAN-10 §5 Definition-of-Done check that can be proven without a window server
//! holds. The in-browser "UI renders within 200 ms" check is proven separately by
//! `scripts/ui-evidence.sh` (see MAN-14).
//!
//! ```bash
//! node scripts/test-server.mjs --port 3000 --log evidence/mvp/local-server.log &
//! TARGET_PORT=3000 LOCAL_SERVER_LOG=evidence/mvp/local-server.log \
//!   CLOUDFLARED_BIN=$(command -v cloudflared) \
//!   cargo run -p interceptor-core --example mvp_e2e -- evidence/mvp
//! ```
//!
//! Artifacts written to the output dir (default `evidence/mvp`):
//! * `portforward.db`          — the SQLite capture store;
//! * `webhook-events.jsonl`    — the emitted `webhook-received` payloads;
//! * `public-url.txt`          — the assigned `https://<id>.trycloudflare.com`;
//! * `curl-transcript.txt`     — the raw `curl` transcript through the tunnel;
//! * `captured-requests.json`  — the stored captures in MAN-10 §3 wire shape
//!   (also consumed by the UI evidence harness via `?realCaptures=`);
//! * `replay-output.json`      — the replay `ResponsePayload` (status/latency/body);
//! * `summary.json`            — every check, with observed values.

use std::fs;
use std::io::Write;
use std::path::PathBuf;
use std::process::Command;
use std::sync::Arc;
use std::time::{Duration, Instant};

use interceptor_core::{replay, CapturedRequest, Interceptor, RequestStore, ResponsePayload};
use tunnel_core::{BinaryLocation, BinarySource, TunnelManager};

const WEBHOOK_PATH: &str = "/api/webhooks/stripe";
const WEBHOOK_QUERY: &str = "source=stripe&v=2";
const WEBHOOK_BODY: &str =
    r#"{"id":"evt_mvp_1","type":"charge.succeeded","amount":4200,"currency":"usd","metadata":{"order_id":"ORD-MVP-1"}}"#;

fn now_iso() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}

fn pgrep_cloudflared() -> String {
    match Command::new("pgrep").arg("-x").arg("cloudflared").output() {
        Ok(out) => {
            let text = String::from_utf8_lossy(&out.stdout).trim().to_string();
            if text.is_empty() {
                "(none)".to_string()
            } else {
                text.replace('\n', ", ")
            }
        }
        Err(err) => format!("(pgrep failed: {err})"),
    }
}

/// Pull the value curl appended with `-w "\n__HTTP_CODE__%{http_code}__\n"`.
fn http_code(transcript: &str) -> String {
    transcript
        .rsplit("__HTTP_CODE__")
        .next()
        .and_then(|rest| rest.trim().split("__").next())
        .unwrap_or("")
        .trim()
        .to_string()
}

fn check(label: &str, passed: bool, detail: &str) -> bool {
    let suffix = if detail.is_empty() {
        String::new()
    } else {
        format!("  ({detail})")
    };
    println!("  [{}] {label}{suffix}", if passed { "PASS" } else { "FAIL" });
    passed
}

#[tokio::main]
async fn main() {
    let out_dir = std::env::args()
        .nth(1)
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("evidence/mvp"));
    fs::create_dir_all(&out_dir).expect("create output dir");

    let target_port: u16 = std::env::var("TARGET_PORT")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(3000);

    let db_path = out_dir.join("portforward.db");
    let events_path = out_dir.join("webhook-events.jsonl");
    let url_path = out_dir.join("public-url.txt");
    let transcript_path = out_dir.join("curl-transcript.txt");
    let captures_path = out_dir.join("captured-requests.json");
    let replay_path = out_dir.join("replay-output.json");
    let summary_path = out_dir.join("summary.json");
    let log_path = std::env::var("LOCAL_SERVER_LOG")
        .map(PathBuf::from)
        .unwrap_or_else(|_| out_dir.join("local-server.log"));
    for path in [
        &db_path,
        &events_path,
        &transcript_path,
        &captures_path,
        &replay_path,
    ] {
        let _ = fs::remove_file(path);
    }

    println!("== MAN-15 end-to-end MVP acceptance ==");
    println!("started       : {}", now_iso());
    println!("target server : http://localhost:{target_port}  (log: {})", log_path.display());
    println!("output dir    : {}", out_dir.display());
    println!();

    // --- 0. cold start: is the local test server up? ------------------------
    let mut server_ready = false;
    let mut last_probe = String::new();
    for _ in 0..40 {
        match Command::new("curl")
            .args([
                "-sS",
                "--max-time",
                "2",
                &format!("http://127.0.0.1:{target_port}/health"),
            ])
            .output()
        {
            Ok(out) if out.status.success() => {
                last_probe = String::from_utf8_lossy(&out.stdout).trim().to_string();
                if last_probe.contains("\"ok\":true") {
                    server_ready = true;
                    break;
                }
            }
            Ok(out) => last_probe = String::from_utf8_lossy(&out.stderr).trim().to_string(),
            Err(err) => last_probe = err.to_string(),
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
    println!("-- 0. local test server --");
    if !server_ready {
        eprintln!(
            "[FAIL] no test server answering on http://127.0.0.1:{target_port}/health (last: {last_probe:?})\n\
             hint: node scripts/test-server.mjs --port {target_port} --log {} &",
            log_path.display()
        );
        std::process::exit(2);
    }
    println!("  GET /health => {last_probe}");
    println!();

    // --- 1. interceptor proxy (capture + persist + emit) --------------------
    let store = RequestStore::open(&db_path).await.expect("open store");
    let events_sink = events_path.clone();
    let emitter: interceptor_core::Emitter = Arc::new(move |record: &CapturedRequest| {
        if let Ok(mut file) = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&events_sink)
        {
            let _ = writeln!(file, "{}", record.to_json());
        }
    });
    let interceptor = Interceptor::start(target_port, store.clone(), Some(emitter))
        .await
        .expect("start interceptor");
    let proxy_port = interceptor.info().proxy_port;
    println!("-- 1. interceptor --");
    println!("  start_interceptor(target_port={target_port}) => http://127.0.0.1:{proxy_port}");
    println!();

    // --- 2. cloudflared quick tunnel onto the proxy -------------------------
    let app_data = TunnelManager::default_app_data_dir();
    let location = match std::env::var("CLOUDFLARED_BIN") {
        Ok(path) => BinaryLocation {
            path: path.into(),
            source: BinarySource::Path,
        },
        Err(_) => match tunnel_core::find_cloudflared(app_data.as_deref()) {
            Ok(location) => location,
            Err(err) => {
                eprintln!("[FAIL] cloudflared not found: {err}");
                std::process::exit(1);
            }
        },
    };
    let binary = location.path.clone();
    println!("-- 2. tunnel (cloudflared) --");
    println!("  binary: {} ({:?})", location.path.display(), location.source);
    let pgrep_before = pgrep_cloudflared();
    println!("  pgrep -x cloudflared BEFORE  : {pgrep_before}");

    let tunnel_started = Instant::now();
    let joined = tokio::task::spawn_blocking(move || {
        let mut manager = TunnelManager::new();
        match manager.start(&binary, proxy_port, Duration::from_secs(45)) {
            Ok(info) => Ok((manager, info)),
            Err(err) => Err(err.to_string()),
        }
    })
    .await
    .expect("tunnel task panicked");
    let (mut manager, tunnel_info) = match joined {
        Ok(value) => value,
        Err(err) => {
            eprintln!("[FAIL] start_tunnel: {err}");
            std::process::exit(1);
        }
    };
    println!(
        "  start_tunnel({proxy_port}) => {{\"public_url\":\"{}\",\"pid\":{},\"local_port\":{}}}  ({} ms)",
        tunnel_info.public_url,
        tunnel_info.pid,
        tunnel_info.local_port,
        tunnel_started.elapsed().as_millis()
    );
    let pgrep_during = pgrep_cloudflared();
    println!("  pgrep -x cloudflared DURING  : {pgrep_during}");
    fs::write(&url_path, &tunnel_info.public_url).expect("write public url");
    println!();

    // --- 3. curl POST to the public URL until the edge route propagates -----
    let public_url = tunnel_info.public_url.clone();
    let request_url = format!("{public_url}{WEBHOOK_PATH}?{WEBHOOK_QUERY}");
    println!("-- 3. curl POST through the public URL --");
    let mut transcript = String::new();
    let mut code = String::from("000");
    let mut body_from_public = String::new();
    for attempt in 1..=20u32 {
        let args: Vec<String> = vec![
            "-sS".into(),
            "-i".into(),
            "--max-time".into(),
            "20".into(),
            "-X".into(),
            "POST".into(),
            request_url.clone(),
            "-H".into(),
            "Content-Type: application/json".into(),
            "-H".into(),
            "User-Agent: Stripe/1.0".into(),
            "-H".into(),
            "X-Custom-Marker: mvp-acceptance".into(),
            "--data-binary".into(),
            WEBHOOK_BODY.into(),
            "-w".into(),
            "\n__HTTP_CODE__%{http_code}__\n".into(),
        ];
        transcript.push_str(&format!(
            "\n--- attempt {attempt} at {} ---\n$ curl {}\n",
            now_iso(),
            args.join(" ")
        ));
        match Command::new("curl").args(&args).output() {
            Ok(out) => {
                let stdout = String::from_utf8_lossy(&out.stdout).to_string();
                let stderr = String::from_utf8_lossy(&out.stderr).to_string();
                transcript.push_str(&stdout);
                transcript.push_str(&stderr);
                code = http_code(&stdout);
                println!("  attempt {attempt}: {request_url} => http_code {code}");
                if code == "200" {
                    body_from_public = stdout.clone();
                    break;
                }
            }
            Err(err) => {
                transcript.push_str(&format!("curl failed: {err}\n"));
                println!("  attempt {attempt}: curl failed: {err}");
            }
        }
        tokio::time::sleep(Duration::from_secs(3)).await;
    }
    fs::write(&transcript_path, &transcript).expect("write transcript");
    transcript
        .lines()
        .filter(|line| line.contains('{') || line.starts_with("HTTP/"))
        .take(12)
        .for_each(|line| println!("    | {line}"));
    println!("  transcript: {}", transcript_path.display());
    println!();

    // --- 4. the capture is persisted and emitted ---------------------------
    let mut captured: Option<CapturedRequest> = None;
    for _ in 0..50 {
        let all = store.all().await.unwrap_or_default();
        if let Some(record) = all.iter().find(|r| r.method == "POST" && r.path == WEBHOOK_PATH) {
            captured = Some(record.clone());
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    let captures = store.all().await.expect("read store");
    let mut ordered = captures.clone();
    ordered.sort_by(|a, b| b.timestamp.cmp(&a.timestamp));
    let wire: Vec<serde_json::Value> = ordered.iter().map(CapturedRequest::to_json).collect();
    fs::write(
        &captures_path,
        serde_json::to_string_pretty(&wire).expect("serialize captures"),
    )
    .expect("write captures");
    println!("-- 4. capture store --");
    println!(
        "  captured_request_count() => {}  (newest: {})",
        captures.len(),
        captured
            .as_ref()
            .map(|r| format!("{} {} {} @ {}", r.id, r.method, r.path, r.timestamp))
            .unwrap_or_else(|| "(none)".into())
    );
    println!("  wire captures: {}", captures_path.display());
    println!();

    // --- 5. replay directly to the local server (no proxy, no tunnel) ------
    println!("-- 5. replay_request(id) -> localhost:{target_port} (direct) --");
    let mut replay_payload: Option<ResponsePayload> = None;
    let mut replay_url_shown = String::new();
    if let Some(record) = &captured {
        replay_url_shown = interceptor_core::replay_url(target_port, record);
        println!("  {replay_url_shown}");
        match replay(&store, &record.id, target_port).await {
            Ok(payload) => {
                println!(
                    "  -> status={} latency={}ms body={}",
                    payload.status,
                    payload.latency_ms,
                    String::from_utf8_lossy(&payload.body_bytes())
                );
                fs::write(
                    &replay_path,
                    serde_json::to_string_pretty(&payload).expect("serialize payload"),
                )
                .expect("write replay output");
                replay_payload = Some(payload);
            }
            Err(err) => println!("  -> replay failed: {err}"),
        }
    } else {
        println!("  -> no capture to replay");
    }
    println!();

    // --- 6. the local server log (both hops) -------------------------------
    let log_text = fs::read_to_string(&log_path).unwrap_or_default();
    let entries: Vec<serde_json::Value> = log_text
        .lines()
        .filter_map(|line| serde_json::from_str(line).ok())
        .collect();
    println!("-- 6. local server log ({}) --", log_path.display());
    for line in log_text.lines() {
        println!("  {line}");
    }
    println!();

    // --- 7. stop the tunnel, then the interceptor --------------------------
    let stop_result = tokio::task::spawn_blocking(move || manager.stop().map_err(|e| e.to_string()))
        .await
        .expect("stop task panicked");
    tokio::time::sleep(Duration::from_millis(500)).await;
    let pgrep_after = pgrep_cloudflared();
    let final_count = store.count().await.unwrap_or(-1);
    interceptor.stop().await.expect("stop interceptor");

    // --- 8. Definition of Done -------------------------------------------
    println!("== Definition of Done (MAN-10 §5) ==");
    let mut ok = true;

    let url_ok = tunnel_info.public_url.starts_with("https://")
        && tunnel_info.public_url.ends_with(".trycloudflare.com");
    ok &= check(
        "starting the tunnel output a public HTTPS URL",
        url_ok,
        &tunnel_info.public_url,
    );
    ok &= check(
        "curl POST to the public URL returned 200",
        code == "200",
        &format!("http_code {code}"),
    );
    // The echo embeds the raw body as a JSON string, so its quotes are escaped
    // in the response the tunnel returned.
    let escaped_body = WEBHOOK_BODY.replace('"', "\\\"");
    ok &= check(
        "the public URL forwarded to the local test server (echo received)",
        body_from_public.contains("\"ok\":true")
            && body_from_public.contains("x-served-by: port-forward-test-server")
            && body_from_public.contains("\"path\":\"/api/webhooks/stripe\"")
            && body_from_public.contains(&escaped_body),
        "echo.ok + upstream x-served-by header + the original path/body echoed back",
    );

    let record = captured.as_ref();
    ok &= check(
        "the request was captured (method/path/query)",
        record
            .map(|r| r.method == "POST" && r.path == WEBHOOK_PATH
                && r.query_params.get("source").map(String::as_str) == Some("stripe")
                && r.query_params.get("v").map(String::as_str) == Some("2"))
            .unwrap_or(false),
        record
            .map(|r| format!("{} {} query={:?}", r.method, r.path, r.query_params))
            .unwrap_or_else(|| "(no capture)".into())
            .as_str(),
    );
    ok &= check(
        "the captured body is byte-identical to what curl sent",
        record.map(|r| r.body == WEBHOOK_BODY.as_bytes()).unwrap_or(false),
        &format!("{} bytes", record.map(|r| r.body.len()).unwrap_or(0)),
    );
    ok &= check(
        "the captured upstream response is a 200 with a measured latency",
        record.map(|r| r.response.status == 200 && r.response.latency_ms < 5_000).unwrap_or(false),
        record
            .map(|r| format!("status={} latency={}ms", r.response.status, r.response.latency_ms))
            .unwrap_or_else(|| "(no capture)".into())
            .as_str(),
    );
    ok &= check(
        "the public URL produced exactly one capture",
        captures.len() == 1,
        &format!("{} record(s)", captures.len()),
    );

    // The `/health` readiness probes are logged too; the DoD cares about the
    // two webhook POSTs (the forwarded request and the replay).
    let posts: Vec<&serde_json::Value> = entries
        .iter()
        .filter(|value| {
            value.get("method").and_then(|m| m.as_str()) == Some("POST")
                && value.get("path").and_then(|p| p.as_str()) == Some(WEBHOOK_PATH)
        })
        .collect();
    let post = |index: usize, key: &str| -> String {
        posts
            .get(index)
            .and_then(|value| value.get(key))
            .map(|value| {
                value
                    .as_str()
                    .map(str::to_string)
                    .unwrap_or_else(|| value.to_string())
            })
            .unwrap_or_default()
    };
    ok &= check(
        "the local server saw both the forwarded request and the replay",
        posts.len() == 2,
        &format!("{} webhook POST(s) in the server log", posts.len()),
    );
    ok &= check(
        "the forwarded request reached the local server unaltered",
        posts.len() >= 1
            && post(0, "method") == "POST"
            && post(0, "path") == WEBHOOK_PATH
            && post(0, "body") == WEBHOOK_BODY,
        &format!("{} {} ({} bytes)", post(0, "method"), post(0, "path"), post(0, "bytes")),
    );

    let replay_status = replay_payload.as_ref().map(|p| p.status);
    let replay_latency = replay_payload.as_ref().map(|p| p.latency_ms);
    ok &= check(
        "Replay returned a fresh 200 from the local server",
        replay_status == Some(200),
        &format!("status={replay_status:?} latency={replay_latency:?}ms url={replay_url_shown}"),
    );
    ok &= check(
        "the replayed request reached the local server identical to the original",
        posts.len() == 2
            && post(1, "method") == post(0, "method")
            && post(1, "path") == post(0, "path")
            && post(1, "body") == post(0, "body")
            && post(1, "content_type") == "application/json"
            && post(1, "user_agent") == "Stripe/1.0",
        &format!(
            "{} {} content-type={} ua={} remote_addr={}",
            post(1, "method"),
            post(1, "path"),
            post(1, "content_type"),
            post(1, "user_agent"),
            post(1, "remote_addr")
        ),
    );
    ok &= check(
        "the replay bypassed the proxy (no new capture)",
        final_count == 1,
        &format!("captured_request_count() => {final_count}"),
    );
    ok &= check(
        "stop_tunnel() left no orphan cloudflared process",
        stop_result.is_ok() && pgrep_after == "(none)",
        &format!("stop={stop_result:?} pgrep_after={pgrep_after}"),
    );

    let summary = serde_json::json!({
        "ranAt": now_iso(),
        "publicUrl": tunnel_info.public_url,
        "tunnelPid": tunnel_info.pid,
        "proxyPort": proxy_port,
        "targetPort": target_port,
        "curlHttpCode": code,
        "capturedId": record.map(|r| r.id.clone()),
        "capturedTimestamp": record.map(|r| r.timestamp.clone()),
        "captures": captures.len(),
        "replayStatus": replay_status,
        "replayLatencyMs": replay_latency,
        "replayUrl": replay_url_shown,
        "localServerEntries": entries.len(),
        "localServerWebhookPosts": posts.len(),
        "pgrepBefore": pgrep_before,
        "pgrepDuring": pgrep_during,
        "pgrepAfter": pgrep_after,
        "uiRenderBudgetMs": 200,
        "uiRenderEvidence": "scripts/ui-evidence.sh (real component tree, event->DOM latency)",
        "checksPass": ok,
    });
    fs::write(
        &summary_path,
        serde_json::to_string_pretty(&summary).expect("serialize summary"),
    )
    .expect("write summary");

    println!();
    if ok {
        println!("RESULT: PASS — every headless MAN-10 §5 check satisfied");
        println!("Next: scripts/ui-evidence.sh proves the <=200 ms UI render, then post the evidence.");
    } else {
        println!("RESULT: FAIL — see the failed checks above");
        std::process::exit(1);
    }
}
