/**
 * Port Forward — application shell.
 *
 * Layout: sidebar (capture toggle + public URL + local port) | live request
 * list | detail panel (JSON tree, headers, replay).
 *
 * All host access goes through the injected [`Bridge`], so the exact same
 * component tree runs against the real Tauri runtime and against the
 * deterministic simulator used for evidence (`evidence/main.tsx`).
 */

import { useCallback, useEffect, useRef, useState } from "react";

import type { Bridge } from "./lib/bridge";
import type {
  CapturedRequestJson,
  InterceptorInfo,
  ReplayResponse,
  RequestSummary,
  TunnelCommandError,
  TunnelInfo,
} from "./types";
import { toSummary } from "./types";
import { Sidebar } from "./components/Sidebar";
import { RequestList } from "./components/RequestList";
import { DetailPanel } from "./components/DetailPanel";

const MAX_LIST = 500;

/** Normalise a rejection from `invoke` into `{ kind, message }`. */
function asCommandError(err: unknown): TunnelCommandError {
  if (err && typeof err === "object" && "message" in err) {
    const e = err as { kind?: unknown; message?: unknown };
    return { kind: String(e.kind ?? "error"), message: String(e.message) };
  }
  return { kind: "error", message: String(err) };
}

function fallbackCopy(text: string): void {
  const el = document.createElement("textarea");
  el.value = text;
  el.setAttribute("readonly", "");
  el.style.position = "fixed";
  el.style.opacity = "0";
  document.body.appendChild(el);
  el.select();
  try {
    document.execCommand("copy");
  } finally {
    document.body.removeChild(el);
  }
}

