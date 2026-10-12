/**
 * The live request list: a real-time log table of captured webhooks.
 *
 * Rows are keyed by capture id and carry `data-request-id` so the evidence
 * harness can measure event→render latency from the DOM. The newest request is
 * always first.
 */

import type { RequestSummary } from "../types";
import { formatTime, methodClass, statusClass } from "../lib/format";

export function RequestList({
  requests,
  selectedId,
  onSelect,
  liveCount,
}: {
  requests: RequestSummary[];
  selectedId: string | null;
  onSelect: (id: string) => void;
  liveCount: number;
}) {
  return (
    <section
      className="flex min-w-0 flex-1 flex-col border-r border-slate-800"
      data-testid="request-list"
    >
      <header className="flex items-baseline justify-between border-b border-slate-800 px-4 py-3">
        <h2 className="text-sm font-medium">Requests</h2>
        <span className="text-xs text-slate-500" data-testid="request-count">
          {requests.length} shown · {liveCount} live
        </span>
      </header>

      <div className="min-h-0 flex-1 overflow-auto">
        {requests.length === 0 ? (
          <p className="p-4 text-xs text-slate-500" data-testid="list-empty">
            Waiting for a request through the public URL…
          </p>
        ) : (
          <table className="w-full border-collapse text-left text-xs">
            <thead className="sticky top-0 z-10 bg-slate-950/95 text-[10px] uppercase tracking-wide text-slate-500">
              <tr>
                <th className="px-3 py-2 font-medium">Status</th>
                <th className="px-3 py-2 font-medium">Method</th>
                <th className="px-3 py-2 font-medium">Path</th>
                <th className="px-3 py-2 font-medium">Time</th>
                <th className="px-3 py-2 text-right font-medium">Latency</th>
              </tr>
            </thead>
            <tbody>
              {requests.map((request) => {
                const selected = request.id === selectedId;
                return (
                  <tr
                    key={request.id}
                    data-request-id={request.id}
                    data-testid="request-row"
                    onClick={() => onSelect(request.id)}
                    className={`cursor-pointer border-b border-slate-800/60 ${
                      selected ? "bg-slate-800/80" : "hover:bg-slate-900/70"
                    }`}
                  >
                    <td className={`px-3 py-1.5 font-mono ${statusClass(request.status)}`}>
                      {request.status}
                    </td>
                    <td className={`px-3 py-1.5 font-mono ${methodClass(request.method)}`}>
                      {request.method}
                    </td>
                    <td className="max-w-[16rem] truncate px-3 py-1.5 font-mono text-slate-200" title={request.path}>
                      {request.path}
                    </td>
                    <td className="px-3 py-1.5 font-mono text-slate-500">
                      {formatTime(request.timestamp)}
                    </td>
                    <td className="px-3 py-1.5 text-right font-mono text-slate-400">
                      {request.latency_ms}ms
                    </td>
                  </tr>
                );
              })}
            </tbody>
          </table>
        )}
      </div>
    </section>
  );
}
