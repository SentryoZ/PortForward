/**
 * Headless UI evidence harness (MAN-14).
 *
 * Boots the real `App` against the deterministic `SimBridge` in a plain browser
 * (no Tauri, no window server), runs a scripted scenario driven by query params,
 * and publishes machine-readable results on `window.__pfEvidence` plus a hidden
 * `#evidence-json` node so `chrome --headless --dump-dom` can read them.
 *
 * Params:
 *   seed=N      preload N stored requests (default 6)
 *   emit=N      emit N live `webhook-received` events
 *   select=N    click the Nth list row (0-based)
 *   replay=1    click “Replay request” after selecting
 *   copy=1      click the public-URL “Copy” button
 *   latency=1   measure event→render latency for the emitted events
 */

import { createRoot } from "react-dom/client";

import App from "../src/App";
import { SimBridge, makeRecord } from "../src/lib/simBridge";
import type { CapturedRequestJson } from "../src/types";
import "../src/styles.css";

type LatencySample = { id: string; method: string; domMs: number; paintMs: number };

type Evidence = {
  scenario: Record<string, string | number | boolean | null>;
  publicUrl: string | null;
  copy: { clicked: boolean; clipboardValue: string | null; label: string | null } | null;
  replay: { status: number | null; latencyMs: number | null } | null;
  latency: {
    samples: LatencySample[];
    count: number;
    minMs: number;
    medianMs: number;
    p95Ms: number;
    maxMs: number;
    paintMaxMs: number;
    budgetMs: number;
    pass: boolean;
    method: string;
  } | null;
  nonJsonRendered: boolean;
  binaryRendered: boolean;
  errors: string[];
};

const params = new URLSearchParams(window.location.search);
const seedCount = Number(params.get("seed") ?? "6");
const emitCount = Number(params.get("emit") ?? "0");
const selectIndex = params.get("select");
const doReplay = params.get("replay") === "1";
const doCopy = params.get("copy") === "1";
const doLatency = params.get("latency") === "1";
const LATENCY_BUDGET_MS = 200;

const bridge = new SimBridge();
bridge.seed(seedCount);

const evidence: Evidence = {
  scenario: {
    seed: seedCount,
    emit: emitCount,
    select: selectIndex,
    replay: doReplay,
    copy: doCopy,
    latency: doLatency,
  },
  publicUrl: null,
  copy: null,
  replay: null,
  latency: null,
  nonJsonRendered: false,
  binaryRendered: false,
  errors: [],
};

function waitFor(selector: string, timeoutMs = 5000): Promise<Element> {
  return new Promise((resolve, reject) => {
    const existing = document.querySelector(selector);
    if (existing) return resolve(existing);
    const observer = new MutationObserver(() => {
      const el = document.querySelector(selector);
      if (el) {
        observer.disconnect();
        resolve(el);
      }
    });
    observer.observe(document.body, { childList: true, subtree: true });
    window.setTimeout(() => {
      observer.disconnect();
      reject(new Error(`timeout waiting for ${selector}`));
    }, timeoutMs);
  });
}

function rows(): HTMLElement[] {
  return Array.from(document.querySelectorAll<HTMLElement>('[data-testid="request-row"]'));
}

/** Measure event→render: mutation (DOM) and next-frame (paint) for one event. */
function measure(record: CapturedRequestJson): Promise<LatencySample> {
  return new Promise((resolve, reject) => {
    const selector = `[data-request-id="${record.id}"]`;
    let t0 = 0;
    let settled = false;
    let observer: MutationObserver | null = null;
    const finish = (domMs: number, paintMs: number) => {
      if (settled) return;
      settled = true;
      observer?.disconnect();
      resolve({ id: record.id, method: record.method, domMs, paintMs });
    };
    observer = new MutationObserver(() => {
      if (!document.querySelector(selector)) return;
      const domMs = performance.now() - t0;
      // Prefer the next frame when the compositor runs; headless virtual-time
      // environments may never fire rAF, so fall back to the DOM time.
      requestAnimationFrame(() => finish(domMs, performance.now() - t0));
      window.setTimeout(() => finish(domMs, domMs), 50);
    });
    observer.observe(document.body, { childList: true, subtree: true });
    t0 = performance.now();
    bridge.emit(record);
    window.setTimeout(() => {
      if (!settled) reject(new Error(`row never rendered: ${record.id}`));
    }, 2000);
  });
}

function percentile(sorted: number[], p: number): number {
  if (sorted.length === 0) return 0;
  const index = Math.min(sorted.length - 1, Math.ceil((p / 100) * sorted.length) - 1);
  return sorted[index]!;
}

