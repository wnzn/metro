// A typed client for the desktop bridge (contract v1): the loopback HTTP API
// of the packaged Metro launcher, which runs this computer's device
// connections. The UI never sees a device token: the bridge asks the Metro
// server for it (pair, rotate) and keeps it on this device. Vendor keys typed
// here go to the bridge only, and are bound there to their endpoint.
//
// Every request: the per-launch bearer, `redirect: "error"`, a deadline, and
// JSON. Errors carry the bridge's short safe text plus its `code`, scrubbed
// once more so a token-shaped string never reaches the UI.

import type { BridgeEndpoint } from "./env";

export type DeviceKind =
  | "device-openai-oauth"
  | "device-claude-code"
  | "device-openai-compatible"
  | "device-systemone"
  | "device-embeddings"
  | "device-embeddinggemma-2";

/** Endpoint kinds take a base URL and an optional key; the others a sign-in or a key. */
export const ENDPOINT_KINDS: readonly string[] = ["device-openai-compatible", "device-systemone", "device-embeddings", "device-embeddinggemma-2"];
export const isEndpointKind = (kind: string) => ENDPOINT_KINDS.includes(kind);

export type Credential = "signed_in" | "signed_out" | "key_saved" | "no_key" | "missing";
export type Worker = "stopped" | "needs_setup" | "starting" | "online" | "offline" | "unauthorized" | "error";
type LoginStage = "storage" | "discovery" | "callback" | "waiting" | "response" | "exchange";
export type LoginState = {
  state: "waiting" | "done" | "failed";
  browserReady: boolean;
  browser: "not_requested" | "opening" | "opened" | "failed" | null;
  browserError: string | null;
  stage: LoginStage | null;
  beforeBrowser: boolean;
  error: string | null;
};

/** One connection paired on this computer, for the current session. */
export type LocalConnection = {
  connectionId: string;
  kind: string;
  baseUrl: string | null;
  models: string[];
  credential: Credential;
  worker: Worker;
  detail: string | null;
  login: LoginState | null;
};

export type LocalState = "unavailable" | "stopped" | "starting" | "running" | "failed";

export type BridgeStatus = {
  version: string;
  /** `secondary`: another Metro window of this user already serves device connections. */
  instance: "primary" | "secondary";
  kinds: string[];
  session: { serverUrl: string; accountId: string } | null;
  connections: LocalConnection[];
  /** The desktop-hosted solo workspace, when the bridge reports it. */
  local: LocalState | null;
};

export type BridgeErrorCode =
  | "no_session"
  | "other_instance"
  | "not_paired"
  | "not_your_connection"
  | "kind_mismatch"
  | "invalid"
  | "session_rejected"
  | "server_unreachable"
  | "server_error"
  | "login_in_progress"
  | "unsupported"
  | "unauthorized"
  | "local_unavailable"
  | "local_failed"
  /** The bridge didn't answer (not running, timed out). */
  | "unreachable"
  /** It answered something this client doesn't understand. */
  | "protocol"
  | (string & {});

export class BridgeError extends Error {
  constructor(
    readonly code: BridgeErrorCode,
    message: string,
    readonly status = 0,
  ) {
    super(message);
    this.name = "BridgeError";
  }
}

/** Drops anything shaped like a bearer or a vendor key from text shown to the user. */
export function scrub(text: string): string {
  return text
    .replace(/\b[0-9a-f]{32,}\b/gi, "[hidden]")
    .replace(/\b(sk|rk|pk)-[A-Za-z0-9_-]{6,}/g, "[hidden]")
    .replace(/\bmetro_[a-z0-9_]{6,}/gi, "[hidden]")
    .replace(/Bearer\s+\S+/gi, "Bearer [hidden]");
}

const isObj = (v: unknown): v is Record<string, unknown> => typeof v === "object" && v !== null && !Array.isArray(v);
const str = (v: unknown): string | null => (typeof v === "string" ? v : null);
const strs = (v: unknown): string[] => (Array.isArray(v) ? v.filter((x): x is string => typeof x === "string") : []);

