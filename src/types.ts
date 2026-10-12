/**
 * Shared wire types.
 *
 * These mirror the Rust types in `interceptor-core::schema` (the MAN-10 §3
 * record shape) and the payloads returned by the Tauri commands. Keep them in
 * sync when the backend contract changes.
 */

/** `start_tunnel` / `tunnel_status` payload. */
export type TunnelInfo = { public_url: string; pid: number; local_port: number };

/** Serializable error returned by every tunnel/interceptor command. */
export type TunnelCommandError = { kind: string; message: string };

/** `start_interceptor` / `interceptor_status` payload. */
export type InterceptorInfo = {
  proxy_port: number;
  proxy_url: string;
  target_port: number;
};

/** The `response{…}` half of the MAN-10 §3 record. */
export type ResponseJson = {
  status: number;
  headers: Record<string, string>;
  body: string;
  /** `"utf8"` (default, omitted on the wire) or `"base64"`. */
  body_encoding?: string;
  latency_ms: number;
};

/** A fully captured request/response pair, as emitted on `webhook-received`. */
export type CapturedRequestJson = {
  id: string;
  timestamp: string;
  method: string;
  path: string;
  headers: Record<string, string>;
  query_params: Record<string, string>;
  body: string;
  /** `"utf8"` (default, omitted) or `"base64"`. */
  body_encoding?: string;
  response: ResponseJson;
};

/** Lightweight row rendered in the live request list. */
export type RequestSummary = {
  id: string;
  timestamp: string;
  method: string;
  path: string;
  status: number;
  latency_ms: number;
};

/** Response returned by `replay_request` (Rust `ResponsePayload`). */
export type ReplayResponse = ResponseJson;

/** Narrow a full capture down to a list row. */
export function toSummary(record: CapturedRequestJson): RequestSummary {
  return {
    id: record.id,
    timestamp: record.timestamp,
    method: record.method,
    path: record.path,
    status: record.response.status,
    latency_ms: record.response.latency_ms,
  };
}
