//! Capture record types — a byte-exact Rust representation of the MAN-10 §3
//! JSON schema, plus the wire (JSON) projection emitted to the UI.
//!
//! The Rust types keep payloads as `Vec<u8>` so nothing is lost for non-UTF-8
//! (binary) bodies. [`CapturedRequest::to_wire`] renders the §3 shape: for a
//! UTF-8 payload `body` is the decoded string and no `body_encoding` field is
//! emitted; for a binary payload `body` is base64 and `body_encoding:
//! "base64"` is added so consumers can tell the difference unambiguously.
//! Every required §3 field is always present with its exact name and type.

use std::collections::BTreeMap;

use base64::engine::general_purpose::STANDARD as BASE64;
use base64::Engine as _;
use serde::{Deserialize, Serialize};

/// `body_encoding` value for payloads stored as a plain UTF-8 JSON string.
pub const BODY_ENCODING_UTF8: &str = "utf8";
/// `body_encoding` value for payloads stored as base64 (non-UTF-8 bytes).
pub const BODY_ENCODING_BASE64: &str = "base64";

fn default_utf8() -> String {
    BODY_ENCODING_UTF8.to_string()
}

fn is_utf8(enc: &str) -> bool {
    enc == BODY_ENCODING_UTF8
}

/// Encode raw bytes as `(encoding, value)` for the JSON wire format.
pub(crate) fn encode_body(bytes: &[u8]) -> (&'static str, String) {
    match std::str::from_utf8(bytes) {
        Ok(text) => (BODY_ENCODING_UTF8, text.to_string()),
        Err(_) => (BODY_ENCODING_BASE64, BASE64.encode(bytes)),
    }
}

/// Decode a wire `(encoding, value)` pair back to raw bytes.
pub(crate) fn decode_body(encoding: &str, value: &str) -> Vec<u8> {
    if encoding == BODY_ENCODING_BASE64 {
        BASE64
            .decode(value)
            .unwrap_or_else(|_| value.as_bytes().to_vec())
    } else {
        value.as_bytes().to_vec()
    }
}

/// The `response{...}` half of a captured request (MAN-10 §3), raw bytes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CapturedResponse {
    /// Upstream HTTP status code.
    pub status: u16,
    /// Upstream response headers (lower-cased names).
    pub headers: BTreeMap<String, String>,
    /// Upstream response body, byte-exact.
    pub body: Vec<u8>,
    /// Round-trip time to the upstream, in milliseconds.
    pub latency_ms: u64,
}

/// A fully captured request/response pair.
///
/// This is the authoritative in-process representation (payloads as bytes) and
/// is what the proxy hands to the store and the Tauri emitter.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CapturedRequest {
    /// uuid-v4.
    pub id: String,
    /// ISO-8601 UTC (millisecond precision, `Z` suffix).
    pub timestamp: String,
    /// Uppercase HTTP method, e.g. `POST`.
    pub method: String,
    /// Path only, no query string, e.g. `/api/webhooks/stripe`.
    pub path: String,
    /// Request headers (lower-cased names).
    pub headers: BTreeMap<String, String>,
    /// Query string parsed into a map.
    pub query_params: BTreeMap<String, String>,
    /// Raw request body, byte-exact.
    pub body: Vec<u8>,
    /// Upstream response.
    pub response: CapturedResponse,
}

impl CapturedRequest {
    /// Project this record into the MAN-10 §3 JSON wire shape.
    pub fn to_wire(&self) -> CapturedRequestJson {
        let (body_encoding, body) = encode_body(&self.body);
        let (response_body_encoding, response_body) = encode_body(&self.response.body);
        CapturedRequestJson {
            id: self.id.clone(),
            timestamp: self.timestamp.clone(),
            method: self.method.clone(),
            path: self.path.clone(),
            headers: self.headers.clone(),
            query_params: self.query_params.clone(),
            body,
            body_encoding: body_encoding.to_string(),
            response: ResponseJson {
                status: self.response.status,
                headers: self.response.headers.clone(),
                body: response_body,
                body_encoding: response_body_encoding.to_string(),
                latency_ms: self.response.latency_ms,
            },
        }
    }

