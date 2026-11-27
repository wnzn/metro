// Local workspace tools: read-only workspace.list / workspace.read for a Space
// of this computer's own hosted Metro workspace, served by the packaged
// launcher (bridge routes `/v1/local-tools…`, crates/connector desktop_tools).
//
// - Only with the packaged launcher, and only while the session is this
//   computer's own local workspace: the bridge answers `available: false`
//   for any other server, and the card stays hidden.
// - Owner only, one Space at a time, turned on explicitly. The bridge pairs
//   with a guarded request that never replaces a Space's existing runner.
// - The runner token never reaches this UI: the bridge keeps it on this
//   computer. The bridge bearer lives in ../../device/env.ts's closure.

import { signal } from "nsh";
import { readBridgeEnv } from "../../device/env";
import type { BridgeEndpoint } from "../../device/env";
import { BridgeError, scrub } from "../../device/bridge";
import { onSessionEnd, sessionGuard } from "../../store";

/** `failed`: didn't start, or stopped unexpectedly (the bridge restarts it); `detail` says why. */
export type LocalToolsWorker = "starting" | "online" | "offline" | "revoked" | "failed" | "stopped";

export type LocalToolsSpace = {
  spaceId: string;
  enabled: boolean;
  /** `on`: paired from this computer; `revoked`: the server no longer accepts that pairing; `off`. */
  state: "on" | "revoked" | "off";
  worker: LocalToolsWorker;
  detail: string | null;
  /** This Space's private folder on this computer: the only place the tools read. */
  folder: string;
  /** Nonsecret revision of the exact local record; older launchers lack it. */
  pairingRevision?: string;
};

export type LocalToolsDisableTarget = { spaceId: string; accountId: string; revision: string };

/** `problem`: why the bridge couldn't serve every pairing on this computer (null: it does). */
export type LocalToolsStatus = { available: boolean; reason: string | null; problem: string | null; spaces: LocalToolsSpace[] };

const isObj = (v: unknown): v is Record<string, unknown> => typeof v === "object" && v !== null && !Array.isArray(v);
const str = (v: unknown): string | null => (typeof v === "string" ? v : null);
const WORKERS: readonly string[] = ["starting", "online", "offline", "revoked", "failed", "stopped"];

export function parseSpace(v: unknown): LocalToolsSpace {
  if (!isObj(v) || typeof v.space_id !== "string" || typeof v.folder !== "string") {
    throw new BridgeError("protocol", "Metro's device helper sent local tools data this app doesn't understand.");
  }
  const state = v.state === "on" || v.state === "revoked" ? v.state : "off";
  const worker = WORKERS.includes(String(v.worker)) ? (v.worker as LocalToolsWorker) : "stopped";
  const detail = str(v.detail);
  const revision = v.pairing_revision;
  return { spaceId: v.space_id, enabled: v.enabled === true, state, worker, detail: detail === null ? null : scrub(detail), folder: v.folder,
    pairingRevision: typeof revision === "string" && /^(0|[1-9][0-9]{0,18})$/.test(revision) ? revision : undefined };
}

export function parseStatus(v: unknown): LocalToolsStatus {
  if (!isObj(v) || typeof v.available !== "boolean") {
    throw new BridgeError("protocol", "Metro's device helper sent local tools data this app doesn't understand.");
  }
  const reason = str(v.reason);
  const problem = str(v.problem);
  return {
    available: v.available,
    reason: reason === null ? null : scrub(reason),
    problem: problem === null ? null : scrub(problem),
    spaces: Array.isArray(v.spaces) ? v.spaces.map(parseSpace) : [],
  };
}

const SPACE_ID_RE = /^[a-z0-9][a-z0-9_-]{0,63}$/;

type BridgeMethod = "GET" | "POST" | "PUT" | "DELETE";