export default function App({ bridge }: { bridge: Bridge }) {
  const [port, setPort] = useState(3000);
  const [active, setActive] = useState(false);
  const [busy, setBusy] = useState(false);
  const [tunnel, setTunnel] = useState<TunnelInfo | null>(null);
  const [interceptor, setInterceptor] = useState<InterceptorInfo | null>(null);
  const [error, setError] = useState<TunnelCommandError | null>(null);
  const [dbStatus, setDbStatus] = useState("loading store…");
  const [copied, setCopied] = useState(false);

  const [requests, setRequests] = useState<RequestSummary[]>([]);
  const [liveCount, setLiveCount] = useState(0);
  const [selectedId, setSelectedId] = useState<string | null>(null);
  const [detail, setDetail] = useState<CapturedRequestJson | null>(null);
  const [detailLoading, setDetailLoading] = useState(false);

  const [replay, setReplay] = useState<ReplayResponse | null>(null);
  const [replaying, setReplaying] = useState(false);
  const [replayError, setReplayError] = useState<string | null>(null);

  const records = useRef(new Map<string, CapturedRequestJson>());

  // --- initial hydration ---------------------------------------------------
  useEffect(() => {
    let cancelled = false;
    (async () => {
      try {
        const stored = await bridge.listRequests();
        if (cancelled) return;
        setRequests(stored.slice(0, MAX_LIST));
        setDbStatus(`store ready — ${stored.length} stored request${stored.length === 1 ? "" : "s"}`);
      } catch (err) {
        if (!cancelled) setDbStatus(`store unavailable: ${asCommandError(err).message}`);
      }
      try {
        const t = await bridge.invoke<TunnelInfo | null>("tunnel_status");
        if (!cancelled) {
          setTunnel(t);
          setActive(t !== null);
        }
      } catch {
        /* no tunnel status before the app is ready */
      }
      try {
        const i = await bridge.invoke<InterceptorInfo | null>("interceptor_status");
        if (!cancelled) setInterceptor(i);
      } catch {
        /* ignore */
      }
    })();
    return () => {
      cancelled = true;
    };
  }, [bridge]);

  // --- live events ---------------------------------------------------------
  useEffect(() => {
    let unlisten: (() => void) | null = null;
    let disposed = false;
    bridge
      .listen<CapturedRequestJson>("webhook-received", (record) => {
        records.current.set(record.id, record);
        setLiveCount((n) => n + 1);
        setRequests((prev) => [toSummary(record), ...prev.filter((r) => r.id !== record.id)].slice(0, MAX_LIST));
      })
      .then((off) => {
        if (disposed) off();
        else unlisten = off;
      })
      .catch(() => undefined);
    return () => {
      disposed = true;
      unlisten?.();
    };
  }, [bridge]);

  const selectRequest = useCallback(
    async (id: string) => {
      setSelectedId(id);
      setReplay(null);
      setReplayError(null);
      const cached = records.current.get(id);
      if (cached) {
        setDetail(cached);
        return;
      }
      setDetail(null);
      setDetailLoading(true);
      try {
        const record = await bridge.invoke<CapturedRequestJson | null>("get_captured_request", { id });
        setDetail(record);
        if (record) records.current.set(record.id, record);
      } catch (err) {
        setError(asCommandError(err));
      } finally {
        setDetailLoading(false);
      }
    },
    [bridge],
  );

  // --- capture-stack toggle ------------------------------------------------
  const toggle = useCallback(async () => {
    setBusy(true);
    setError(null);
    try {
      if (!active) {
        const target = Number.isFinite(port) && port > 0 ? port : 3000;
        // Capture requires the tunnel to point at the interceptor proxy, so the
        // proxy starts first and cloudflared is bound to its port.
        const interceptorInfo = await bridge.invoke<InterceptorInfo>("start_interceptor", {
          targetPort: target,
        });
        setInterceptor(interceptorInfo);
        const tunnelInfo = await bridge.invoke<TunnelInfo>("start_tunnel", {
          localPort: interceptorInfo.proxy_port,
        });
        setTunnel(tunnelInfo);
        setActive(true);
      } else {
        await bridge.invoke("stop_tunnel").catch(() => undefined);
        setTunnel(null);
        await bridge.invoke("stop_interceptor").catch(() => undefined);
        setInterceptor(null);
        setActive(false);
      }
    } catch (err) {
      setError(asCommandError(err));
    } finally {
      setBusy(false);
    }
  }, [active, bridge, port]);

  const copyUrl = useCallback((text: string) => {
    (async () => {
      try {
        await navigator.clipboard.writeText(text);
      } catch {
        fallbackCopy(text);
      }
    })();
    setCopied(true);
    window.setTimeout(() => setCopied(false), 1500);
  }, []);

  const doReplay = useCallback(
    async (id: string) => {
      setReplaying(true);
      setReplayError(null);
      setReplay(null);
      try {
        const payload = await bridge.invoke<ReplayResponse>("replay_request", { requestId: id });
        setReplay(payload);
      } catch (err) {
        setReplayError(asCommandError(err).message);
      } finally {
        setReplaying(false);
      }
    },
    [bridge],
  );

  return (
    <div className="flex h-screen w-screen overflow-hidden bg-[#0b1020] text-slate-100">
      <Sidebar
        port={port}
        onPortChange={setPort}
        active={active}
        busy={busy}
        tunnel={tunnel}
        interceptor={interceptor}
        dbStatus={dbStatus}
        copied={copied}
        onToggle={toggle}
        onCopy={copyUrl}
      />
      <main className="flex min-w-0 flex-1 flex-col">
        {error && (
          <div
            data-testid="command-error"
            className="border-b border-rose-800 bg-rose-950/60 px-4 py-2 text-xs text-rose-200"
          >
            <span className="font-semibold">[{error.kind}]</span> {error.message}
          </div>
        )}
        <div className="flex min-h-0 flex-1">
          <RequestList
            requests={requests}
            selectedId={selectedId}
            onSelect={selectRequest}
            liveCount={liveCount}
          />
          {detailLoading ? (
            <section
              className="flex min-w-0 flex-1 items-center justify-center p-6"
              data-testid="detail-panel"
            >
              <p className="text-sm text-slate-500">Loading request…</p>
            </section>
          ) : (
            <DetailPanel
              request={detail}
              replay={replay}
              replaying={replaying}
              replayError={replayError}
              onReplay={doReplay}
            />
          )}
        </div>
      </main>
    </div>
  );
}
