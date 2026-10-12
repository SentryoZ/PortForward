//! SQLite persistence for captured requests — the "store created in MAN-11".
//!
//! The table is defined by `migrations/0001_init.sql` (embedded at compile
//! time), which is also the migration registered with `tauri-plugin-sql` in the
//! Tauri layer. The schema is idempotent, so opening an existing database is
//! safe and both the frontend pool and this pool can point at the same file.
//!
//! Bodies are stored losslessly: UTF-8 payloads go in as TEXT, everything else
//! as a BLOB. Reads always decode raw bytes, so binary payloads round-trip
//! byte-for-byte.

use std::path::Path;
use std::time::Duration;

use sqlx::sqlite::{
    SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions, SqliteSynchronous,
};
use sqlx::{Row, SqlitePool};
use thiserror::Error;

use crate::schema::{CapturedRequest, CapturedResponse};

/// The canonical `requests` schema, shared with `tauri-plugin-sql`.
pub const SCHEMA_SQL: &str = include_str!("../migrations/0001_init.sql");

/// Errors from the capture store.
#[derive(Debug, Error)]
pub enum StoreError {
    /// The underlying SQLite operation failed.
    #[error("sqlite error: {0}")]
    Sqlx(#[from] sqlx::Error),
    /// Creating the database directory failed.
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    /// A stored row could not be decoded back into a [`CapturedRequest`].
    #[error("corrupt stored record: {0}")]
    Decode(String),
}

/// Insert with both body spellings; `COALESCE` keeps the non-NULL one with its
/// original SQLite storage class (TEXT stays TEXT, BLOB stays BLOB).
const INSERT_SQL: &str = r#"
INSERT INTO requests
    (id, timestamp, method, path, headers, query_params, body,
     response_status, response_headers, response_body, response_latency_ms, response_json)
VALUES
    (?1, ?2, ?3, ?4, ?5, ?6, COALESCE(?7, ?8),
     ?9, ?10, COALESCE(?11, ?12), ?13, ?14)
"#;

const SELECT_COLUMNS: &str = "id, timestamp, method, path, headers, query_params, body, \
     response_status, response_headers, response_body, response_latency_ms";

/// A file-backed (or in-memory) SQLite store of captured requests.
#[derive(Clone)]
pub struct RequestStore {
    pool: SqlitePool,
}

/// Split raw bytes into the `(text, blob)` bind pair used by [`INSERT_SQL`].
fn body_args(bytes: &[u8]) -> (Option<&str>, Option<&[u8]>) {
    match std::str::from_utf8(bytes) {
        Ok(text) => (Some(text), None),
        Err(_) => (None, Some(bytes)),
    }
}

impl RequestStore {
    /// Open (creating if needed) the SQLite file at `path` and apply the schema.
    pub async fn open(path: impl AsRef<Path>) -> Result<Self, StoreError> {
        let path = path.as_ref();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let options = SqliteConnectOptions::new()
            .filename(path)
            .create_if_missing(true)
            .journal_mode(SqliteJournalMode::Wal)
            .synchronous(SqliteSynchronous::Normal)
            .busy_timeout(Duration::from_secs(5));
        Self::from_options(options, 4).await
    }

    /// Open a private in-memory database (tests only).
    ///
    /// A single connection is used: each SQLite in-memory connection would
    /// otherwise see its own empty database.
    pub async fn in_memory() -> Result<Self, StoreError> {
        Self::from_options(SqliteConnectOptions::new().in_memory(true), 1).await
    }

    async fn from_options(
        options: SqliteConnectOptions,
        max_connections: u32,
    ) -> Result<Self, StoreError> {
        let pool = SqlitePoolOptions::new()
            .max_connections(max_connections)
            .connect_with(options)
            .await?;
        // `raw_sql` runs the whole multi-statement migration in one shot.
        sqlx::raw_sql(SCHEMA_SQL).execute(&pool).await?;
        Ok(Self { pool })
    }

