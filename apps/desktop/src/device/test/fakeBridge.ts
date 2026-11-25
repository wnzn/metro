// A stand-in for the packaged launcher's desktop bridge (contract v1), next to
// test/fakeServer.ts, through nsh test's network stubs. It plays the bridge's
// side of pairing on the fake server's state: it "issues" the device token
// itself, so the UI never gets one. Fake values only, never real ones.

import { header, TOKENS } from "../../../test/fakeServer";
import type { fakeServer } from "../../../test/fakeServer";

export const BRIDGE = "http://127.0.0.1:47123";
/** A fake per-launch bearer: 64 hex characters, as the launcher makes them. */
export const BRIDGE_TOKEN = "ab".repeat(32);
/** What the bridge "gets" from the server; the UI must never show it. */
export const BRIDGE_DEVICE_SECRET = "metro_pc_bridge_secret";
export const LOCAL_MODELS = ["local-small", "local-large"];

/** What nsh/test's `open` gives the app as its launcher-set environment. */
export const bridgeEnv = (url = BRIDGE, token = BRIDGE_TOKEN) => ({ process: { "default.env": { METRO_DESKTOP_BRIDGE_URL: url, METRO_DESKTOP_BRIDGE_TOKEN: token } } });

type F = ReturnType<typeof fakeServer>;
type Req = { method: string; url: string; headers: Record<string, string>; body?: string | null };
type Local = {
  connection_id: string;
  kind: string;
  base_url: string | null;
  models: string[];
  credential: string;
  worker: string;
  detail: string | null;
  login: null | {
    state: "waiting" | "done" | "failed"; browser_ready: boolean; before_browser: boolean;
    browser: "opening" | "opened" | "failed" | "not_requested"; browser_error: string | null;
    stage: string | null; error: string | null;
  };
};

const ENDPOINT = ["device-openai-compatible", "device-systemone", "device-embeddings", "device-embeddinggemma-2"];

