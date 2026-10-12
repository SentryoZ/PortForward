/**
 * The detail panel: a formatted JSON tree for the request body, a headers
 * key/value table and the **Replay Request** action.
 *
 * Non-JSON bodies never crash the viewer — they fall back to a raw text/binary
 * view with an explanatory note.
 */

import { useMemo, useState } from "react";

import type { CapturedRequestJson, ReplayResponse } from "../types";
import {
  byteLength,
  decodeBody,
  formatTime,
  methodClass,
  responseBody,
  statusClass,
  tryParseJson,
} from "../lib/format";
import { HeadersTable } from "./HeadersTable";
import { JsonTree } from "./JsonTree";

type Tab = "request" | "headers" | "query" | "response";

const TABS: { id: Tab; label: string }[] = [
  { id: "request", label: "Request body" },
  { id: "headers", label: "Headers" },
  { id: "query", label: "Query" },
  { id: "response", label: "Response" },
];

/** Render a payload: JSON tree when parseable, otherwise a safe raw view. */
function PayloadView({ body, encoding }: { body: string; encoding?: string }) {
  const decoded = useMemo(() => decodeBody({ body, body_encoding: encoding }), [body, encoding]);
  const parsed = useMemo(() => (decoded.binary ? null : tryParseJson(decoded.text)), [decoded]);

  if (decoded.binary) {
    return (
      <div data-testid="body-binary" className="flex flex-col gap-2">
        <p className="text-xs text-amber-400">
          Binary payload ({byteLength(decoded)} bytes) — shown as base64.
        </p>
        <pre className="max-h-80 overflow-auto whitespace-pre-wrap break-all rounded bg-slate-950 p-3 font-mono text-xs text-slate-300">
          {decoded.text}
        </pre>
      </div>
    );
  }

  if (parsed && parsed.ok) {
    return <JsonTree value={parsed.value} />;
  }

  return (
    <div data-testid="body-raw" className="flex flex-col gap-2">
      <p className="text-xs text-slate-500">
        Not JSON ({parsed ? parsed.reason : "unparseable"}) — showing raw text.
      </p>
      <pre className="max-h-80 overflow-auto whitespace-pre-wrap break-words rounded bg-slate-950 p-3 font-mono text-xs text-slate-300">
        {decoded.text === "" ? "(empty body)" : decoded.text}
      </pre>
    </div>
  );
}

function ReplayResult({ replay }: { replay: ReplayResponse }) {
  return (
    <div
      data-testid="replay-result"
      className="rounded-lg border border-emerald-800 bg-emerald-950/40 p-3"
    >
      <div className="flex items-center gap-3">
        <span className="text-[10px] uppercase tracking-wide text-emerald-500">Replay result</span>
        <span className={`font-mono text-sm ${statusClass(replay.status)}`} data-testid="replay-status">
          {replay.status}
        </span>
        <span className="font-mono text-xs text-slate-300" data-testid="replay-latency">
          {replay.latency_ms}ms
        </span>
      </div>
      <div className="mt-2">
        <PayloadView body={replay.body} encoding={replay.body_encoding} />
      </div>
    </div>
  );
}

export function DetailPanel({
  request,
  replay,
  replaying,
  replayError,
  onReplay,
}: {
  request: CapturedRequestJson | null;
  replay: ReplayResponse | null;
  replaying: boolean;
  replayError: string | null;
  onReplay: (id: string) => void;
}) {
  const [tab, setTab] = useState<Tab>("request");

  if (!request) {
    return (
      <section
        className="flex min-w-0 flex-1 items-center justify-center p-6"
        data-testid="detail-panel"
      >
        <p className="text-sm text-slate-500">Select a request to inspect its payload.</p>
      </section>
    );
  }

  const decodedRequest = decodeBody(request);
  const decodedResponse = responseBody(request.response);

  return (
    <section className="flex min-w-0 flex-1 flex-col" data-testid="detail-panel">
      <header className="border-b border-slate-800 px-4 py-3">
        <div className="flex items-center gap-2">
          <span className={`font-mono text-sm font-semibold ${methodClass(request.method)}`}>
            {request.method}
          </span>
          <span className="min-w-0 flex-1 truncate font-mono text-sm text-slate-200" title={request.path}>
            {request.path}
          </span>
          <button
            type="button"
            data-testid="replay-button"
            disabled={replaying}
            onClick={() => onReplay(request.id)}
            className="shrink-0 rounded bg-sky-600 px-3 py-1.5 text-xs font-medium text-white hover:bg-sky-500 disabled:opacity-50"
          >
            {replaying ? "Replaying…" : "Replay request"}
          </button>
        </div>
        <div className="mt-1 flex items-center gap-3 text-xs text-slate-500">
          <span className={`font-mono ${statusClass(request.response.status)}`}>
            {request.response.status}
          </span>
          <span className="font-mono">{request.response.latency_ms}ms</span>
          <span>{formatTime(request.timestamp)}</span>
        </div>
      </header>

      <div className="flex gap-1 border-b border-slate-800 px-3" role="tablist">
        {TABS.map((t) => (
          <button
            key={t.id}
            type="button"
            role="tab"
            aria-selected={tab === t.id}
            data-testid={`tab-${t.id}`}
            onClick={() => setTab(t.id)}
            className={`-mb-px border-b-2 px-3 py-2 text-xs ${
              tab === t.id
                ? "border-emerald-500 text-slate-100"
                : "border-transparent text-slate-500 hover:text-slate-300"
            }`}
          >
            {t.label}
          </button>
        ))}
      </div>

      <div className="min-h-0 flex-1 overflow-auto p-4">
        <div className="flex flex-col gap-4">
          {replayError && (
            <div
              data-testid="replay-error"
              className="rounded border border-rose-800 bg-rose-950/50 p-3 text-xs text-rose-200"
            >
              Replay failed: {replayError}
            </div>
          )}
          {replay && <ReplayResult replay={replay} />}

          {tab === "request" && (
            <div>
              <div className="mb-2 flex items-center gap-2 text-[10px] uppercase tracking-wide text-slate-500">
                <span>Request body</span>
                <span className="text-slate-600">
                  {byteLength(decodedRequest)} bytes
                  {decodedRequest.binary ? " · base64" : ""}
                </span>
              </div>
              <PayloadView body={request.body} encoding={request.body_encoding} />
            </div>
          )}

          {tab === "headers" && <HeadersTable headers={request.headers} caption="Request headers" />}

          {tab === "query" && (
            <HeadersTable
              headers={request.query_params}
              caption="Query parameters"
              emptyLabel="No query parameters."
            />
          )}

          {tab === "response" && (
            <div className="flex flex-col gap-4">
              <HeadersTable headers={request.response.headers} caption="Response headers" />
              <div>
                <div className="mb-2 flex items-center gap-2 text-[10px] uppercase tracking-wide text-slate-500">
                  <span>Response body</span>
                  <span className="text-slate-600">
                    {byteLength(decodedResponse)} bytes
                    {decodedResponse.binary ? " · base64" : ""}
                  </span>
                </div>
                <PayloadView body={request.response.body} encoding={request.response.body_encoding} />
              </div>
            </div>
          )}
        </div>
      </div>
    </section>
  );
}