    /// Persist one captured request.
    pub async fn insert(&self, record: &CapturedRequest) -> Result<(), StoreError> {
        let (req_text, req_blob) = body_args(&record.body);
        let (res_text, res_blob) = body_args(&record.response.body);
        let headers = serde_json::to_string(&record.headers).unwrap_or_else(|_| "{}".into());
        let query_params =
            serde_json::to_string(&record.query_params).unwrap_or_else(|_| "{}".into());
        let response_headers =
            serde_json::to_string(&record.response.headers).unwrap_or_else(|_| "{}".into());
        let response_json =
            serde_json::to_string(&record.to_wire().response).unwrap_or_else(|_| "{}".into());

        sqlx::query(INSERT_SQL)
            .bind(&record.id)
            .bind(&record.timestamp)
            .bind(&record.method)
            .bind(&record.path)
            .bind(headers)
            .bind(query_params)
            .bind(req_text)
            .bind(req_blob)
            .bind(record.response.status as i64)
            .bind(response_headers)
            .bind(res_text)
            .bind(res_blob)
            .bind(record.response.latency_ms as i64)
            .bind(response_json)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    /// Fetch a single record by its uuid, byte-exact.
    pub async fn get(&self, id: &str) -> Result<Option<CapturedRequest>, StoreError> {
        let sql = format!("SELECT {SELECT_COLUMNS} FROM requests WHERE id = ?1");
        let row = sqlx::query(&sql).bind(id).fetch_optional(&self.pool).await?;
        row.map(decode_row).transpose()
    }

    /// Number of stored records.
    pub async fn count(&self) -> Result<i64, StoreError> {
        let row = sqlx::query("SELECT COUNT(*) AS n FROM requests")
            .fetch_one(&self.pool)
            .await?;
        Ok(row.try_get::<i64, _>("n")?)
    }

    /// All stored records in capture order (oldest first). Small helper for
    /// evidence dumps and tests; the paginated list API belongs to MAN-14.
    pub async fn all(&self) -> Result<Vec<CapturedRequest>, StoreError> {
        let sql = format!("SELECT {SELECT_COLUMNS} FROM requests ORDER BY created_at ASC, rowid ASC");
        let rows = sqlx::query(&sql).fetch_all(&self.pool).await?;
        rows.into_iter().map(decode_row).collect()
    }
}

fn decode_row(row: sqlx::sqlite::SqliteRow) -> Result<CapturedRequest, StoreError> {
    let decode = |e: sqlx::Error| StoreError::Decode(e.to_string());
    let headers: String = row.try_get("headers").map_err(decode)?;
    let query_params: String = row.try_get("query_params").map_err(decode)?;
    let response_headers: String = row.try_get("response_headers").map_err(decode)?;
    let response_status: Option<i64> = row.try_get("response_status").map_err(decode)?;
    let response_latency: Option<i64> = row.try_get("response_latency_ms").map_err(decode)?;

    Ok(CapturedRequest {
        id: row.try_get("id").map_err(decode)?,
        timestamp: row.try_get("timestamp").map_err(decode)?,
        method: row.try_get("method").map_err(decode)?,
        path: row.try_get("path").map_err(decode)?,
        headers: serde_json::from_str(&headers).unwrap_or_default(),
        query_params: serde_json::from_str(&query_params).unwrap_or_default(),
        body: row.try_get("body").map_err(decode)?,
        response: CapturedResponse {
            status: response_status.unwrap_or(0) as u16,
            headers: serde_json::from_str(&response_headers).unwrap_or_default(),
            body: row.try_get("response_body").map_err(decode)?,
            latency_ms: response_latency.unwrap_or(0) as u64,
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    fn record(body: Vec<u8>) -> CapturedRequest {
        CapturedRequest {
            id: uuid::Uuid::new_v4().to_string(),
            timestamp: chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
            method: "POST".into(),
            path: "/hooks".into(),
            headers: BTreeMap::from([("content-type".into(), "application/json".into())]),
            query_params: BTreeMap::from([("a".into(), "1".into())]),
            body,
            response: CapturedResponse {
                status: 201,
                headers: BTreeMap::from([("x-marker".into(), "yes".into())]),
                body: b"created".to_vec(),
                latency_ms: 3,
            },
        }
    }

    #[tokio::test]
    async fn round_trips_text_and_binary_bodies() {
        let store = RequestStore::in_memory().await.unwrap();

        let text = record(br#"{"hello":"world"}"#.to_vec());
        let binary: Vec<u8> = (0u8..=255).collect();
        let bin = record(binary.clone());

        store.insert(&text).await.unwrap();
        store.insert(&bin).await.unwrap();
        assert_eq!(store.count().await.unwrap(), 2);

        assert_eq!(store.get(&text.id).await.unwrap().unwrap(), text);
        let got = store.get(&bin.id).await.unwrap().unwrap();
        assert_eq!(got.body, binary, "binary body must be byte-exact");
    }

    #[test]
    fn schema_is_shared_with_tauri_layer() {
        assert!(SCHEMA_SQL.contains("CREATE TABLE IF NOT EXISTS requests"));
    }
}
