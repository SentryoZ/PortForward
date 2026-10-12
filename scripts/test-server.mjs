#!/usr/bin/env node
/**
 * Tiny echo/test server for the Port Forward acceptance run (MAN-15).
 *
 * This is the "local app server" the tunnel forwards to and the replay engine
 * re-sends to. It is dependency-free Node (`node:http`) so the acceptance run
 * needs nothing but Node itself.
 *
 *   node scripts/test-server.mjs --port 3000 --log evidence/mvp/local-server.log
 *
 * Behaviour:
 *   * logs every request it receives to the `--log` JSONL file (one object per
 *     line: timestamp, method, path, query, headers, content_type, user_agent,
 *     body, body_encoding, remote_addr) and to stdout;
 *   * echoes the request back as JSON: `{ ok, received_at, echo: { … } }`;
 *   * `GET /health`  -> `{ ok: true }` (readiness probe);
 *   * `ANY /status/<code>` -> responds with that status code (passthrough test).
 *
 * Env fallbacks: `PORT`, `LOCAL_SERVER_LOG`.
 */

import { createServer } from "node:http";
import { appendFileSync, mkdirSync, writeFileSync } from "node:fs";
import { dirname } from "node:path";

function arg(name, fallback) {
  const index = process.argv.indexOf(`--${name}`);
  if (index !== -1 && process.argv[index + 1]) return process.argv[index + 1];
  return fallback;
}

const PORT = Number(arg("port", process.env.PORT ?? "3000"));
const LOG_PATH = arg("log", process.env.LOCAL_SERVER_LOG ?? "local-server.log");

mkdirSync(dirname(LOG_PATH), { recursive: true });
// Start from a clean log so every acceptance run is self-contained.
writeFileSync(LOG_PATH, "");

const utf8 = new TextDecoder("utf-8", { fatal: true });

function decodeBody(buffer) {
  try {
    return { body: utf8.decode(buffer), body_encoding: "utf8" };
  } catch {
    return { body: buffer.toString("base64"), body_encoding: "base64" };
  }
}

function appendLog(entry) {
  appendFileSync(LOG_PATH, `${JSON.stringify(entry)}\n`);
}

const server = createServer((req, res) => {
  const chunks = [];
  req.on("data", (chunk) => chunks.push(chunk));
  req.on("end", () => {
    const raw = Buffer.concat(chunks);
    const url = new URL(req.url ?? "/", `http://localhost:${PORT}`);
    const { body, body_encoding } = decodeBody(raw);
    const receivedAt = new Date().toISOString();

    const entry = {
      timestamp: receivedAt,
      method: req.method,
      path: url.pathname,
      query: Object.fromEntries(url.searchParams.entries()),
      headers: req.headers,
      content_type: req.headers["content-type"] ?? null,
      user_agent: req.headers["user-agent"] ?? null,
      body,
      body_encoding,
      bytes: raw.length,
      remote_addr: req.socket.remoteAddress ?? null,
    };
    appendLog(entry);
    console.log(
      `[test-server] ${receivedAt} ${req.method} ${url.pathname}${url.search} ` +
        `(${raw.length} bytes) from ${entry.remote_addr}`,
    );

    // `/status/<code>` lets a scenario exercise response passthrough.
    const statusMatch = /^\/status\/(\d{3})$/.exec(url.pathname);
    const status = statusMatch ? Number(statusMatch[1]) : 200;

    if (url.pathname === "/health") {
      res.writeHead(200, { "content-type": "application/json" });
      res.end(JSON.stringify({ ok: true, port: PORT }));
      return;
    }

    const payload = {
      ok: status < 400,
      received_at: receivedAt,
      echo: {
        method: req.method,
        path: url.pathname,
        query: entry.query,
        content_type: entry.content_type,
        body,
        body_encoding,
        bytes: raw.length,
      },
    };
    res.writeHead(status, {
      "content-type": "application/json",
      "x-served-by": "port-forward-test-server",
    });
    res.end(JSON.stringify(payload));
  });
});

server.listen(PORT, "127.0.0.1", () => {
  console.log(`[test-server] READY port=${PORT} log=${LOG_PATH} pid=${process.pid}`);
});

for (const signal of ["SIGINT", "SIGTERM"]) {
  process.on(signal, () => {
    console.log(`[test-server] ${signal} -> shutting down`);
    server.close(() => process.exit(0));
    setTimeout(() => process.exit(0), 500).unref();
  });
}
