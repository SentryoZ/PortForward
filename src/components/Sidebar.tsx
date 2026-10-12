/**
 * Left sidebar: the capture-stack toggle, the public URL + Copy button and the
 * target local-port input.
 *
 * "Active" turns on the whole capture path (interceptor proxy → cloudflared
 * tunnel) so a webhook that hits the public URL is captured and forwarded to
 * the target local port.
 */

import type { InterceptorInfo, TunnelInfo } from "../types";

export type SidebarProps = {
  port: number;
  onPortChange: (port: number) => void;
  active: boolean;
  busy: boolean;
  tunnel: TunnelInfo | null;
  interceptor: InterceptorInfo | null;
  dbStatus: string;
  copied: boolean;
  onToggle: () => void;
  onCopy: (text: string) => void;
};

function StatusDot({ on }: { on: boolean }) {
  return (
    <span
      className={`inline-block h-2 w-2 rounded-full ${on ? "bg-emerald-400" : "bg-slate-600"}`}
      aria-hidden
    />
  );
}

export function Sidebar({
  port,
  onPortChange,
  active,
  busy,
  tunnel,
  interceptor,
  dbStatus,
  copied,
  onToggle,
  onCopy,
}: SidebarProps) {
  return (
    <aside
      className="flex w-72 shrink-0 flex-col gap-5 border-r border-slate-800 bg-slate-950/60 p-5"
      data-testid="sidebar"
    >
      <div>
        <h1 className="text-lg font-semibold tracking-tight">Port Forward</h1>
        <p className="mt-0.5 text-xs text-slate-500">
          Intercept webhooks, then replay them against your local server.
        </p>
      </div>

      <section className="rounded-lg border border-slate-800 bg-slate-900/60 p-3">
        <div className="flex items-center justify-between">
          <span className="flex items-center gap-2 text-xs font-medium uppercase tracking-wide text-slate-300">
            <StatusDot on={active} />
            Tunnel
          </span>
          <button
            type="button"
            role="switch"
            aria-checked={active}
            aria-label="Toggle tunnel"
            data-testid="tunnel-toggle"
            disabled={busy}
            onClick={onToggle}
            className={`relative h-5 w-10 rounded-full transition-colors disabled:opacity-50 ${
              active ? "bg-emerald-600" : "bg-slate-700"
            }`}
          >
            <span
              className={`absolute top-0.5 h-4 w-4 rounded-full bg-white transition-all ${
                active ? "left-5" : "left-0.5"
              }`}
            />
          </button>
        </div>
        <p className="mt-2 text-xs text-slate-500" data-testid="tunnel-status">
          {active ? "Active — forwarding to local port" : "Stopped"}
        </p>
      </section>

      <section className="flex flex-col gap-1">
        <label className="flex flex-col gap-1">
          <span className="text-[10px] uppercase tracking-wide text-slate-500">Local port</span>
          <input
            type="number"
            min={1}
            max={65535}
            data-testid="port-input"
            className="rounded border border-slate-700 bg-slate-900 px-3 py-2 text-sm outline-none focus:border-emerald-600 disabled:opacity-60"
            value={Number.isFinite(port) ? port : ""}
            disabled={active || busy}
            onChange={(e) => onPortChange(Number(e.target.value))}
          />
        </label>
        <span className="text-[10px] text-slate-600">Requests are forwarded to 127.0.0.1:{port}</span>
      </section>

      <section className="flex flex-col gap-1">
        <span className="text-[10px] uppercase tracking-wide text-slate-500">Public URL</span>
        {tunnel ? (
          <div className="flex items-center gap-2">
            <a
              className="min-w-0 flex-1 truncate font-mono text-xs text-emerald-400 underline"
              href={tunnel.public_url}
              target="_blank"
              rel="noreferrer"
              data-testid="public-url"
              title={tunnel.public_url}
            >
              {tunnel.public_url}
            </a>
            <button
              type="button"
              data-testid="copy-url"
              onClick={() => onCopy(tunnel.public_url)}
              className="shrink-0 rounded border border-slate-700 px-2 py-1 text-xs text-slate-200 hover:border-emerald-600 hover:text-emerald-300"
            >
              {copied ? "Copied" : "Copy"}
            </button>
          </div>
        ) : (
          <span className="font-mono text-xs text-slate-600">— not running —</span>
        )}
      </section>

      {interceptor && (
        <section className="rounded-lg border border-slate-800 bg-slate-900/40 p-3 text-xs text-slate-400">
          <div className="text-[10px] uppercase tracking-wide text-slate-500">Proxy</div>
          <div className="mt-1 font-mono text-slate-300" data-testid="proxy-url">
            {interceptor.proxy_url}
          </div>
        </section>
      )}

      <p className="mt-auto text-[10px] text-slate-600" data-testid="db-status">
        {dbStatus}
      </p>
    </aside>
  );
}
