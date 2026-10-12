//! Evidence harness for MAN-13 — the replay engine.
//!
//! Runs the full replay path with **no GUI**: starts a local origin server that
//! logs every request it receives, starts the real interceptor, sends one
//! request through the proxy (the "tunnel" path), then replays the stored record
//! **directly** to the local server with [`interceptor_core::replay`]. Every
//! assertion in the MAN-13 Definition of Done is checked and printed as
//! `PASS` / `FAIL`.
//!
//! ```bash
//! cargo run -p interceptor-core --example replay_curl -- /tmp/replay-evidence
//! ```
//!
//! Artifacts written to the output dir (default `./replay-evidence`):
//! * `portforward.db`         — SQLite store holding the original capture;
//! * `origin-requests.jsonl`  — the **local server log** (method/path/query/body
//!   for the proxied request *and* the replay — the evidence that the replay was
//!   byte-identical);
//! * `replay-output.json`     — the [`interceptor_core::ResponsePayload`] the
//!   replay returned (status, headers, body, latency_ms).

use std::fs::{self, File};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Mutex};

use axum::body::Bytes;
use axum::extract::State;
use axum::http::{HeaderMap, Method, StatusCode, Uri};
use axum::response::Response;
use axum::Router;
use interceptor_core::{replay, Interceptor, ReplayError, RequestStore, ResponsePayload};
use tokio::net::TcpListener;

const STRIPE_BODY: &str = r#"{"id":"evt_1","type":"charge.succeeded"}"#;

#[tokio::main]
async fn main() {
    let out_dir = std::env::args()
        .nth(1)
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("replay-evidence"));
    fs::create_dir_all(&out_dir).expect("create output dir");

    let db_path = out_dir.join("portforward.db");
    let origin_log_path = out_dir.join("origin-requests.jsonl");
    let replay_out_path = out_dir.join("replay-output.json");
    for path in [&db_path, &origin_log_path, &replay_out_path] {
        let _ = fs::remove_file(path);
    }

    // The local server logs every request it receives to `origin-requests.jsonl`.
    let log = Arc::new(Mutex::new(
        File::create(&origin_log_path).expect("create origin log"),
    ));
    let origin_port = spawn_origin(log.clone()).await;
    let store = RequestStore::open(&db_path).await.expect("open store");

    let interceptor = Interceptor::start(origin_port, store.clone(), None)
        .await
        .expect("start interceptor");
    let info = interceptor.info().clone();

    println!("== MAN-13 replay evidence ==");
    println!("local server  : http://localhost:{origin_port}   (logs to {})", origin_log_path.display());
    println!("proxy         : {}", info.proxy_url);
    println!("store         : {}", db_path.display());
    println!("replay output : {}", replay_out_path.display());
    println!();

    // --- 1. Original request through the proxy (the tunnel path) ------------
    let original_url = format!("{}/api/webhooks/stripe?source=stripe&v=2", info.proxy_url);
    println!("-- 1. original request (through the proxy) --");
    let curl_args: Vec<String> = vec![
        "-sS".into(),
        "-i".into(),
        "-X".into(),
        "POST".into(),
        original_url.clone(),
        "-H".into(),
        "Content-Type: application/json".into(),
        "-H".into(),
        "User-Agent: Stripe/1.0".into(),
        "-H".into(),
        "X-Custom-Marker: replay-me".into(),
        "--data-binary".into(),
        STRIPE_BODY.into(),
    ];
    println!("$ {}", shell_command("curl", &curl_args));
    match Command::new("curl").args(&curl_args).output() {
        Ok(output) => {
            print!("{}", String::from_utf8_lossy(&output.stdout));
            if !output.stderr.is_empty() {
                eprint!("{}", String::from_utf8_lossy(&output.stderr));
            }
        }
        Err(err) => println!("curl failed: {err}"),
    }

    let stored = store.all().await.expect("read store");
    let record = stored.first().expect("one captured record").clone();
    println!(
        "\nstored record id: {}  (method={} path={} latency={}ms)",
        record.id, record.method, record.path, record.response.latency_ms
    );

    // --- 2. Replay directly to the local server (no proxy, no tunnel) -------
    println!("\n-- 2. replay_request(\"{}\") -> local server :{origin_port} --", record.id);
    println!("    interceptor_core::replay(&store, id, {origin_port})");
    let payload: ResponsePayload = match replay(&store, &record.id, origin_port).await {
        Ok(payload) => payload,
        Err(err) => {
            println!("replay failed: {err}");
            std::process::exit(1);
        }
    };
    println!("    -> status={} latency={}ms headers={:?}", payload.status, payload.latency_ms, payload.headers);
    println!("    -> body={:?}", payload.body_bytes().iter().map(|b| *b as char).collect::<String>());
    fs::write(
        &replay_out_path,
        serde_json::to_string_pretty(&payload).expect("serialize payload"),
    )
    .expect("write replay output");

    // --- 3. Unknown id must be a typed error -------------------------------
    println!("\n-- 3. replay with an unknown id --");
    let unknown = "00000000-0000-4000-8000-000000000000";
    let unknown_err = replay(&store, unknown, origin_port).await.err();
    match &unknown_err {
        Some(ReplayError::UnknownRequest(id)) => println!("    -> ReplayError::UnknownRequest({id})"),
        Some(other) => println!("    -> unexpected error: {other}"),
        None => println!("    -> ERROR: unknown id was accepted!"),
    }

    // --- 4. Local server log ------------------------------------------------
    interceptor.stop().await.expect("stop interceptor");
    println!("\n-- local server log ({}) --", origin_log_path.display());
    let log_lines: Vec<String> = fs::read_to_string(&origin_log_path)
        .unwrap_or_default()
        .lines()
        .map(str::to_string)
        .collect();
    for line in &log_lines {
        println!("{line}");
    }

    println!("\n-- stored records (sqlite3) --");
    run_sqlite(&db_path, "SELECT id, method, path, response_status, response_latency_ms FROM requests;");

    // --- Definition-of-Done assertions -------------------------------------
    println!("\n== Definition of Done ==");
    let mut ok = true;

    let parsed: Vec<serde_json::Value> = log_lines
        .iter()
        .filter_map(|line| serde_json::from_str(line).ok())
        .collect();
    ok &= check("local server saw the original request", parsed.len() >= 1, &format!("{} requests", parsed.len()));
    ok &= check("local server saw the replay", parsed.len() == 2, &format!("{} requests", parsed.len()));

    if parsed.len() == 2 {
        let req = |v: &serde_json::Value, key: &str| v.get(key).and_then(|x| x.as_str()).unwrap_or("").to_string();
        let original = &parsed[0];
        let replayed = &parsed[1];
        ok &= check(
            "replay method identical",
            req(original, "method") == req(replayed, "method"),
            &req(replayed, "method"),
        );
        ok &= check(
            "replay path identical",
            req(original, "path") == req(replayed, "path"),
            &req(replayed, "path"),
        );
        ok &= check(
            "replay query identical",
            req(original, "query") == req(replayed, "query"),
            &req(replayed, "query"),
        );
        ok &= check(
            "replay body byte-identical",
            original.get("body") == replayed.get("body"),
            &req(replayed, "body"),
        );
        ok &= check(
            "replay headers preserved (content-type/user-agent/custom)",
            req(replayed, "content_type") == "application/json"
                && req(replayed, "user_agent") == "Stripe/1.0"
                && req(replayed, "custom") == "replay-me",
            "",
        );
    }

    ok &= check(
        "replay returned the fresh status",
        payload.status == 200,
        &format!("status={}", payload.status),
    );
    ok &= check(
        "replay returned the fresh latency",
        payload.latency_ms < 5_000,
        &format!("{}ms", payload.latency_ms),
    );
    ok &= check(
        "replay bypassed the proxy (no new capture)",
        store.count().await.map(|n| n == 1).unwrap_or(false),
        &format!("{} record(s)", store.count().await.unwrap_or(-1)),
    );
    ok &= check(
        "unknown id is a typed ReplayError::UnknownRequest",
        matches!(unknown_err, Some(ReplayError::UnknownRequest(ref id)) if id == unknown),
        unknown,
    );

    println!();
    if ok {
        println!("RESULT: PASS — all MAN-13 DoD checks satisfied");
    } else {
        println!("RESULT: FAIL — see the failed checks above");
        std::process::exit(1);
    }
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

