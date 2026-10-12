//! The interceptor proxy: a local `axum` server that forwards every request to
//! the user's target port, captures it, and returns the upstream response
//! completely unaltered.
//!
//! Bound to `127.0.0.1:<free port>` so it can sit between the cloudflared quick
//! tunnel and the local application server. For each request it:
//!
//! 1. forwards method, path, query, headers and the raw body to
//!    `http://localhost:<target_port>`, timing the round-trip;
//! 2. builds a [`CapturedRequest`] (MAN-10 §3) with uuid-v4 `id` and ISO-8601
//!    UTC `timestamp`;
//! 3. persists it to the [`RequestStore`];
//! 4. fires the caller's emitter **exactly once** so the UI can show it live;
//! 5. returns the upstream status/headers/body unchanged.
//!
//! Headers are carried as `(lower-cased name, raw value bytes)` pairs so no
//! header value is corrupted in the process.

use std::collections::BTreeMap;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Instant;

use axum::body::{to_bytes, Body, Bytes};
use axum::extract::{Request, State};
use axum::http::{HeaderName, HeaderValue, StatusCode};
use axum::response::Response;
use axum::Router;
use serde::{Deserialize, Serialize};
use thiserror::Error;
use tokio::net::TcpListener;
use tokio::sync::oneshot;
use tokio::task::JoinHandle;

use crate::schema::{CapturedRequest, CapturedResponse};
use crate::store::{RequestStore, StoreError};

/// Callback invoked once per captured request (Tauri `emit`, a log sink, ...).
pub type Emitter = Arc<dyn Fn(&CapturedRequest) + Send + Sync>;

/// `(lower-cased name, raw value bytes)` header pairs.
type RawHeaders = Vec<(String, Vec<u8>)>;

/// Errors raised while starting or stopping the proxy.
#[derive(Debug, Error)]
pub enum ProxyError {
    /// The listener could not bind (e.g. the port was taken).
    #[error("failed to bind interceptor listener: {0}")]
    Bind(#[from] std::io::Error),
    /// The capture store could not be opened.
    #[error(transparent)]
    Store(#[from] StoreError),
}

/// Public handle information for a running interceptor.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InterceptorInfo {
    /// The free port the proxy was bound to.
    pub proxy_port: u16,
    /// Convenience URL, `http://127.0.0.1:<proxy_port>`.
    pub proxy_url: String,
    /// The upstream port requests are forwarded to.
    pub target_port: u16,
}

/// Shared state for one proxy instance.
struct ProxyState {
    target_port: u16,
    client: reqwest::Client,
    store: RequestStore,
    emitter: Option<Emitter>,
}

/// A running interceptor proxy. Always call [`Interceptor::stop`] to shut it
/// down cleanly.
pub struct Interceptor {
    info: InterceptorInfo,
    shutdown: Option<oneshot::Sender<()>>,
    join: Option<JoinHandle<()>>,
}

impl Interceptor {
    /// Bind `127.0.0.1:0` (a free port), then serve in the background.
    pub async fn start(
        target_port: u16,
        store: RequestStore,
        emitter: Option<Emitter>,
    ) -> Result<Self, ProxyError> {
        let listener = TcpListener::bind(("127.0.0.1", 0)).await?;
        let addr = listener.local_addr()?;
        Self::serve_on(listener, addr, target_port, store, emitter).await
    }

    /// Bind a specific `127.0.0.1:port` (used by tests) and serve in the
    /// background.
    pub async fn start_on(
        port: u16,
        target_port: u16,
        store: RequestStore,
        emitter: Option<Emitter>,
    ) -> Result<Self, ProxyError> {
        let listener = TcpListener::bind(("127.0.0.1", port)).await?;
        let addr = listener.local_addr()?;
        Self::serve_on(listener, addr, target_port, store, emitter).await
    }

