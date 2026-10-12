//! Evidence harness for MAN-12 — the request interceptor.
//!
//! Runs the full path with **no GUI**: starts a local origin server and the real
//! interceptor proxy, drives it with real `curl` calls, then dumps the SQLite
//! store and the captured `webhook-received` event log. Every assertion in the
//! MAN-12 Definition of Done is checked and printed as `PASS` / `FAIL`.
//!
//! ```bash
//! cargo run -p interceptor-core --example intercept_curl -- /tmp/interceptor-evidence
//! ```
//!
//! Artifacts written to the output dir (default `./interceptor-evidence`):
//! `portforward.db` (SQLite store), `webhook-events.jsonl` (event log) and
//! `binary-payload.bin` (the 0x00–0xFF body sent through the proxy).

use std::fs;
use std::io::Write;
use std::path::PathBuf;
use std::process::Command;
use std::sync::Arc;

use axum::body::Bytes;
use axum::extract::State;
use axum::http::{HeaderMap, Method, StatusCode};
use axum::response::Response;
use axum::Router;
use interceptor_core::{is_iso8601_utc, is_uuid, CapturedRequest, Interceptor, RequestStore};
use tokio::net::TcpListener;

const BINARY_LEN: usize = 256;

#[tokio::main]
async fn main() {
    let out_dir = std::env::args()
        .nth(1)
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("interceptor-evidence"));
    fs::create_dir_all(&out_dir).expect("create output dir");

    let db_path = out_dir.join("portforward.db");
    let events_path = out_dir.join("webhook-events.jsonl");
    let binary_path = out_dir.join("binary-payload.bin");
    let _ = fs::remove_file(&db_path);
    let _ = fs::remove_file(&events_path);

    // The 0x00..=0xFF payload used to prove binary safety end to end.
    let binary: Vec<u8> = (0..BINARY_LEN).map(|i| i as u8).collect();
    fs::write(&binary_path, &binary).expect("write binary payload");

    let origin_port = spawn_origin().await;
    let store = RequestStore::open(&db_path).await.expect("open store");

    let sink_path = events_path.clone();
    let emitter: interceptor_core::Emitter = Arc::new(move |record: &CapturedRequest| {
        let line = serde_json::to_string(&record.to_json()).expect("serialize event");
        if let Ok(mut file) = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&sink_path)
        {
            let _ = writeln!(file, "{line}");
        }
    });

    let interceptor = Interceptor::start(origin_port, store.clone(), Some(emitter))
        .await
        .expect("start interceptor");
    let info = interceptor.info().clone();

    println!("== MAN-12 interceptor evidence ==");
    println!("origin        : http://127.0.0.1:{origin_port}");
    println!("proxy         : {}", info.proxy_url);
    println!("store         : {}", db_path.display());
    println!("event log     : {}", events_path.display());
    println!("binary payload: {} ({} bytes)", binary_path.display(), binary.len());
    println!();

    let base = &info.proxy_url;
    let stripe_body = r#"{"id":"evt_1","type":"charge.succeeded"}"#;

    let curls: Vec<(&str, Vec<String>)> = vec![
        (
            "JSON webhook with query params + custom headers",
            vec![
                "-sS".into(), "-i".into(),
                "-X".into(), "POST".into(),
                format!("{base}/api/webhooks/stripe?source=stripe&v=2"),
                "-H".into(), "Content-Type: application/json".into(),
                "-H".into(), "User-Agent: Stripe/1.0".into(),
                "--data-binary".into(), stripe_body.into(),
            ],
        ),
        (
            "upstream 418 must pass through",
            vec!["-sS".into(), "-i".into(), format!("{base}/teapot")],
        ),
        (
            "non-JSON form body",
            vec![
                "-sS".into(), "-i".into(),
                "-X".into(), "POST".into(),
                format!("{base}/form"),
                "-H".into(), "Content-Type: application/x-www-form-urlencoded".into(),
                "--data-binary".into(), "name=alice&role=admin".into(),
            ],
        ),
        (
            "binary body (0x00..=0xFF)",
            vec![
                "-sS".into(), "-i".into(),
                "-X".into(), "POST".into(),
                format!("{base}/binary"),
                "-H".into(), "Content-Type: application/octet-stream".into(),
                "--data-binary".into(), format!("@{}", binary_path.display()),
            ],
        ),
    ];

    println!("-- curl transcript (through the proxy) --");
    for (label, args) in &curls {
        println!("\n# {label}\n$ {}", shell_command("curl", args));
        match Command::new("curl").args(args).output() {
            Ok(output) => {
                print!("{}", String::from_utf8_lossy(&output.stdout));
                if !output.stderr.is_empty() {
                    eprint!("{}", String::from_utf8_lossy(&output.stderr));
                }
            }
            Err(err) => println!("curl failed: {err}"),
        }
    }

    println!("\n-- stopping proxy --");
    interceptor.stop().await.expect("stop interceptor");

    println!("\n-- stored records (sqlite3) --");
    run_sqlite(&db_path, "SELECT id, method, path, response_status, response_latency_ms FROM requests ORDER BY created_at;");
    println!("\n-- response_json of the stripe record (sqlite3) --");
    run_sqlite(&db_path, "SELECT response_json FROM requests WHERE path = '/api/webhooks/stripe';");
    println!("\n-- binary body integrity (sqlite3 hex) --");
    run_sqlite(
        &db_path,
        "SELECT length(body) AS bytes, substr(hex(body),1,32) AS hex_head FROM requests WHERE path = '/binary';",
    );

    println!("\n-- captured webhook-received event log ({}) --", events_path.display());
    let event_lines: Vec<String> = fs::read_to_string(&events_path)
        .unwrap_or_default()
        .lines()
        .map(str::to_string)
        .collect();
    for line in &event_lines {
        println!("{line}");
    }

    // ---- Definition-of-Done assertions -----------------------------------
    println!("\n== Definition of Done ==");
    let mut ok = true;

    let stored = store.all().await.expect("read store");
    ok &= check("4 requests persisted", stored.len() == 4, &format!("{}", stored.len()));
    ok &= check(
        "webhook-received fired once per request",
        event_lines.len() == 4,
        &format!("{} events", event_lines.len()),
    );

    let stripe = stored.iter().find(|r| r.path == "/api/webhooks/stripe");
    ok &= check("stripe record persisted", stripe.is_some(), "");
    if let Some(rec) = stripe {
        ok &= check("retrievable by id", store.get(&rec.id).await.unwrap().is_some(), &rec.id);
        ok &= check("request body persisted", rec.body == stripe_body.as_bytes(), "");
        ok &= check("upstream status 200 preserved", rec.response.status == 200, "");
        ok &= check(
            "upstream body unchanged",
            rec.response.body == stripe_body.as_bytes(),
            "",
        );
        ok &= check("query param captured", rec.query_params.get("v").map(String::as_str) == Some("2"), "");
        ok &= check(
            "request header captured",
            rec.headers.get("user-agent").map(String::as_str) == Some("Stripe/1.0"),
            "",
        );
    }

    let teapot = stored.iter().find(|r| r.path == "/teapot");
    ok &= check(
        "418 status/body pass through",
        teapot.map(|r| (r.response.status, r.response.body.clone()))
            == Some((418, b"I'm a teapot".to_vec())),
        "",
    );

    let bin = stored.iter().find(|r| r.path == "/binary");
    ok &= check(
        "binary body stored without corruption",
        bin.map(|r| r.body.clone()) == Some(binary.clone()),
        &format!("{} bytes", binary.len()),
    );

    let all_valid = stored.iter().all(|r| is_uuid(&r.id) && is_iso8601_utc(&r.timestamp));
    ok &= check("every record has uuid-v4 id + ISO-8601 UTC timestamp", all_valid, "");

    let events_valid = event_lines.iter().all(|line| {
        serde_json::from_str::<serde_json::Value>(line)
            .map(|v| {
                v.get("id").and_then(|x| x.as_str()).map(is_uuid).unwrap_or(false)
                    && v.get("response").is_some()
                    && v.get("query_params").is_some()
            })
            .unwrap_or(false)
    });
    ok &= check("every event is schema-valid JSON", events_valid, "");

    // Compare the sqlite3 hex dump with the expected payload.
    let hex = sqlite_stdout(
        &db_path,
        "SELECT hex(body) FROM requests WHERE path = '/binary';",
    );
    let expected: String = binary.iter().map(|b| format!("{b:02X}")).collect();
    ok &= check("sqlite3 hex(body) == original bytes", hex.trim() == expected, "");

    println!();
    if ok {
        println!("RESULT: PASS — all MAN-12 DoD checks satisfied");
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
    println!(
        "  [{}] {label}{suffix}",
        if passed { "PASS" } else { "FAIL" }
    );
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

fn sqlite_stdout(db: &std::path::Path, sql: &str) -> String {
    match Command::new("sqlite3").arg(db).arg(sql).output() {
        Ok(output) => String::from_utf8_lossy(&output.stdout).into_owned(),
        Err(err) => format!("sqlite3 unavailable: {err}"),
    }
}

fn run_sqlite(db: &std::path::Path, sql: &str) {
    println!("sqlite> {sql}");
    let output = Command::new("sqlite3")
        .arg("-header")
        .arg("-column")
        .arg(db)
        .arg(sql)
        .output();
    match output {
        Ok(output) => {
            print!("{}", String::from_utf8_lossy(&output.stdout));
            if !output.stderr.is_empty() {
                eprint!("{}", String::from_utf8_lossy(&output.stderr));
            }
        }
        Err(err) => println!("sqlite3 unavailable: {err}"),
    }
}

/// Local origin: echoes the body back (so body passthrough is provable) except
/// `/teapot`, which returns a distinctive 418.
async fn spawn_origin() -> u16 {
    let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let app = Router::new().fallback(origin).with_state(());
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    port
}

async fn origin(
    State(_): State<()>,
    method: Method,
    uri: axum::http::Uri,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    if uri.path() == "/teapot" {
        return Response::builder()
            .status(StatusCode::IM_A_TEAPOT)
            .header("content-type", "text/plain")
            .body(axum::body::Body::from("I'm a teapot"))
            .unwrap();
    }
    let marker = headers
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("none")
        .to_string();
    Response::builder()
        .status(StatusCode::OK)
        .header("content-type", "application/octet-stream")
        .header("x-origin-method", method.as_str())
        .header("x-origin-request-content-type", marker)
        .body(axum::body::Body::from(body))
        .unwrap()
}