function parseLogin(v: unknown): LoginState | null {
  if (!isObj(v)) return null;
  const state = v.state === "waiting" || v.state === "done" || v.state === "failed" ? v.state : null;
  if (!state) return null;
  const { stage, browser } = v;
  const safe = (text: unknown) => typeof text === "string" ? scrub(text.replace(/https?:\/\/\S+/gi, "[link hidden]")) : null;
  return {
    state, error: safe(v.error), browserError: safe(v.browser_error),
    browserReady: v.browser_ready === true, beforeBrowser: v.before_browser === true,
    stage: stage === "storage" || stage === "discovery" || stage === "callback" || stage === "waiting" || stage === "response" || stage === "exchange" ? stage : null,
    browser: browser === "opening" || browser === "opened" || browser === "failed" || browser === "not_requested" ? browser : null,
  };
}

export function parseConnection(v: unknown): LocalConnection {
  if (!isObj(v) || typeof v.connection_id !== "string" || typeof v.kind !== "string") {
    throw new BridgeError("protocol", "Metro's device helper sent a connection this app doesn't understand.");
  }
  const detail = str(v.detail);
  return {
    connectionId: v.connection_id,
    kind: v.kind,
    baseUrl: str(v.base_url),
    models: strs(v.models),
    credential: (str(v.credential) ?? "missing") as Credential,
    worker: (str(v.worker) ?? "error") as Worker,
    detail: detail === null ? null : scrub(detail),
    login: parseLogin(v.login),
  };
}

export function parseStatus(v: unknown): BridgeStatus {
  if (!isObj(v) || v.bridge !== 1) throw new BridgeError("protocol", "Metro's device helper speaks a version this app doesn't know.");
  const session = isObj(v.session) && typeof v.session.server_url === "string" && typeof v.session.account_id === "string"
    ? { serverUrl: v.session.server_url, accountId: v.session.account_id }
    : null;
  const local = str(v.local);
  return {
    version: str(v.version) ?? "",
    instance: v.instance === "secondary" ? "secondary" : "primary",
    kinds: strs(v.kinds),
    session,
    connections: Array.isArray(v.connections) ? v.connections.map(parseConnection) : [],
    local: local === "unavailable" || local === "stopped" || local === "starting" || local === "running" || local === "failed" ? local : null,
  };
}

export type CredentialsBody = { open_browser?: boolean } | { api_key: string } | { no_api_key: true };

export type Bridge = {
  status(): Promise<BridgeStatus>;
  /** Binds the signed-in session: the bridge learns the account and starts its paired workers. */
  bindSession(serverUrl: string, sessionToken: string): Promise<BridgeStatus>;
  /** Stops every worker and drops the session (sign-out, account switch). */
  unbindSession(): Promise<BridgeStatus>;
  pair(connectionId: string, body: { kind: string; base_url?: string; models?: string[] }): Promise<LocalConnection>;
  rotate(connectionId: string): Promise<LocalConnection>;
  setCredentials(connectionId: string, body: CredentialsBody): Promise<LocalConnection>;
  clearCredentials(connectionId: string): Promise<LocalConnection>;
  openLogin(connectionId: string): Promise<LocalConnection>;
  cancelLogin(connectionId: string): Promise<LocalConnection>;
  availableModels(connectionId: string): Promise<string[]>;
  setModels(connectionId: string, models: string[]): Promise<LocalConnection>;
  setEndpoint(connectionId: string, baseUrl: string): Promise<LocalConnection>;
  forget(connectionId: string, revoke: boolean): Promise<void>;
  /**
   * Starts (or reuses) the desktop-hosted solo workspace and returns its URL
   * and a session bearer for it, in memory. For the local-workspace lane:
   * connect with them, never store or show the token.
   */
  openLocalWorkspace(): Promise<{ serverUrl: string; token: string }>;
};

