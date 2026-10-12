/**
 * A deterministic, in-memory implementation of [`Bridge`] for evidence and
 * screenshots.
 *
 * It is **not** part of the shipped app: `main.tsx` always uses `tauriBridge`.
 * The simulator reproduces the exact command/event contract the Rust layer
 * exposes (`webhook-received`, `start_interceptor`, `start_tunnel`,
 * `replay_request`, …) so the same UI can be captured headlessly without a
 * window server, cloudflared or a real target server.
 */

import type {
  CapturedRequestJson,
  InterceptorInfo,
  ReplayResponse,
  RequestSummary,
  TunnelInfo,
} from "../types";
import { toSummary } from "../types";
import type { Bridge, Unlisten } from "./bridge";

const PROXY_PORT = 42891;
export const SIM_PUBLIC_URL = "https://port-forward-demo.trycloudflare.com";

type Listener = (record: CapturedRequestJson) => void;

function iso(offsetMs: number): string {
  return new Date(Date.now() - offsetMs).toISOString();
}

function jsonBody(value: unknown): string {
  return JSON.stringify(value, null, 2);
}

/** Build one representative capture at `index`. */
export function makeRecord(index: number): CapturedRequestJson {
  const templates: Array<Partial<CapturedRequestJson> & { status: number; latency: number }> = [
    {
      method: "POST",
      path: "/api/webhooks/stripe",
      status: 200,
      latency: 42,
      body: jsonBody({
        id: `evt_${index}`,
        type: "payment_intent.succeeded",
        created: 1728691200,
        data: {
          object: {
            id: "pi_3P",
            amount: 4200,
            currency: "usd",
            metadata: { order_id: `ORD-${index}`, source: "checkout" },
            charges: [{ id: "ch_1", paid: true, outcome: { risk_level: "normal" } }],
          },
        },
      }),
    },
    {
      method: "POST",
      path: "/hooks/github",
      status: 201,
      latency: 18,
      body: jsonBody({ action: "opened", number: 42, pull_request: { title: "Add replay", draft: false } }),
    },
    {
      method: "GET",
      path: "/health",
      status: 200,
      latency: 3,
      body: "ok",
    },
    {
      method: "POST",
      path: "/webhooks/form",
      status: 400,
      latency: 11,
      body: "name=Ada&email=ada%40example.com&subscribe=true",
    },
    {
      method: "POST",
      path: "/webhooks/binary",
      status: 202,
      latency: 27,
      body: "AAECAwQFBgcICQ==",
      body_encoding: "base64",
    },
  ];

  const template = templates[index % templates.length]!;
  const method = template.method ?? "POST";
  const path = template.path ?? "/webhook";
  const id = `sim-${String(index).padStart(4, "0")}`;

  return {
    id,
    timestamp: iso(index * 1500),
    method,
    path,
    headers: {
      "content-type": template.body_encoding
        ? "application/octet-stream"
        : path.endsWith("/health")
          ? "text/plain"
          : path.endsWith("/form")
            ? "application/x-www-form-urlencoded"
            : "application/json",
      "user-agent": "Stripe/1.0 (+https://stripe.com)",
      "x-forwarded-for": `203.0.113.${(index % 250) + 1}`,
      "x-webhook-id": `wh_${index}`,
    },
    query_params: index % 2 === 0 ? { v: "2", source: "sim" } : {},
    body: template.body ?? "",
    ...(template.body_encoding ? { body_encoding: template.body_encoding } : {}),
    response: {
      status: template.status,
      headers: { "content-type": "application/json", "x-served-by": "sim-origin" },
      body: jsonBody({ received: true, id }),
      latency_ms: template.latency,
    },
  };
}

export class SimBridge implements Bridge {
  private store = new Map<string, CapturedRequestJson>();
  private listeners = new Set<Listener>();
  private tunnel: TunnelInfo | null = null;
  private interceptor: InterceptorInfo | null = null;

  /** Preload the store (returned by `listRequests`, newest first). */
  seed(count: number): CapturedRequestJson[] {
    const records: CapturedRequestJson[] = [];
    for (let i = count - 1; i >= 0; i -= 1) {
      const record = makeRecord(i);
      this.store.set(record.id, record);
      records.push(record);
    }
    return records;
  }

  /** Fire a `webhook-received` event, exactly like the Rust emitter. */
  emit(record: CapturedRequestJson): void {
    this.store.set(record.id, record);
    for (const listener of this.listeners) listener(record);
  }

  async invoke<T>(cmd: string, args?: Record<string, unknown>): Promise<T> {
    switch (cmd) {
      case "tunnel_status":
        return this.tunnel as T;
      case "interceptor_status":
        return this.interceptor as T;
      case "start_interceptor": {
        const targetPort = Number(args?.targetPort ?? 3000);
        this.interceptor = {
          proxy_port: PROXY_PORT,
          proxy_url: `http://127.0.0.1:${PROXY_PORT}`,
          target_port: targetPort,
        };
        return this.interceptor as T;
      }
      case "start_tunnel": {
        const localPort = Number(args?.localPort ?? PROXY_PORT);
        this.tunnel = { public_url: SIM_PUBLIC_URL, pid: 4321, local_port: localPort };
        return this.tunnel as T;
      }
      case "stop_tunnel":
        this.tunnel = null;
        return undefined as T;
      case "stop_interceptor":
        this.interceptor = null;
        return undefined as T;
      case "get_captured_request":
        return (this.store.get(String(args?.id)) ?? null) as T;
      case "captured_request_count":
        return this.store.size as T;
      case "replay_request": {
        const record = this.store.get(String(args?.requestId));
        if (!record) {
          throw { kind: "unknown_request", message: `no captured request with id \`${args?.requestId}\`` };
        }
        if (!this.interceptor) {
          throw { kind: "not_running", message: "start the interceptor before replaying a captured request" };
        }
        const replay: ReplayResponse = {
          status: record.response.status,
          headers: record.response.headers,
          body: record.response.body,
          ...(record.response.body_encoding ? { body_encoding: record.response.body_encoding } : {}),
          latency_ms: 6 + ((this.store.size * 7) % 33),
        };
        return replay as T;
      }
      default:
        throw { kind: "unknown_command", message: `simulator has no command \`${cmd}\`` };
    }
  }

  async listen<T>(event: string, handler: (payload: T) => void): Promise<Unlisten> {
    if (event !== "webhook-received") return () => undefined;
    const wrapped = (record: CapturedRequestJson) => handler(record as T);
    this.listeners.add(wrapped);
    return () => this.listeners.delete(wrapped);
  }

  async listRequests(): Promise<RequestSummary[]> {
    return [...this.store.values()]
      .sort((a, b) => (a.timestamp < b.timestamp ? 1 : -1))
      .map(toSummary);
  }
}
