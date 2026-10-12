//! The replay engine (MAN-13, MVP step 3).
//!
//! Loads a captured request from the [`RequestStore`] and re-issues it with
//! `reqwest` **directly** to the user's local server at
//! `http://localhost:<target_port><path>` — bypassing the interceptor proxy and
//! the public tunnel entirely.
//!
//! The bytes sent are the ones that were captured: the same method, the same
//! path + query params, the same body (text or binary, byte-exact) and the same
//! end-to-end headers. Only hop-by-hop headers (`host`, `content-length`,
//! `connection`, …) are dropped, because the local HTTP client re-derives them —
//! exactly what the proxy already does when it forwards the first time.
//!
//! A replay is *not* captured: it deliberately does not pass through the
//! interceptor, so it does not create a new record or a new `webhook-received`
//! event.

use std::collections::BTreeMap;
use std::time::Instant;

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::proxy::is_hop_by_hop;
use crate::schema::{encode_body, CapturedRequest, BODY_ENCODING_UTF8};
use crate::store::{RequestStore, StoreError};

/// Errors raised while replaying a captured request.
#[derive(Debug, Error)]
pub enum ReplayError {
    /// No stored record exists for the requested id.
    #[error("no captured request with id `{0}`")]
    UnknownRequest(String),
    /// The stored method is not a valid HTTP method.
    #[error("stored record has an invalid method: {0}")]
    InvalidMethod(String),
    /// A stored header name or value is not valid for an HTTP request.
    #[error("stored record has an invalid header: {0}")]
    InvalidHeader(String),
    /// `reqwest` could not reach the local server (or read its response).
    #[error("replay request to the local server failed: {0}")]
    Request(String),
    /// The capture store could not be read.
    #[error(transparent)]
    Store(#[from] StoreError),
}

/// The fresh response produced by a replay.
///
/// Mirrors the `response{…}` half of the MAN-10 §3 schema: `body` is the decoded
/// UTF-8 string for text responses, or base64 with `body_encoding: "base64"` for
/// binary responses (same convention as [`crate::schema::ResponseJson`]).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResponsePayload {
    /// Status code returned by the local server.
    pub status: u16,
    /// Response headers (lower-cased names).
    pub headers: BTreeMap<String, String>,
    /// Response body (see [`ResponsePayload::body_encoding`]).
    pub body: String,
    /// `utf8` (default, omitted on the wire) or `base64`.
    #[serde(default = "default_utf8", skip_serializing_if = "is_utf8")]
    pub body_encoding: String,
    /// Round-trip time to the local server, in milliseconds.
    pub latency_ms: u64,
}

fn default_utf8() -> String {
    BODY_ENCODING_UTF8.to_string()
}

fn is_utf8(encoding: &str) -> bool {
    encoding == BODY_ENCODING_UTF8
}

impl ResponsePayload {
    /// Build a payload from raw response bytes, applying the wire encoding.
    pub fn from_bytes(
        status: u16,
        headers: BTreeMap<String, String>,
        body: &[u8],
        latency_ms: u64,
    ) -> Self {
        let (body_encoding, body) = encode_body(body);
        Self {
            status,
            headers,
            body,
            body_encoding: body_encoding.to_string(),
            latency_ms,
        }
    }

    /// Recover the raw response bytes (inverse of the wire encoding).
    pub fn body_bytes(&self) -> Vec<u8> {
        crate::schema::decode_body(&self.body_encoding, &self.body)
    }
}

/// Build the direct-to-local-server URL for a captured record:
/// `http://localhost:<target_port><path>[?<query_params>]`.
///
/// The record stores the path without its query string, so the query params are
/// re-serialized (deterministically, since `query_params` is a `BTreeMap`).
pub fn replay_url(target_port: u16, record: &CapturedRequest) -> String {
    let mut url = format!("http://localhost:{target_port}{}", record.path);
    if !record.query_params.is_empty() {
        let mut serializer = url::form_urlencoded::Serializer::new(String::new());
        for (key, value) in &record.query_params {
            serializer.append_pair(key, value);
        }
        url.push('?');
        url.push_str(&serializer.finish());
    }
    url
}

/// Comma-join response headers into the schema's `String -> String` map.
fn response_headers(headers: &reqwest::header::HeaderMap) -> BTreeMap<String, String> {
    let mut map: BTreeMap<String, String> = BTreeMap::new();
    for (name, value) in headers {
        let text = String::from_utf8_lossy(value.as_bytes()).into_owned();
        match map.get_mut(name.as_str()) {
            Some(existing) => {
                existing.push_str(", ");
                existing.push_str(&text);
            }
            None => {
                map.insert(name.as_str().to_string(), text);
            }
        }
    }
    map
}