type Client = {
  /** The bridge call itself, for this computer's personal runner (../installedRunner). */
  call(method: BridgeMethod, path: string, body?: unknown): Promise<unknown>;
  status(): Promise<LocalToolsStatus>;
  enable(spaceId: string): Promise<LocalToolsSpace>;
  disable(target: LocalToolsDisableTarget): Promise<LocalToolsSpace>;
};

function createClient(endpoint: BridgeEndpoint): Client {
  async function call(method: BridgeMethod, path: string, body?: unknown): Promise<unknown> {
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
        signal: AbortSignal.timeout(30_000),
      });
    } catch {
      throw new BridgeError("unreachable", "Metro's device helper on this computer didn't answer.");
    }
    let data: unknown = null;
    try {
      const text = await response.text();
      data = text.trim() ? JSON.parse(text) : null;
    } catch {
      throw new BridgeError("protocol", `Metro's device helper answered HTTP ${response.status} with something that isn't JSON.`, response.status);
    }
    if (!response.ok) {
      const e = isObj(data) && isObj(data.error) ? data.error : null;
      const code = (e && str(e.code)) || "protocol";
      const message = (e && str(e.message)) || `Metro's device helper refused the request (HTTP ${response.status}).`;
      throw new BridgeError(code, scrub(message), response.status);
    }
    return data;
  }
  const seg = (sid: string) => {
    if (!SPACE_ID_RE.test(sid)) throw new BridgeError("invalid", "This Space can't use local tools.");
    return `/v1/local-tools/spaces/${sid}`;
  };
  return {
    call,
    status: async () => parseStatus(await call("GET", "/v1/local-tools")),
    enable: async (sid) => parseSpace(await call("POST", `${seg(sid)}/enable`, {})),
    // Do not send a confirmation to an older unguarded handler.
    disable: async (target) => parseSpace(await call("POST", `${seg(target.spaceId)}/disable-if-current`, {
      account_id: target.accountId, pairing_revision: target.revision,
    })),
  };
}

const env = readBridgeEnv();
const client: Client | null = env.present ? createClient(env.endpoint) : null;

/**
 * A request to the packaged launcher's bridge on this computer, under
 * `/v1/local-tools/runner` only. Never reaches a Metro server: the bridge
 * answers, using the session it already holds. Null without the launcher.
 */
export function localRunnerCall(): ((method: BridgeMethod, path: string, body?: { runner_id: string } | { path: string }) => Promise<unknown>) | null {
  if (!client) return null;
  return (method, path, body) => {
    if (path !== "/v1/local-tools/runner" && !path.startsWith("/v1/local-tools/runner/")) {
      return Promise.reject(new BridgeError("invalid", "Not a local runner request."));
    }
    return client.call(method, path, method === "GET" ? undefined : body ?? {});
  };
}

/** The bridge's answer, or null before it answered / without the launcher. */
export const localTools = signal<LocalToolsStatus | null>(null);
export const localToolsBusy = signal<null | "enable" | "disable">(null);
export const localToolsError = signal<string | null>(null);
export const localToolsNotice = signal<string | null>(null);
let statusSeq = 0;

/** The entry for one Space, or null when local tools are off for it. */
export function localToolsFor(spaceId: string | undefined): LocalToolsSpace | null {
  if (!spaceId) return null;
  return localTools()?.spaces.find((s) => s.spaceId === spaceId) ?? null;
}

function message(error: unknown, what: string): string {
  if (error instanceof BridgeError) return error.message;
  return `${what} didn't work.`;
}

/** Only an authoritative GET may publish status, in the shared read order. */
export async function loadLocalTools(spaceId: string, stillOpen: () => boolean): Promise<LocalToolsStatus | undefined> {
  if (!client) return;
  const live = sessionGuard(spaceId);
  const seq = ++statusSeq;
  const latest = () => seq === statusSeq && live() && stillOpen();
  try {
    const next = await client.status();
    if (!latest()) return;
    localTools.set(next);
    return next;
  } catch {
    // Unknown means hidden: the card never guesses.
    if (latest()) localTools.set(null);
  }
}