    /// Serialize the wire shape to a `serde_json::Value`.
    pub fn to_json(&self) -> serde_json::Value {
        serde_json::to_value(self.to_wire()).expect("CapturedRequest is always serializable")
    }
}

/// The `response` object exactly as it appears in MAN-10 §3.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResponseJson {
    /// Upstream HTTP status code.
    pub status: u16,
    /// Upstream response headers.
    pub headers: BTreeMap<String, String>,
    /// Response body (see [`CapturedRequestJson::body_encoding`]).
    pub body: String,
    /// `utf8` (default, omitted) or `base64`.
    #[serde(default = "default_utf8", skip_serializing_if = "is_utf8")]
    pub body_encoding: String,
    /// Round-trip time to the upstream, in milliseconds.
    pub latency_ms: u64,
}

/// The MAN-10 §3 record as emitted to the UI on the `webhook-received` event.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CapturedRequestJson {
    /// uuid-v4.
    pub id: String,
    /// ISO-8601 UTC.
    pub timestamp: String,
    /// Uppercase HTTP method.
    pub method: String,
    /// Path only.
    pub path: String,
    /// Request headers.
    pub headers: BTreeMap<String, String>,
    /// Query parameters.
    pub query_params: BTreeMap<String, String>,
    /// Request body (see `body_encoding`).
    pub body: String,
    /// `utf8` (default, omitted) or `base64`.
    #[serde(default = "default_utf8", skip_serializing_if = "is_utf8")]
    pub body_encoding: String,
    /// Upstream response.
    pub response: ResponseJson,
}

impl CapturedRequestJson {
    /// Recover the raw request bytes (inverse of [`CapturedRequest::to_wire`]).
    pub fn request_body_bytes(&self) -> Vec<u8> {
        decode_body(&self.body_encoding, &self.body)
    }

    /// Recover the raw response bytes.
    pub fn response_body_bytes(&self) -> Vec<u8> {
        decode_body(&self.response.body_encoding, &self.response.body)
    }
}

/// Validate a TIMESTAMP string is ISO-8601 UTC (`...Z`).
pub fn is_iso8601_utc(timestamp: &str) -> bool {
    chrono::DateTime::parse_from_rfc3339(timestamp)
        .map(|dt| dt.offset().local_minus_utc() == 0)
        .unwrap_or(false)
}

/// True when `id` parses as a uuid, i.e. uuid-v4 string form.
pub fn is_uuid(id: &str) -> bool {
    uuid::Uuid::parse_str(id).is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample(body: Vec<u8>) -> CapturedRequest {
        CapturedRequest {
            id: uuid::Uuid::new_v4().to_string(),
            timestamp: chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
            method: "POST".into(),
            path: "/api/webhooks/stripe".into(),
            headers: BTreeMap::from([("content-type".into(), "application/json".into())]),
            query_params: BTreeMap::from([("v".into(), "2".into())]),
            body,
            response: CapturedResponse {
                status: 200,
                headers: BTreeMap::from([("content-type".into(), "text/plain".into())]),
                body: b"OK".to_vec(),
                latency_ms: 7,
            },
        }
    }

    #[test]
    fn utf8_body_matches_schema_and_omits_encoding() {
        let rec = sample(b"{\"id\":\"evt_1\"}".to_vec());
        let json = serde_json::to_value(rec.to_wire()).unwrap();
        assert_eq!(json["body"], "{\"id\":\"evt_1\"}");
        assert!(json.get("body_encoding").is_none(), "utf8 omits the field");
        assert_eq!(json["response"]["status"], 200);
        assert_eq!(json["response"]["latency_ms"], 7);
        assert!(is_uuid(json["id"].as_str().unwrap()));
        assert!(is_iso8601_utc(json["timestamp"].as_str().unwrap()));
    }

    #[test]
    fn binary_body_is_base64_and_round_trips() {
        let bytes: Vec<u8> = (0u8..=255).collect();
        let rec = sample(bytes.clone());
        let wire = rec.to_wire();
        assert_eq!(wire.body_encoding, BODY_ENCODING_BASE64);
        assert_eq!(wire.request_body_bytes(), bytes, "binary body must not corrupt");
    }
}
