/**
 * Presentation helpers shared by the list and the detail panel.
 */

import type { CapturedRequestJson, ResponseJson } from "../types";

/** `HH:MM:SS.mmm` in the viewer's local time zone (falls back to the raw value). */
export function formatTime(iso: string): string {
  const date = new Date(iso);
  if (Number.isNaN(date.getTime())) return iso;
  const pad = (n: number, width = 2) => String(n).padStart(width, "0");
  return (
    `${pad(date.getHours())}:${pad(date.getMinutes())}:${pad(date.getSeconds())}` +
    `.${pad(date.getMilliseconds(), 3)}`
  );
}

/** Tailwind text colour for an HTTP status code. */
export function statusClass(status: number): string {
  if (status >= 500) return "text-rose-400";
  if (status >= 400) return "text-amber-400";
  if (status >= 300) return "text-sky-400";
  if (status >= 200) return "text-emerald-400";
  return "text-slate-400";
}

/** Tailwind text colour for an HTTP method. */
export function methodClass(method: string): string {
  switch (method.toUpperCase()) {
    case "GET":
      return "text-emerald-400";
    case "POST":
      return "text-sky-400";
    case "PUT":
    case "PATCH":
      return "text-amber-400";
    case "DELETE":
      return "text-rose-400";
    default:
      return "text-slate-300";
  }
}

/** Decoded payload text plus whether it was a binary (base64) body. */
export type DecodedBody = { text: string; binary: boolean; bytes?: Uint8Array };

/** Decode a wire body (`body` + optional `body_encoding`). */
export function decodeBody(part: { body: string; body_encoding?: string }): DecodedBody {
  if (part.body_encoding === "base64") {
    return { text: part.body, binary: true, bytes: base64ToBytes(part.body) };
  }
  return { text: part.body, binary: false };
}

/** Convenience wrapper for a captured request's request-side body. */
export function requestBody(record: CapturedRequestJson): DecodedBody {
  return decodeBody(record);
}

/** Convenience wrapper for a response body. */
export function responseBody(response: ResponseJson): DecodedBody {
  return decodeBody(response);
}

function base64ToBytes(base64: string): Uint8Array | undefined {
  try {
    const binary = atob(base64);
    const bytes = new Uint8Array(binary.length);
    for (let i = 0; i < binary.length; i += 1) bytes[i] = binary.charCodeAt(i);
    return bytes;
  } catch {
    return undefined;
  }
}

export type ParsedJson =
  | { ok: true; value: unknown }
  | { ok: false; reason: string };

/**
 * Parse a payload as JSON without ever throwing.
 *
 * The detail panel renders the parsed tree when `ok`, and falls back to the raw
 * text otherwise — so a non-JSON body (form data, plain text, base64 binary)
 * never crashes the viewer.
 */
export function tryParseJson(text: string): ParsedJson {
  const trimmed = text.trim();
  if (trimmed === "") return { ok: false, reason: "empty body" };
  try {
    return { ok: true, value: JSON.parse(trimmed) };
  } catch (err) {
    return { ok: false, reason: err instanceof Error ? err.message : String(err) };
  }
}

/** Byte length of a decoded payload. */
export function byteLength(decoded: DecodedBody): number {
  return decoded.bytes ? decoded.bytes.length : new TextEncoder().encode(decoded.text).length;
}