export function fakeBridge(f: F, opts: { instance?: "primary" | "secondary"; kinds?: string[] } = {}) {
  const state = {
    instance: opts.instance ?? "primary",
    kinds: opts.kinds ?? ["device-openai-oauth", "device-claude-code", "device-openai-compatible", "device-systemone", "device-embeddings"],
    session: null as null | { server_url: string; account_id: string },
    connections: new Map<string, Local>(),
    /** Every request, with its headers and parsed body. */
    log: [] as { method: string; path: string; headers: Record<string, string>; body: any }[],
    /** Vendor keys received, by connection id (the bridge keeps them on "this device"). */
    keys: new Map<string, string | null>(),
    /** Answer every request with this error instead (status, code). */
    failWith: null as null | { status: number; code: string; message: string },
    /** Ready workers stay "starting" until `serve(id)`, as a real worker does while it first reaches the server. */
    slowStart: false,
    served: new Set<string>(),
    /** This computer's monitoring events; null: an older helper without them (404). */
    monitoring: null as null | unknown[],
    /** Delay on NSH's stub clock, without blocking other route handlers. */
    monitoringDelay: 0,
  };

  const ok = (body: unknown, status = 200) => ({ status, headers: { "cache-control": "no-store" }, body });
  const err = (status: number, code: string, message: string) => ({ status, body: { error: { code, message } } });
  const status = () => ({
    bridge: 1,
    version: "0.2.0-test",
    instance: state.instance,
    kinds: state.kinds,
    session: state.session,
    connections: state.session ? [...state.connections.values()] : [],
    local: "unavailable",
  });
  const account = (token: string) => Object.keys(TOKENS).find((u) => TOKENS[u] === token) ?? null;
  const ready = (c: Local) => {
    const credOk = c.credential === "signed_in" || c.credential === "key_saved" || c.credential === "no_key";
    c.worker = credOk && c.models.length ? (state.slowStart && !state.served.has(c.connection_id) ? "starting" : "online") : "needs_setup";
    if (c.worker === "online") f.state.online.add(c.connection_id);
    else f.state.online.delete(c.connection_id);
  };

  function handle(req: Req) {
    const url = new URL(req.url);
    const path = url.pathname;
    let body: any = null;
    try {
      body = req.body ? JSON.parse(req.body) : null;
    } catch {
      body = req.body;
    }
    state.log.push({ method: req.method, path, headers: req.headers, body });
    if (header(req as any, "origin") !== undefined) return err(403, "origin_refused", "Origin refused.");
    if (header(req as any, "authorization") !== `Bearer ${BRIDGE_TOKEN}`) return err(401, "unauthorized", "Unauthorized.");
    if (state.failWith) return err(state.failWith.status, state.failWith.code, state.failWith.message);

    if (path === "/v1/status" && req.method === "GET") return ok(status());
    if (path === "/v1/monitoring/events" && req.method === "GET") {
      if (!state.monitoring) return err(404, "not_found", "Unknown path.");
      return { ...ok({ events: state.monitoring }), delay: state.monitoringDelay };
    }
    if (path === "/v1/session") {
      if (req.method === "DELETE") {
        state.session = null;
        for (const c of state.connections.values()) f.state.online.delete(c.connection_id);
        return ok(status());
      }
      if (state.instance === "secondary") return err(409, "other_instance", "Another Metro window serves device connections.");
      const who = account(body?.session_token ?? "");
      if (!who) return err(422, "session_rejected", "The server refused this session.");
      state.session = { server_url: body.server_url, account_id: who };
      for (const c of state.connections.values()) ready(c);
      return ok(status());
    }
    const m = /^\/v1\/connections\/([^/]+)\/([a-z-]+|login\/open)$/.exec(path);
    if (!m) return err(404, "not_found", "Unknown path.");
    if (!state.session) return err(409, "no_session", "Sign in to Metro first.");
    const id = decodeURIComponent(m[1]);
    const server = f.state.connections.get(id);
    if (!server || server.owner !== state.session.account_id) return err(404, "not_your_connection", "Not your connection.");
    const c = state.connections.get(id);
    switch (`${req.method} ${m[2]}`) {
      case "POST pair": {
        if (server.provider_id !== body?.kind) return err(422, "kind_mismatch", "Kind mismatch.");
        if (ENDPOINT.includes(body.kind) && !body.base_url) return err(422, "invalid", "base_url is required.");
        server.deviceToken = BRIDGE_DEVICE_SECRET; // the bridge asked the server itself
        const next: Local = {
          connection_id: id,
          kind: body.kind,
          base_url: ENDPOINT.includes(body.kind) ? body.base_url : null,
          models: [],
          credential: ENDPOINT.includes(body.kind) ? "missing" : "signed_out",
          worker: "needs_setup",
          detail: null,
          login: null,
        };
        state.connections.set(id, next);
        return ok(next);
      }
      case "POST rotate":
        if (!c) return err(404, "not_paired", "Not paired here.");
        server.deviceToken = `${BRIDGE_DEVICE_SECRET}_rotated`;
        return ok(c);
      case "POST credentials": {
        if (!c) return err(404, "not_paired", "Not paired here.");
        if (c.kind === "device-openai-oauth") {
          c.login = { state: "waiting", browser_ready: true, before_browser: false, browser: "opened", browser_error: null, stage: null, error: null };
          return ok(c);
        }
        if (body?.no_api_key) {
          state.keys.set(id, null);
          c.credential = "no_key";
        } else {
          state.keys.set(id, body?.api_key ?? "");
          c.credential = "key_saved";
        }
        ready(c);
        return ok(c);
      }
      case "POST login/open":
        if (!c?.login?.browser_ready || c.login.browser === "opening") return err(409, "login_preparing", "Sign-in is not ready.");
        c.login.browser = "opened";
        c.login.browser_error = null;
        return ok(c);
      case "DELETE login":
        if (!c) return err(404, "not_paired", "Not paired here.");
        c.login = null;
        return ok(c);
      case "DELETE credentials":
        if (!c) return err(404, "not_paired", "Not paired here.");
        c.credential = ENDPOINT.includes(c.kind) ? "missing" : "signed_out";
        state.keys.delete(id);
        ready(c);
        return ok(c);
      case "GET available-models":
        if (!c) return err(404, "not_paired", "Not paired here.");
        return ok({ models: LOCAL_MODELS });
      case "PUT models":
        if (!c) return err(404, "not_paired", "Not paired here.");
        c.models = body.models;
        ready(c);
        return ok(c);
      case "PUT endpoint":
        if (!c) return err(404, "not_paired", "Not paired here.");
        if (c.base_url !== body.base_url) {
          c.base_url = body.base_url;
          c.credential = "missing";
          state.keys.delete(id);
        }
        ready(c);
        return ok(c);
      case "POST forget":
        state.connections.delete(id);
        state.keys.delete(id);
        f.state.online.delete(id);
        if (body?.revoke) server.deviceToken = null;
        return ok({ forgotten: true });
    }
    return err(405, "method_not_allowed", "Wrong method.");
  }

  /** Finishes (or fails) a browser sign-in, as the user would in the browser. */
  const finishLogin = (id: string, success = true) => {
    const c = state.connections.get(id)!;
    c.login = { ...c.login!, state: success ? "done" : "failed", browser_ready: false, error: success ? null : "The browser sign-in was cancelled." };
    if (success) c.credential = "signed_in";
    ready(c);
  };

  /** The starting worker reached the server and serves now (nothing tells the UI; it has to look). */
  const serve = (id: string) => {
    state.served.add(id);
    const c = state.connections.get(id);
    if (c) ready(c);
  };

  const network: Record<string, (req: Req) => unknown> = {};
  for (const method of ["GET", "POST", "PUT", "DELETE"]) network[`${method} ${BRIDGE}/v1/*`] = handle;
  return { state, network, finishLogin, serve, calls: (path: string, method?: string) => state.log.filter((l) => l.path === path && (!method || l.method === method)) };
}