/// Replay a stored request against the local server on `target_port`.
///
/// Returns the local server's fresh [`ResponsePayload`] (status, headers, body,
/// latency). Unknown ids yield [`ReplayError::UnknownRequest`].
pub async fn replay(
    store: &RequestStore,
    request_id: &str,
    target_port: u16,
) -> Result<ResponsePayload, ReplayError> {
    // Mirror the proxy's client: do not follow redirects, so the local server's
    // response comes back unaltered.
    let client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .map_err(|err| ReplayError::Request(err.to_string()))?;
    replay_with_client(&client, store, request_id, target_port).await
}

/// [`replay`] with a caller-supplied `reqwest::Client` (redirect policy, pool,
/// timeouts). Used by the integration test so replay matches the proxy's client
/// behaviour.
pub async fn replay_with_client(
    client: &reqwest::Client,
    store: &RequestStore,
    request_id: &str,
    target_port: u16,
) -> Result<ResponsePayload, ReplayError> {
    let record = store
        .get(request_id)
        .await?
        .ok_or_else(|| ReplayError::UnknownRequest(request_id.to_string()))?;

    let method = reqwest::Method::from_bytes(record.method.as_bytes()).map_err(|err| {
        ReplayError::InvalidMethod(format!("{} ({err})", record.method))
    })?;

    let mut headers = reqwest::header::HeaderMap::new();
    for (name, value) in &record.headers {
        if is_hop_by_hop(name) {
            continue;
        }
        let name = reqwest::header::HeaderName::from_bytes(name.as_bytes())
            .map_err(|err| ReplayError::InvalidHeader(format!("`{name}` ({err})")))?;
        let value = reqwest::header::HeaderValue::from_bytes(value.as_bytes())
            .map_err(|err| ReplayError::InvalidHeader(format!("`{name}: {value}` ({err})")))?;
        headers.append(name, value);
    }

    let url = replay_url(target_port, &record);
    let started = Instant::now();
    let response = client
        .request(method, &url)
        .headers(headers)
        .body(record.body.clone())
        .send()
        .await
        .map_err(|err| ReplayError::Request(err.to_string()))?;

    let status = response.status().as_u16();
    let headers = response_headers(response.headers());
    let bytes = response
        .bytes()
        .await
        .map_err(|err| ReplayError::Request(err.to_string()))?;
    let latency_ms = started.elapsed().as_millis() as u64;

    Ok(ResponsePayload::from_bytes(
        status, headers, &bytes, latency_ms,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record() -> CapturedRequest {
        CapturedRequest {
            id: uuid::Uuid::new_v4().to_string(),
            timestamp: chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
            method: "POST".into(),
            path: "/api/webhooks/stripe".into(),
            headers: BTreeMap::from([("content-type".into(), "application/json".into())]),
            query_params: BTreeMap::from([("source".into(), "stripe".into()), ("v".into(), "2".into())]),
            body: b"{}".to_vec(),
            response: crate::schema::CapturedResponse {
                status: 200,
                headers: BTreeMap::new(),
                body: b"ok".to_vec(),
                latency_ms: 1,
            },
        }
    }

    #[test]
    fn replay_url_includes_port_path_and_query() {
        let url = replay_url(3000, &record());
        assert_eq!(url, "http://localhost:3000/api/webhooks/stripe?source=stripe&v=2");
    }

    #[test]
    fn replay_url_omits_empty_query() {
        let mut rec = record();
        rec.query_params.clear();
        assert_eq!(replay_url(8080, &rec), "http://localhost:8080/api/webhooks/stripe");
    }

    #[test]
    fn response_payload_round_trips_binary_bodies() {
        let bytes: Vec<u8> = (0u8..=255).collect();
        let payload = ResponsePayload::from_bytes(200, BTreeMap::new(), &bytes, 4);
        assert_eq!(payload.body_encoding, "base64");
        assert_eq!(payload.body_bytes(), bytes);
    }

    #[test]
    fn response_payload_omits_encoding_for_utf8() {
        let payload = ResponsePayload::from_bytes(201, BTreeMap::new(), b"created", 2);
        let json = serde_json::to_value(&payload).unwrap();
        assert_eq!(json["status"], 201);
        assert_eq!(json["latency_ms"], 2);
        assert!(json.get("body_encoding").is_none(), "utf8 omits the field");
    }
}
