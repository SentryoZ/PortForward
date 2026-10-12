import { useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import Database from "@tauri-apps/plugin-sql";

type TunnelInfo = { public_url: string; pid: number; local_port: number };
type TunnelCommandError = { kind: string; message: string };

const DB_URL = "sqlite:portforward.db";

function isTauri(): boolean {
  return typeof window !== "undefined" && "__TAURI_INTERNALS__" in window;
}

export default function App() {
  const [port, setPort] = useState(3000);
  const [tunnel, setTunnel] = useState<TunnelInfo | null>(null);
  const [error, setError] = useState<TunnelCommandError | null>(null);
  const [busy, setBusy] = useState(false);
  const [dbStatus, setDbStatus] = useState<string>("connecting…");

  useEffect(() => {
    if (!isTauri()) {
      setDbStatus("not running inside Tauri");
      return;
    }
    invoke<TunnelInfo | null>("tunnel_status")
      .then(setTunnel)
      .catch(() => undefined);

    // Load the SQLite store; this applies the `requests` migration on first run.
    (async () => {
      try {
        const db = await Database.load(DB_URL);
        const rows = await db.select<{ n: number }[]>("SELECT COUNT(*) AS n FROM requests");
        setDbStatus(`sqlite ready — requests table OK (${rows[0]?.n ?? 0} rows)`);
      } catch (e) {
        setDbStatus(`sqlite error: ${String(e)}`);
      }
    })();
  }, []);

  async function start() {
    setBusy(true);
    setError(null);
    try {
      if (!isTauri()) {
        setError({ kind: "not_tauri", message: "Run inside the Tauri window (npm run tauri dev)." });
        return;
      }
      setTunnel(await invoke<TunnelInfo>("start_tunnel", { localPort: port }));
    } catch (e) {
      setError(e as TunnelCommandError);
    } finally {
      setBusy(false);
    }
  }

  async function stop() {
    setBusy(true);
    setError(null);
    try {
      await invoke("stop_tunnel");
      setTunnel(null);
    } catch (e) {
      setError(e as TunnelCommandError);
    } finally {
      setBusy(false);
    }
  }

  return (
    <main className="mx-auto flex max-w-3xl flex-col gap-6 p-8">
      <header>
        <h1 className="text-2xl font-semibold">Port Forward</h1>
        <p className="text-sm text-slate-400">Expose a local port through a cloudflared quick tunnel.</p>
        <p className="mt-1 text-xs text-slate-500" data-testid="db-status">{dbStatus}</p>
      </header>

      <section className="flex items-end gap-3">
        <label className="flex flex-col gap-1">
          <span className="text-xs uppercase tracking-wide text-slate-400">Local port</span>
          <input
            type="number"
            className="w-32 rounded border border-slate-700 bg-slate-900 px-3 py-2"
            value={port}
            onChange={(e) => setPort(Number(e.target.value))}
          />
        </label>
        <button
          className="rounded bg-emerald-600 px-4 py-2 font-medium disabled:opacity-50"
          onClick={start}
          disabled={busy || !!tunnel}
        >
          Start tunnel
        </button>
        <button
          className="rounded bg-rose-600 px-4 py-2 font-medium disabled:opacity-50"
          onClick={stop}
          disabled={busy || !tunnel}
        >
          Stop tunnel
        </button>
      </section>

      {tunnel && (
        <section className="rounded border border-slate-700 bg-slate-900/60 p-4">
          <div className="text-xs uppercase tracking-wide text-slate-400">Public URL</div>
          <a className="break-all text-emerald-400 underline" href={tunnel.public_url} target="_blank">
            {tunnel.public_url}
          </a>
          <div className="mt-2 text-xs text-slate-400">
            pid {tunnel.pid} → 127.0.0.1:{tunnel.local_port}
          </div>
        </section>
      )}

      {error && (
        <section className="rounded border border-rose-700 bg-rose-950/50 p-4 text-sm">
          <div className="font-semibold">[{error.kind}]</div>
          <div>{error.message}</div>
        </section>
      )}
    </main>
  );
}