export async function enableLocalTools(spaceId: string, stillOpen: () => boolean, onAck: () => void): Promise<void> {
  if (client) await changeLocalTools("enable", spaceId, stillOpen, () => client!.enable(spaceId), onAck);
}

export async function disableLocalTools(target: LocalToolsDisableTarget, stillOpen: () => boolean, onAck: () => void): Promise<void> {
  if (client) await changeLocalTools("disable", target.spaceId, stillOpen, () => client!.disable(target), onAck);
}

async function changeLocalTools(
  action: "enable" | "disable",
  spaceId: string,
  stillOpen: () => boolean,
  write: () => Promise<LocalToolsSpace>,
  onAck: () => void,
) {
  if (!stillOpen() || localToolsBusy()) return;
  const live = sessionGuard(spaceId);
  const onPage = () => live() && stillOpen();
  const session = sessionGuard();
  statusSeq++;
  localToolsBusy.set(action);
  localToolsError.set(null);
  localToolsNotice.set(null);
  try {
    const receipt = await write();
    if (!live() || receipt.spaceId !== spaceId) return;
    if (receipt.state === (action === "enable" ? "on" : "off")) onAck();
    // An acknowledgement can be delayed across replacement or page reopen.
    // Only the originating page can publish bridge-status feedback.
    if (!onPage()) return;
    const current = await loadLocalTools(spaceId, onPage);
    if (!onPage() || !current?.available || localTools() !== current) return;
    const entry = current.spaces.find((s) => s.spaceId === spaceId);
    if (action === "enable" && entry?.state === "on" && entry.pairingRevision === receipt.pairingRevision) {
      localToolsNotice.set("Local tools are on for this Space.");
    } else if (action === "disable" && !entry) {
      localToolsNotice.set(receipt.detail ?? "Local tools are off. The files in the folder stay on this computer.");
    }
  } catch (error) {
    if (!onPage()) return;
    localToolsError.set(message(error, action === "enable" ? "Turning on local tools" : "Turning off local tools"));
    if (error instanceof BridgeError && error.code === "pairing_changed") await loadLocalTools(spaceId, onPage);
  } finally {
    if (session()) localToolsBusy.set(null);
  }
}

/** While a worker says `starting`: re-read at most this often, this many times. */
export const STARTING_POLL_MS = 750;
export const STARTING_POLL_MAX = 20;

// At most one follow-up poller, app-wide. Starting a new one ends the old.
let pollGen = 0;

/**
 * Re-reads the status until `spaceId`'s worker leaves `starting`, at most
 * STARTING_POLL_MAX times, one request at a time. `stillValid` is checked
 * before every request and before applying an answer (the view is still
 * open for the same Space, owner and session). Returns the stop function.
 */
export function followStarting(spaceId: string, stillValid: () => boolean): () => void {
  const gen = ++pollGen;
  const scope = sessionGuard(spaceId);
  let timer: number | undefined;
  let left = STARTING_POLL_MAX;
  const live = () => gen === pollGen && scope() && stillValid();
  const tick = async () => {
    timer = undefined;
    if (!client || !live() || left-- <= 0) return;
    const seq = ++statusSeq;
    let next: LocalToolsStatus;
    try {
      next = await client.status();
    } catch {
      return; // unknown: stop; the next visit reads again
    }
    if (!live()) return;
    if (seq === statusSeq) localTools.set(next);
    const e = localToolsFor(spaceId);
    if (e && e.state === "on" && e.worker === "starting") timer = setTimeout(() => void tick(), STARTING_POLL_MS);
  };
  timer = setTimeout(() => void tick(), STARTING_POLL_MS);
  return () => {
    if (gen === pollGen) pollGen++;
    clearTimeout(timer);
  };
}

export function clearLocalToolsMessages() {
  localToolsError.set(null);
  localToolsNotice.set(null);
}

onSessionEnd(() => {
  localTools.set(null);
  localToolsBusy.set(null);
  clearLocalToolsMessages();
});