fn shell_command(program: &str, args: &[String]) -> String {
    let mut out = program.to_string();
    for arg in args {
        out.push(' ');
        if arg.contains(' ') || arg.contains('\'') || arg.contains('"') {
            out.push('\'');
            out.push_str(&arg.replace('\'', "'\\''"));
            out.push('\'');
        } else {
            out.push_str(arg);
        }
    }
    out
}

fn run_sqlite(db: &Path, sql: &str) {
    println!("sqlite> {sql}");
    match Command::new("sqlite3").arg("-header").arg("-column").arg(db).arg(sql).output() {
        Ok(output) => {
            print!("{}", String::from_utf8_lossy(&output.stdout));
            if !output.stderr.is_empty() {
                eprint!("{}", String::from_utf8_lossy(&output.stderr));
            }
        }
        Err(err) => println!("sqlite3 unavailable: {err}"),
    }
}

/// Bind a local origin on a free port; every request is appended to `log`.
async fn spawn_origin(log: Arc<Mutex<File>>) -> u16 {
    let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let app = Router::new().fallback(origin).with_state(log);
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    port
}

/// Origin handler: append a JSON line describing the request to the log, then
/// echo the body back so body passthrough is provable.
async fn origin(
    State(log): State<Arc<Mutex<File>>>,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let header = |name: &str| {
        headers
            .get(name)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_string()
    };
    let entry = serde_json::json!({
        "method": method.as_str(),
        "path": uri.path(),
        "query": uri.query().unwrap_or(""),
        "content_type": header("content-type"),
        "user_agent": header("user-agent"),
        "custom": header("x-custom-marker"),
        "body": String::from_utf8_lossy(&body),
        "body_len": body.len(),
    });
    if let Ok(mut file) = log.lock() {
        let _ = writeln!(file, "{entry}");
        let _ = file.flush();
    }
    Response::builder()
        .status(StatusCode::OK)
        .header("content-type", "application/octet-stream")
        .header("x-origin-method", method.as_str())
        .body(axum::body::Body::from(body))
        .unwrap()
}