type Method = "GET" | "POST" | "PUT" | "DELETE";

/** Generous: pair, session and rotate make the bridge call the Metro server. */
const DEADLINE_MS = 30_000;

export function createBridge(endpoint: BridgeEndpoint): Bridge {
  async function call(method: Method, path: string, body?: unknown, deadline = DEADLINE_MS): Promise<unknown> {
    let response: Response;
    try {
      response = await fetch(endpoint.url + path, {
        method,
        headers: {
          Accept: "application/json",
          Authorization: endpoint.authorization(),
          ...(body === undefined ? {} : { "Content-Type": "application/json" }),
        },
        body: body === undefined ? undefined : JSON.stringify(body),
        redirect: "error",
        signal: AbortSignal.timeout(deadline),
      });
    } catch {
      // Never echo the request (it may hold a key) or the platform error.
      throw new BridgeError("unreachable", "Metro's device helper on this computer didn't answer.");
    }
    let text = "";
    try {
      text = await response.text();
    } catch {
      throw new BridgeError("unreachable", "Metro's device helper on this computer stopped answering.");
    }
    let data: unknown = null;
    try {
      data = text.trim() ? JSON.parse(text) : null;
    } catch {
      throw new BridgeError("protocol", `Metro's device helper answered HTTP ${response.status} with something that isn't JSON.`, response.status);
    }
    if (!response.ok) {
      const e = isObj(data) && isObj(data.error) ? data.error : null;
      const code = (e && str(e.code)) || (response.status === 401 ? "unauthorized" : "protocol");
      const message = (e && str(e.message)) || `Metro's device helper refused the request (HTTP ${response.status}).`;
      throw new BridgeError(code, scrub(message), response.status);
    }
    return data;
  }

  const seg = (id: string) => `/v1/connections/${encodeURIComponent(id)}`;
  const conn = async (p: Promise<unknown>) => parseConnection(await p);

  return {
    status: async () => parseStatus(await call("GET", "/v1/status", undefined, 10_000)),
    bindSession: async (serverUrl, sessionToken) => parseStatus(await call("POST", "/v1/session", { server_url: serverUrl, session_token: sessionToken })),
    unbindSession: async () => parseStatus(await call("DELETE", "/v1/session")),
    pair: (id, body) => conn(call("POST", `${seg(id)}/pair`, body)),
    rotate: (id) => conn(call("POST", `${seg(id)}/rotate`, {})),
    setCredentials: (id, body) => conn(call("POST", `${seg(id)}/credentials`, body)),
    clearCredentials: (id) => conn(call("DELETE", `${seg(id)}/credentials`)),
    openLogin: (id) => conn(call("POST", `${seg(id)}/login/open`, {})),
    cancelLogin: (id) => conn(call("DELETE", `${seg(id)}/login`)),
    availableModels: async (id) => {
      const data = await call("GET", `${seg(id)}/available-models`);
      if (!isObj(data) || !Array.isArray(data.models)) throw new BridgeError("protocol", "Metro's device helper didn't list models.");
      return strs(data.models);
    },
    setModels: (id, models) => conn(call("PUT", `${seg(id)}/models`, { models })),
    setEndpoint: (id, baseUrl) => conn(call("PUT", `${seg(id)}/endpoint`, { base_url: baseUrl })),
    forget: async (id, revoke) => {
      await call("POST", `${seg(id)}/forget`, { revoke });
    },
    openLocalWorkspace: async () => {
      const data = await call("POST", "/v1/local-workspace/open", {}, 120_000);
      if (!isObj(data) || typeof data.server_url !== "string" || typeof data.token !== "string" || !data.token) {
        throw new BridgeError("protocol", "Metro's device helper didn't return the local workspace.");
      }
      return { serverUrl: data.server_url, token: data.token };
    },
  };
}
