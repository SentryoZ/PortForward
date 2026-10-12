/**
 * The real host bridge: Tauri IPC, Tauri events and the SQLite-backed request
 * store exposed through `tauri-plugin-sql`.
 */

import { invoke as tauriInvoke } from "@tauri-apps/api/core";
import { listen as tauriListen } from "@tauri-apps/api/event";
import Database from "@tauri-apps/plugin-sql";

import type { RequestSummary } from "../types";
import type { Bridge, Unlisten } from "./bridge";

/** Database URL used by both the Rust side and `tauri-plugin-sql`. */
export const DB_URL = "sqlite:portforward.db";

/** True when the page is running inside a real Tauri webview. */
export function isTauri(): boolean {
  return typeof window !== "undefined" && "__TAURI_INTERNALS__" in window;
}

type RequestRow = {
  id: string;
  timestamp: string;
  method: string;
  path: string;
  response_status: number;
  response_latency_ms: number;
};

const LIST_SQL =
  "SELECT id, timestamp, method, path, response_status, response_latency_ms " +
  "FROM requests ORDER BY timestamp DESC LIMIT 500";

export const tauriBridge: Bridge = {
  invoke<T>(cmd: string, args?: Record<string, unknown>): Promise<T> {
    return tauriInvoke<T>(cmd, args);
  },

  listen<T>(event: string, handler: (payload: T) => void): Promise<Unlisten> {
    return tauriListen<T>(event, (e) => handler(e.payload));
  },

  async listRequests(): Promise<RequestSummary[]> {
    const db = await Database.load(DB_URL);
    const rows = await db.select<RequestRow[]>(LIST_SQL);
    return rows.map((r) => ({
      id: r.id,
      timestamp: r.timestamp,
      method: r.method,
      path: r.path,
      status: r.response_status,
      latency_ms: r.response_latency_ms,
    }));
  },
};
