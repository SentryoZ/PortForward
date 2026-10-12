/**
 * Headless UI evidence driver (MAN-14).
 *
 * Drives Google Chrome over the DevTools Protocol (no npm dependencies — Node's
 * global `fetch` + `WebSocket`). For each scenario it:
 *   1. navigates to the evidence harness (`/evidence/`) with query params,
 *   2. waits for the harness to publish `window.__pfEvidence`,
 *   3. captures a PNG screenshot and the machine-readable evidence.
 *
 * Usage:
 *   node scripts/ui-evidence.mjs [outDir] [baseUrl] [cdpUrl]
 *
 * Requires Chrome running with `--remote-debugging-port=<port>`.
 */

import { mkdirSync, writeFileSync } from "node:fs";
import { join } from "node:path";

const OUT_DIR = process.argv[2] ?? "evidence/out";
const BASE_URL = process.argv[3] ?? "http://localhost:1420/evidence/";
const CDP_URL = process.argv[4] ?? "http://127.0.0.1:9222";

const VIEWPORT = { width: 1440, height: 900, deviceScaleFactor: 2 };
const WAIT_TIMEOUT_MS = 30000;

const SCENARIOS = [
  { name: "01-list-detail-json", query: "?seed=6&select=0&replay=1", shot: true },
  { name: "02-copy-public-url", query: "?seed=6&select=2&copy=1", shot: true },
  { name: "03-non-json-body", query: "?seed=6&select=3", shot: true },
  { name: "04-binary-body", query: "?seed=6&select=4", shot: true },
  { name: "05-live-latency", query: "?seed=3&emit=30&latency=1&select=0", shot: true },
];

const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

async function pageTargetSocketUrl() {
  for (let attempt = 0; attempt < 40; attempt += 1) {
    try {
      const res = await fetch(`${CDP_URL}/json/list`);
      const targets = await res.json();
      const page = targets.find((t) => t.type === "page");
      if (page?.webSocketDebuggerUrl) return page.webSocketDebuggerUrl;
    } catch {
      /* chrome not up yet */
    }
    await sleep(250);
  }
  throw new Error(`no Chrome page target at ${CDP_URL}`);
}

class Cdp {
  constructor(ws) {
    this.ws = ws;
    this.nextId = 1;
    this.pending = new Map();
    ws.addEventListener("message", (event) => {
      const msg = JSON.parse(event.data);
      if (msg.id && this.pending.has(msg.id)) {
        const { resolve, reject } = this.pending.get(msg.id);
        this.pending.delete(msg.id);
        if (msg.error) reject(new Error(JSON.stringify(msg.error)));
        else resolve(msg.result);
      }
    });
  }

  send(method, params = {}) {
    const id = this.nextId++;
    return new Promise((resolve, reject) => {
      this.pending.set(id, { resolve, reject });
      this.ws.send(JSON.stringify({ id, method, params }));
    });
  }
}

async function evaluate(cdp, expression) {
  const result = await cdp.send("Runtime.evaluate", {
    expression,
    returnByValue: true,
    awaitPromise: true,
  });
  if (result.exceptionDetails) {
    throw new Error(`evaluate failed: ${result.exceptionDetails.text}`);
  }
  return result.result.value;
}

async function waitForDone(cdp) {
  const deadline = Date.now() + WAIT_TIMEOUT_MS;
  while (Date.now() < deadline) {
    const done = await evaluate(cdp, "window.__pfEvidenceDone === true").catch(() => false);
    if (done) return;
    await sleep(200);
  }
  throw new Error("timed out waiting for window.__pfEvidenceDone");
}

async function main() {
  mkdirSync(OUT_DIR, { recursive: true });
  const ws = new WebSocket(await pageTargetSocketUrl());
  await new Promise((resolve, reject) => {
    ws.addEventListener("open", resolve, { once: true });
    ws.addEventListener("error", reject, { once: true });
  });
  const cdp = new Cdp(ws);
  await cdp.send("Page.enable");
  await cdp.send("Runtime.enable");
  await cdp.send("Emulation.setDeviceMetricsOverride", {
    width: VIEWPORT.width,
    height: VIEWPORT.height,
    deviceScaleFactor: VIEWPORT.deviceScaleFactor,
    mobile: false,
  });

  const summary = [];
  for (const scenario of SCENARIOS) {
    const url = `${BASE_URL}${scenario.query}`;
    await cdp.send("Page.navigate", { url });
    await sleep(600);
    await cdp.send("Emulation.setDeviceMetricsOverride", {
      width: VIEWPORT.width,
      height: VIEWPORT.height,
      deviceScaleFactor: VIEWPORT.deviceScaleFactor,
      mobile: false,
    });
    await waitForDone(cdp);
    // Let layout settle before the capture.
    await sleep(300);

    const evidence = JSON.parse(await evaluate(cdp, "JSON.stringify(window.__pfEvidence)"));
    writeFileSync(join(OUT_DIR, `${scenario.name}.json`), JSON.stringify(evidence, null, 2));

    if (scenario.shot) {
      const shot = await cdp.send("Page.captureScreenshot", { format: "png" });
      writeFileSync(join(OUT_DIR, `${scenario.name}.png`), Buffer.from(shot.data, "base64"));
    }

    summary.push({
      scenario: scenario.name,
      url,
      errors: evidence.errors,
      latency: evidence.latency
        ? {
            count: evidence.latency.count,
            minMs: evidence.latency.minMs,
            medianMs: evidence.latency.medianMs,
            p95Ms: evidence.latency.p95Ms,
            maxMs: evidence.latency.maxMs,
            budgetMs: evidence.latency.budgetMs,
            pass: evidence.latency.pass,
          }
        : null,
      copy: evidence.copy,
      replay: evidence.replay,
      nonJsonRendered: evidence.nonJsonRendered,
      binaryRendered: evidence.binaryRendered,
    });
  }

  writeFileSync(join(OUT_DIR, "summary.json"), JSON.stringify(summary, null, 2));
  console.log(JSON.stringify(summary, null, 2));
  ws.close();
}

main().catch((err) => {
  console.error(err);
  process.exit(1);
});