function round(n: number): number {
  return Math.round(n * 100) / 100;
}

async function run(): Promise<void> {
  const root = createRoot(document.getElementById("root") as HTMLElement);
  root.render(<App bridge={bridge} />);

  await waitFor('[data-testid="request-row"]');

  // Turn the capture stack on so the public URL and replay are available.
  (document.querySelector('[data-testid="tunnel-toggle"]') as HTMLButtonElement).click();
  const urlEl = await waitFor('[data-testid="public-url"]');
  evidence.publicUrl = urlEl.textContent?.trim() ?? null;

  if (doCopy) {
    const clipboardValue = { value: null as string | null };
    try {
      Object.defineProperty(navigator, "clipboard", {
        configurable: true,
        value: {
          writeText: async (text: string) => {
            clipboardValue.value = text;
          },
          readText: async () => clipboardValue.value ?? "",
        },
      });
    } catch {
      /* clipboard override unavailable; fall back to the button label */
    }
    (document.querySelector('[data-testid="copy-url"]') as HTMLButtonElement).click();
    await new Promise((r) => window.setTimeout(r, 150));
    const label = (document.querySelector('[data-testid="copy-url"]') as HTMLElement).textContent;
    evidence.copy = {
      clicked: true,
      clipboardValue: clipboardValue.value,
      label: label?.trim() ?? null,
    };
  }

  // Live traffic: emit `emitCount` events (and measure them when asked).
  const samples: LatencySample[] = [];
  if (emitCount > 0) {
    for (let i = 0; i < emitCount; i += 1) {
      const record = makeRecord(100 + i);
      if (doLatency) {
        samples.push(await measure(record));
      } else {
        bridge.emit(record);
      }
    }
    await new Promise((r) => requestAnimationFrame(() => r(null)));
  }

  if (doLatency && samples.length > 0) {
    const domSorted = samples.map((s) => s.domMs).sort((a, b) => a - b);
    const med = percentile(domSorted, 50);
    const maxDom = domSorted[domSorted.length - 1]!;
    evidence.latency = {
      samples: samples.map((s) => ({ ...s, domMs: round(s.domMs), paintMs: round(s.paintMs) })),
      count: samples.length,
      minMs: round(domSorted[0]!),
      medianMs: round(med),
      p95Ms: round(percentile(domSorted, 95)),
      maxMs: round(maxDom),
      paintMaxMs: round(Math.max(...samples.map((s) => s.paintMs))),
      budgetMs: LATENCY_BUDGET_MS,
      pass: maxDom <= LATENCY_BUDGET_MS,
      method: "MutationObserver delta (performance.now()) from emit() to the row's DOM insertion; paintMs adds the next animation frame",
    };
  }

  // Select a row for the detail-panel screenshot.
  if (selectIndex !== null) {
    const target = rows()[Number(selectIndex)];
    if (!target) throw new Error(`no row at index ${selectIndex}`);
    target.click();
    await waitFor('[data-testid="detail-panel"] [data-testid="json-tree"], [data-testid="detail-panel"] [data-testid="body-raw"], [data-testid="detail-panel"] [data-testid="body-binary"]');
    evidence.nonJsonRendered = document.querySelector('[data-testid="body-raw"]') !== null;
    evidence.binaryRendered = document.querySelector('[data-testid="body-binary"]') !== null;

    if (doReplay) {
      (document.querySelector('[data-testid="replay-button"]') as HTMLButtonElement).click();
      await waitFor('[data-testid="replay-result"]');
      const status = document.querySelector('[data-testid="replay-status"]')?.textContent;
      const latency = document.querySelector('[data-testid="replay-latency"]')?.textContent;
      evidence.replay = {
        status: status ? Number(status) : null,
        latencyMs: latency ? Number(latency.replace(/[^\d.]/g, "")) : null,
      };
    }
  }

  (window as unknown as { __pfEvidence: Evidence }).__pfEvidence = evidence;
}

function finalize(): void {
  const node = document.getElementById("evidence-json");
  if (node) node.textContent = JSON.stringify(evidence, null, 2);
  (window as unknown as { __pfEvidenceDone: boolean }).__pfEvidenceDone = true;
  document.title = `evidence:${evidence.latency ? (evidence.latency.pass ? "PASS" : "FAIL") : "ok"}`;
}

run()
  .then(finalize)
  .catch((err: unknown) => {
    evidence.errors.push(err instanceof Error ? err.message : String(err));
    finalize();
  });
