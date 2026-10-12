/**
 * The narrow host surface the UI needs.
 *
 * `App` depends on this interface only, so the same components run against the
 * real Tauri runtime (`tauriBridge`) and against the deterministic in-memory
 * simulator used for evidence/screenshots (`simBridge`).
 */

import type { RequestSummary } from "../types";

/** Detach a previously-registered event listener. */
export type Unlisten = () => void;

export interface Bridge {
  /** Invoke a `#[tauri::command]` handler. */
  invoke<T = unknown>(cmd: string, args?: Record<string, unknown>): Promise<T>;
  /** Subscribe to a backend event; resolves to an unsubscribe function. */
  listen<T = unknown>(
    event: string,
    handler: (payload: T) => void,
  ): Promise<Unlisten>;
  /** Load the persisted request summaries shown before live events arrive. */
  listRequests(): Promise<RequestSummary[]>;
}
