-- Port Forward: captured request persistence (MAN-10 §3 schema).
--
-- This file is the single source of truth for the `requests` table. It is
-- embedded by `interceptor-core` (which writes captured records) and by the
-- Tauri layer (`src-tauri/src/lib.rs`), which registers it as the
-- `tauri-plugin-sql` migration for the frontend connection. Because it is
-- idempotent (`IF NOT EXISTS`) both entry points can apply it safely.
--
-- The nested `response` object from the JSON schema is flattened into
-- `response_*` columns so the table stays queryable in plain SQLite. A
-- `response_json` column keeps the original nested shape for exact round-trips.
--
-- headers / query_params are JSON-encoded TEXT (SQLite has no map type).
--
-- `body` / `response_body` carry the raw payload. UTF-8 payloads are stored as
-- TEXT; non-UTF-8 (binary) payloads are stored as BLOBs, which SQLite keeps
-- byte-exact. Readers decode both as raw bytes, so no payload is ever lost.

CREATE TABLE IF NOT EXISTS requests (
    id                  TEXT    PRIMARY KEY,           -- uuid-v4
    timestamp           TEXT    NOT NULL,              -- ISO-8601 UTC
    method              TEXT    NOT NULL,              -- POST
    path                TEXT    NOT NULL,              -- /api/webhooks/stripe
    headers             TEXT    NOT NULL DEFAULT '{}', -- JSON object
    query_params        TEXT    NOT NULL DEFAULT '{}', -- JSON object
    body                TEXT    NOT NULL DEFAULT '',   -- raw payload (TEXT or BLOB)

    -- flattened `response{...}`
    response_status     INTEGER,                       -- 200
    response_headers    TEXT,                          -- JSON object
    response_body       TEXT,                          -- OK
    response_latency_ms INTEGER,                       -- 42
    response_json       TEXT,                          -- original nested object

    created_at          TEXT    NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now'))
);

CREATE INDEX IF NOT EXISTS idx_requests_timestamp ON requests (timestamp DESC);
CREATE INDEX IF NOT EXISTS idx_requests_method    ON requests (method);
CREATE INDEX IF NOT EXISTS idx_requests_path      ON requests (path);
