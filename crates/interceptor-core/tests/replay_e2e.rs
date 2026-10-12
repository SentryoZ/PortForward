//! End-to-end integration test for the MAN-13 replay engine.
//!
//! Proves the MAN-13 sequence against a real local server and the real proxy:
//!
//! 1. a request is captured through the interceptor (the "tunnel" path);
//! 2. `replay` re-sends the stored record **directly** to the local server;
//! 3. the local server receives an identical method / path / query / body and
//!    the preserved end-to-end headers;
//! 4. the replayed request bypasses the interceptor — no new store record and no
//!    new emitter event;
//! 5. the fresh status + latency are returned;
//! 6. an unknown id is a typed [`ReplayError::UnknownRequest`].
//!
//! Binary bodies are covered too, because the schema is byte-exact.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use axum::body::Bytes;
use axum::extract::State;
use axum::http::{HeaderMap, Method, StatusCode, Uri};
use axum::response::Response;
use axum::Router;
use interceptor_core::{replay, CapturedRequest, Interceptor, ReplayError, RequestStore};
use tokio::net::TcpListener;

/// One request as the local server observed it.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Seen {
    method: String,
    path: String,
    query: String,
    content_type: String,
    user_agent: String,
    custom: String,
    body: Vec<u8>,
}

#[derive(Clone)]
struct OriginState {
    seen: Arc<Mutex<Vec<Seen>>>,
}

/// Bind a local origin server on a free port; returns `(port, seen log)`.
async fn spawn_origin() -> (u16, Arc<Mutex<Vec<Seen>>>) {
    let seen: Arc<Mutex<Vec<Seen>>> = Arc::new(Mutex::new(Vec::new()));
    let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let state = OriginState { seen: seen.clone() };
    let app = Router::new().fallback(origin).with_state(state);
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    (port, seen)
}

/// Origin handler: records what it saw, then echoes the body back so the caller
/// can prove the bytes are unchanged. A distinctive status on `/created`.
async fn origin(
    State(state): State<OriginState>,
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
    state.seen.lock().unwrap().push(Seen {
        method: method.as_str().to_string(),
        path: uri.path().to_string(),
        query: uri.query().unwrap_or("").to_string(),
        content_type: header("content-type"),
        user_agent: header("user-agent"),
        custom: header("x-custom-marker"),
        body: body.to_vec(),
    });

    let status = if uri.path() == "/created" {
        StatusCode::CREATED
    } else {
        StatusCode::OK
    };
    Response::builder()
        .status(status)
        .header("content-type", "application/octet-stream")
        .header("x-origin-method", method.as_str())
        .body(axum::body::Body::from(body))
        .unwrap()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn replay_sends_identical_request_directly_and_returns_fresh_response() {
    let (origin_port, seen) = spawn_origin().await;

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

    // 1. Original request through the interceptor (the tunnel path).
    let body = br#"{"id":"evt_replay","type":"charge.succeeded"}"#.to_vec();
    let resp = reqwest::Client::new()
        .post(format!("{base}/api/webhooks/stripe?source=stripe&v=2"))
        .header("content-type", "application/json")
        .header("user-agent", "Stripe/1.0")
        .header("x-custom-marker", "replay-me")
        .body(body.clone())
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status().as_u16(), 200);
    assert_eq!(resp.bytes().await.unwrap().to_vec(), body);

    // The record is persisted and retrievable by id.
    let record = store
        .all()
        .await
        .unwrap()
        .into_iter()
        .next()
        .expect("one captured record");
    let original = seen.lock().unwrap().clone();
    assert_eq!(original.len(), 1, "origin saw the proxied request once");

    // 2. Replay directly to the local server, bypassing the proxy.
    let payload = replay(&store, &record.id, origin_port)
        .await
        .expect("replay succeeds");

    // 3. The local server saw an identical method/path/query/body/headers.
    let observed = seen.lock().unwrap().clone();
    assert_eq!(observed.len(), 2, "replay reached the local server");
    let replayed = observed[1].clone();
    assert_eq!(replayed.method, "POST");
    assert_eq!(replayed.path, "/api/webhooks/stripe");
    assert_eq!(replayed.query, "source=stripe&v=2");
    assert_eq!(replayed.body, body, "body must be byte-identical");
    assert_eq!(replayed.content_type, "application/json");
    assert_eq!(replayed.user_agent, "Stripe/1.0");
    assert_eq!(replayed.custom, "replay-me", "end-to-end headers preserved");
    // The two requests the server saw are identical.
    assert_eq!(
        (&replayed.method, &replayed.path, &replayed.query, &replayed.body),
        (&original[0].method, &original[0].path, &original[0].query, &original[0].body),
        "replay == original request line + body"
    );

    // 4. Replay bypassed the interceptor: no new record, no new event.
    assert_eq!(store.count().await.unwrap(), 1, "replay is not re-captured");
    assert_eq!(events.lock().unwrap().len(), 1, "no extra webhook-received");

    // 5. The fresh status + latency are surfaced.
    assert_eq!(payload.status, 200, "status surfaced");
    assert_eq!(payload.body_bytes(), body, "response body surfaced");
    assert!(payload.latency_ms < 5_000, "latency measured: {}ms", payload.latency_ms);

    interceptor.stop().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn replay_preserves_binary_bodies_and_distinct_status() {
    let (origin_port, seen) = spawn_origin().await;
    let store = RequestStore::in_memory().await.unwrap();
    let interceptor = Interceptor::start(origin_port, store.clone(), None)
        .await
        .unwrap();

    let binary: Vec<u8> = (0u8..=255).collect();
    let resp = reqwest::Client::new()
        .post(format!("{}/created", interceptor.info().proxy_url))
        .header("content-type", "application/octet-stream")
        .body(binary.clone())
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status().as_u16(), 201);

    let record = store.all().await.unwrap().into_iter().next().unwrap();
    let payload = replay(&store, &record.id, origin_port).await.unwrap();

    assert_eq!(payload.status, 201, "distinct upstream status surfaced");
    assert_eq!(payload.body_bytes(), binary, "binary response byte-exact");
    let observed = seen.lock().unwrap().clone();
    assert_eq!(observed.last().unwrap().body, binary, "binary request byte-exact");

    interceptor.stop().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn unknown_id_is_a_typed_error() {
    let (origin_port, _seen) = spawn_origin().await;
    let store = RequestStore::in_memory().await.unwrap();

    let err = replay(&store, "00000000-0000-4000-8000-000000000000", origin_port)
        .await
        .expect_err("unknown id must fail");

    match err {
        ReplayError::UnknownRequest(id) => {
            assert_eq!(id, "00000000-0000-4000-8000-000000000000");
        }
        other => panic!("expected UnknownRequest, got {other:?}"),
    }
}

/// Guard against the query map being ignored: a record with params but no path
/// query still produces a query string on the replayed URL.
#[test]
fn replay_url_is_built_from_path_and_query_params() {
    let record = CapturedRequest {
        id: "id".into(),
        timestamp: "2026-01-01T00:00:00.000Z".into(),
        method: "GET".into(),
        path: "/hook".into(),
        headers: BTreeMap::new(),
        query_params: BTreeMap::from([("a".into(), "1".into())]),
        body: Vec::new(),
        response: interceptor_core::CapturedResponse {
            status: 200,
            headers: BTreeMap::new(),
            body: Vec::new(),
            latency_ms: 0,
        },
    };
    assert_eq!(
        interceptor_core::replay_url(3000, &record),
        "http://localhost:3000/hook?a=1"
    );
}