    async fn serve_on(
        listener: TcpListener,
        addr: SocketAddr,
        target_port: u16,
        store: RequestStore,
        emitter: Option<Emitter>,
    ) -> Result<Self, ProxyError> {
        let state = Arc::new(ProxyState {
            target_port,
            client: reqwest::Client::builder()
                .redirect(reqwest::redirect::Policy::none())
                .build()
                .map_err(std::io::Error::other)?,
            store,
            emitter,
        });

        let app = Router::new().fallback(handle).with_state(state);
        let (tx, rx) = oneshot::channel::<()>();
        let join = tokio::spawn(async move {
            let _ = axum::serve(listener, app)
                .with_graceful_shutdown(async move {
                    let _ = rx.await;
                })
                .await;
        });

        Ok(Self {
            info: InterceptorInfo {
                proxy_port: addr.port(),
                proxy_url: format!("http://127.0.0.1:{}", addr.port()),
                target_port,
            },
            shutdown: Some(tx),
            join: Some(join),
        })
    }

    /// Info for the running proxy.
    pub fn info(&self) -> &InterceptorInfo {
        &self.info
    }

    /// Stop accepting connections and wait for the server task to finish.
    pub async fn stop(mut self) -> Result<(), ProxyError> {
        if let Some(tx) = self.shutdown.take() {
            let _ = tx.send(());
        }
        if let Some(join) = self.join.take() {
            let _ = tokio::time::timeout(std::time::Duration::from_secs(5), join).await;
        }
        Ok(())
    }
}

/// Hop-by-hop / connection-management headers that must not be blindly copied
/// between the inbound connection and the upstream connection.
pub(crate) fn is_hop_by_hop(name: &str) -> bool {
    matches!(
        name,
        "host"
            | "connection"
            | "keep-alive"
            | "proxy-authenticate"
            | "proxy-authorization"
            | "proxy-connection"
            | "te"
            | "trailer"
            | "transfer-encoding"
            | "upgrade"
            | "content-length"
    )
}

/// Lower-case a header name and keep its value bytes.
fn raw_headers(pairs: impl IntoIterator<Item = (String, Vec<u8>)>) -> RawHeaders {
    pairs
        .into_iter()
        .map(|(name, value)| (name.to_ascii_lowercase(), value))
        .collect()
}

/// Project raw header pairs into the schema's `String -> String` map,
/// comma-joining repeated names.
fn headers_to_map(headers: &RawHeaders) -> BTreeMap<String, String> {
    let mut map: BTreeMap<String, String> = BTreeMap::new();
    for (name, value) in headers {
        let text = String::from_utf8_lossy(value).into_owned();
        match map.get_mut(name) {
            Some(existing) => {
                existing.push_str(", ");
                existing.push_str(&text);
            }
            None => {
                map.insert(name.clone(), text);
            }
        }
    }
    map
}

/// Parse a raw query string into the schema's `query_params` map.
fn capture_query(query: Option<&str>) -> BTreeMap<String, String> {
    let mut map = BTreeMap::new();
    if let Some(query) = query {
        for (k, v) in url::form_urlencoded::parse(query.as_bytes()) {
            // First occurrence wins; keeps the mapping deterministic.
            map.entry(k.into_owned()).or_insert_with(|| v.into_owned());
        }
    }
    map
}

/// The catch-all proxy handler.
async fn handle(State(state): State<Arc<ProxyState>>, req: Request) -> Response {
    let method = req.method().clone();
    let uri = req.uri().clone();
    let path = uri.path().to_string();
    let query_params = capture_query(uri.query());
    let request_headers: RawHeaders = raw_headers(
        req.headers()
            .iter()
            .map(|(name, value)| (name.as_str().to_string(), value.as_bytes().to_vec())),
    );
    let path_and_query = uri
        .path_and_query()
        .map(|pq| pq.as_str().to_string())
        .unwrap_or_else(|| path.clone());

    let body = match to_bytes(req.into_body(), usize::MAX).await {
        Ok(bytes) => bytes,
        Err(err) => {
            return simple_response(
                StatusCode::BAD_REQUEST,
                format!("interceptor: failed to read request body: {err}"),
            )
        }
    };

    let url = format!("http://localhost:{}{}", state.target_port, path_and_query);
    let started = Instant::now();

    let captured_response = match forward(&state, &method, &url, &request_headers, &body).await {
        Ok((status, headers, bytes)) => CapturedResponse {
            status,
            headers: headers_to_map(&headers),
            body: bytes.to_vec(),
            latency_ms: 0,
        },
        Err(err) => {
            let message = format!("interceptor: upstream request failed: {err}");
            let mut headers = BTreeMap::new();
            headers.insert(
                "content-type".to_string(),
                "text/plain; charset=utf-8".into(),
            );
            CapturedResponse {
                status: 502,
                headers,
                body: message.into_bytes(),
                latency_ms: 0,
            }
        }
    };
    let latency_ms = started.elapsed().as_millis() as u64;
    let response = build_response(&captured_response);

    let record = CapturedRequest {
        id: uuid::Uuid::new_v4().to_string(),
        timestamp: chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
        method: method.as_str().to_string(),
        path,
        headers: headers_to_map(&request_headers),
        query_params,
        body: body.to_vec(),
        response: CapturedResponse {
            latency_ms,
            ..captured_response
        },
    };

    // Persist, then emit exactly once. A persistence failure is logged but must
    // not break the proxied response.
    if let Err(err) = state.store.insert(&record).await {
        eprintln!(
            "[interceptor] failed to persist request {}: {err}",
            record.id
        );
    }
    if let Some(emitter) = state.emitter.as_ref() {
        emitter(&record);
    }

    response
}

/// Forward the request upstream and return `(status, headers, body)`.
async fn forward(
    state: &ProxyState,
    method: &axum::http::Method,
    url: &str,
    headers: &RawHeaders,
    body: &[u8],
) -> Result<(u16, RawHeaders, Bytes), String> {
    let method = reqwest::Method::from_bytes(method.as_str().as_bytes())
        .map_err(|e| format!("invalid method: {e}"))?;

    let mut upstream_headers = reqwest::header::HeaderMap::new();
    for (name, value) in headers {
        if is_hop_by_hop(name) {
            continue;
        }
        let name = HeaderName::from_bytes(name.as_bytes()).map_err(|e| e.to_string())?;
        let value = HeaderValue::from_bytes(value).map_err(|e| e.to_string())?;
        upstream_headers.append(name, value);
    }

    let response = state
        .client
        .request(method, url)
        .headers(upstream_headers)
        .body(body.to_vec())
        .send()
        .await
        .map_err(|e| e.to_string())?;

    let status = response.status().as_u16();
    let headers = raw_headers(
        response
            .headers()
            .iter()
            .map(|(name, value)| (name.as_str().to_string(), value.as_bytes().to_vec())),
    );
    let bytes = response.bytes().await.map_err(|e| e.to_string())?;
    Ok((status, headers, bytes))
}

/// Rebuild the captured response so the client sees status/headers/body
/// unchanged (minus hop-by-hop headers, which the local connection re-derives).
fn build_response(captured: &CapturedResponse) -> Response {
    let mut builder =
        Response::builder().status(StatusCode::from_u16(captured.status).unwrap_or(StatusCode::BAD_GATEWAY));
    if let Some(map) = builder.headers_mut() {
        for (name, value) in &captured.headers {
            if is_hop_by_hop(name) {
                continue;
            }
            if let (Ok(n), Ok(v)) = (
                HeaderName::from_bytes(name.as_bytes()),
                HeaderValue::from_bytes(value.as_bytes()),
            ) {
                map.append(n, v);
            }
        }
    }
    builder
        .body(Body::from(captured.body.clone()))
        .unwrap_or_else(|_| {
            simple_response(StatusCode::BAD_GATEWAY, "interceptor: bad response".into())
        })
}

/// A tiny text response used for interceptor-generated errors.
fn simple_response(status: StatusCode, message: String) -> Response {
    Response::builder()
        .status(status)
        .header("content-type", "text/plain; charset=utf-8")
        .body(Body::from(message))
        .expect("static response is valid")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn query_and_headers_are_normalised() {
        let q = capture_query(Some("a=1&b=two&a=ignored"));
        assert_eq!(q.get("a").map(String::as_str), Some("1"));
        assert_eq!(q.get("b").map(String::as_str), Some("two"));

        let headers: RawHeaders = vec![
            ("X-Test".into(), b"one".to_vec()),
            ("x-test".into(), b"two".to_vec()),
        ];
        let captured = headers_to_map(&raw_headers(headers));
        assert_eq!(captured.get("x-test").map(String::as_str), Some("one, two"));
    }

    #[test]
    fn hop_by_hop_is_rejected() {
        assert!(is_hop_by_hop("content-length"));
        assert!(is_hop_by_hop("transfer-encoding"));
        assert!(!is_hop_by_hop("content-type"));
    }
}
