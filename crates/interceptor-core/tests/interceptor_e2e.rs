//! End-to-end integration test for the MAN-12 interceptor.
//!
//! Spins up a real local origin server and the real proxy on loopback, sends
//! requests through the proxy, and asserts the Definition of Done:
//!
//! * the upstream status/body come back unchanged;
//! * records are persisted and retrievable by `id`;
//! * `webhook-received` (the emitter callback) fires exactly once per request
//!   with a schema-valid payload;
//! * non-JSON and binary bodies round-trip without corruption.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use axum::body::Bytes;
use axum::extract::State;
use axum::http::{HeaderMap, Method, StatusCode};
use axum::response::Response;
use axum::Router;
use interceptor_core::{is_iso8601_utc, is_uuid, CapturedRequest, Interceptor, RequestStore};
use tokio::net::TcpListener;

/// Bind a local origin server on a free port; returns `(port, join handle)`.
async fn spawn_origin() -> u16 {
    let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let app = Router::new().fallback(origin).with_state(());
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    port
}

/// Origin handler: echoes the request body (so the caller can prove the body is
/// forwarded unaltered), except `/teapot` which returns a distinctive 418.
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

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn forwards_captures_persists_and_emits_once() {
    let origin_port = spawn_origin().await;

    let store = RequestStore::in_memory().await.unwrap();
    let events: Arc<Mutex<Vec<CapturedRequest>>> = Arc::new(Mutex::new(Vec::new()));
    let sink = events.clone();
    let emitter: interceptor_core::Emitter = Arc::new(move |record: &CapturedRequest| {
        sink.lock().unwrap().push(record.clone());
    });

    let interceptor = Interceptor::start(origin_port, store.clone(), Some(emitter))
        .await
        .unwrap();
    let base = interceptor.info().proxy_url.clone();
    let client = reqwest::Client::new();

    // 1. JSON POST with query params and custom headers.
    let stripe_body = br#"{"id":"evt_1","type":"charge.succeeded"}"#.to_vec();
    let resp = client
        .post(format!("{base}/api/webhooks/stripe?source=stripe&v=2"))
        .header("content-type", "application/json")
        .header("user-agent", "Stripe/1.0")
        .body(stripe_body.clone())
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status().as_u16(), 200);
    assert_eq!(resp.headers()["x-origin-method"], "POST");
    let echoed = resp.bytes().await.unwrap();
    assert_eq!(
        echoed.to_vec(),
        stripe_body,
        "upstream body must come back unchanged"
    );

    // 2. Distinct upstream status must be preserved.
    let resp = client.get(format!("{base}/teapot")).send().await.unwrap();
    assert_eq!(resp.status().as_u16(), 418);
    assert_eq!(resp.bytes().await.unwrap().to_vec(), b"I'm a teapot");

    // 3. Binary-safe body.
    let binary: Vec<u8> = (0u8..=255).collect();
    let resp = client
        .post(format!("{base}/binary"))
        .header("content-type", "application/octet-stream")
        .body(binary.clone())
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status().as_u16(), 200);
    assert_eq!(resp.bytes().await.unwrap().to_vec(), binary);

    interceptor.stop().await.unwrap();

    // --- persistence -------------------------------------------------------
    assert_eq!(store.count().await.unwrap(), 3, "one record per request");

    let all = store.all().await.unwrap();
    let stripe = all
        .iter()
        .find(|r| r.path == "/api/webhooks/stripe")
        .expect("stripe record persisted");
    assert_eq!(
        store.get(&stripe.id).await.unwrap().unwrap().id,
        stripe.id,
        "record retrievable by id"
    );
    assert_eq!(stripe.method, "POST");
    assert_eq!(stripe.headers.get("content-type").unwrap(), "application/json");
    assert_eq!(stripe.headers.get("user-agent").unwrap(), "Stripe/1.0");
    assert_eq!(stripe.query_params.get("source").unwrap(), "stripe");
    assert_eq!(stripe.query_params.get("v").unwrap(), "2");
    assert_eq!(stripe.body, stripe_body);
    assert_eq!(stripe.response.status, 200);
    assert_eq!(stripe.response.body, stripe_body);

    let teapot = all.iter().find(|r| r.path == "/teapot").unwrap();
    assert_eq!(teapot.response.status, 418);
    assert_eq!(teapot.response.body, b"I'm a teapot".to_vec());

    let bin = all.iter().find(|r| r.path == "/binary").unwrap();
    assert_eq!(bin.body, binary, "binary body stored without corruption");
    assert_eq!(bin.to_wire().body_encoding, "base64");

    // --- event ------------------------------------------------------------
    let events = events.lock().unwrap();
    assert_eq!(events.len(), 3, "webhook-received fires exactly once per request");
    let mut ids: Vec<_> = events.iter().map(|e| e.id.clone()).collect();
    ids.sort();
    ids.dedup();
    assert_eq!(ids.len(), 3, "each event has a unique uuid-v4 id");
    for event in events.iter() {
        assert!(is_uuid(&event.id), "uuid-v4 id: {}", event.id);
        assert!(
            is_iso8601_utc(&event.timestamp),
            "ISO-8601 UTC timestamp: {}",
            event.timestamp
        );
        let json = event.to_json();
        for field in [
            "id",
            "timestamp",
            "method",
            "path",
            "headers",
            "query_params",
            "body",
            "response",
        ] {
            assert!(json.get(field).is_some(), "schema field `{field}` present");
        }
        for field in ["status", "headers", "body", "latency_ms"] {
            assert!(
                json["response"].get(field).is_some(),
                "schema field `response.{field}` present"
            );
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn forwards_without_emitter_and_still_persists() {
    let origin_port = spawn_origin().await;
    let store = RequestStore::in_memory().await.unwrap();
    let interceptor = Interceptor::start(origin_port, store.clone(), None)
        .await
        .unwrap();
    let base = interceptor.info().proxy_url.clone();

    let resp = reqwest::Client::new()
        .post(format!("{base}/no-emitter"))
        .body("payload")
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status().as_u16(), 200);
    interceptor.stop().await.unwrap();

    let all = store.all().await.unwrap();
    assert_eq!(all.len(), 1);
    assert_eq!(all[0].body, b"payload".to_vec());
    let _: &BTreeMap<String, String> = &all[0].headers;
}
