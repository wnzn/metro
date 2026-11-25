// This computer's device connections, through the packaged launcher's
// desktop bridge (./bridge.ts). Pages read these signals and call these
// functions; nothing here knows about the UI.
//
// Without the launcher (`nsh run`, tests, a hand-started UI) `bridge` is null
// and the app keeps its manual metro-connector path for headless computers.
//
// Secrets
// - The bridge bearer stays in ./env.ts's closure.
// - The Metro session token reaches this module only as an argument of
//   `onSessionStart`, and goes straight to the bridge (POST /v1/session). It is
//   never kept here.
// - Device tokens never reach the UI: the bridge asks the server for them.
// - A vendor API key typed into the setup panel is held by that panel only
//   until it is sent to the bridge (or the panel closes), then dropped.
//
// Races: every answer is applied only if it is still the latest request of
// its kind and the session (store.ts `sessionGuard`) is unchanged.

import { batch, cleanup, computed, effect, signal, untracked } from "nsh";
import { connectedTo, me, onSessionEnd, onSessionStart, place, sessionGuard, settingsSection } from "../store";
import type { WebProvider } from "../api/webSearch";
import { BridgeError, createBridge, isEndpointKind } from "./bridge";
import type { Bridge, BridgeStatus, CredentialsBody, LocalConnection } from "./bridge";
import { readBridgeEnv } from "./env";
import type { BridgeEnv } from "./env";

export type { LocalConnection } from "./bridge";

// ------------------------------------------------------------------ the bridge

// Read once at startup. Tests give the app these variables through
// nsh/test's `modules: { process: { "default.env": … } }`.
const env: BridgeEnv = readBridgeEnv();
const bridge: Bridge | null = env.present ? createBridge(env.endpoint) : null;

/** The bridge client, or null without the packaged launcher. For other lanes (local workspace): never log what it returns. */
export function desktopBridge(): Bridge | null {
  return bridge;
}

/**
 * What the helper says about itself, read at startup (before any sign-in) and
 * with every status: its version, whether this window is the primary one, and
 * the desktop-hosted local workspace's state. Null until it answered, or
 * without the launcher. Holds no session data.
 */
export const helperInfo = signal<{ version: string; instance: "primary" | "secondary"; local: BridgeStatus["local"] } | null>(null);
const noteInfo = (s: BridgeStatus) => helperInfo.set({ version: s.version, instance: s.instance, local: s.local });

/** Re-reads helperInfo (e.g. for the local workspace's state). Never throws. */
export async function refreshHelperInfo(): Promise<void> {
  if (!bridge) return;
  try {
    noteInfo(await bridge.status());
  } catch {
    helperInfo.set(null);
  }
}
void refreshHelperInfo();

export type Helper =
  /** No launcher: a development or unpackaged UI. */
  | { phase: "absent"; reason: "absent" | "malformed" }
  | { phase: "checking" }
  /** Bound to this session and serving its device connections. */
  | { phase: "ready"; status: BridgeStatus }
  /** Another Metro window of this user serves device connections. */
  | { phase: "secondary"; status: BridgeStatus }
  /** The bridge answered, but couldn't bind this session (server unreachable, session refused). */
  | { phase: "unbound"; status: BridgeStatus | null; message: string }
  | { phase: "unreachable"; message: string };

const SIGNED_OUT: Helper = { phase: "unbound", status: null, message: "Sign in to Metro to use this computer's device connections." };

export const helper = signal<Helper>(env.present ? SIGNED_OUT : { phase: "absent", reason: env.reason });

/** The packaged helper serves this session: pairing happens in the app, never by hand. */
export const nativeDevices = computed(() => helper().phase === "ready");
/** The app runs with the launcher at all (ready or not): it never shows manual connector steps then. */
export const packaged = computed(() => helper().phase !== "absent");

/** This computer's paired connections for the session, by connection id. */
export const localConnections = computed<Record<string, LocalConnection>>(() => {
  const h = helper();
  const list = h.phase === "ready" ? h.status.connections : [];
  const out: Record<string, LocalConnection> = {};
  for (const c of list) out[c.connectionId] = c;
  return out;
});

/** Device kinds this helper can run. */
export const helperKinds = computed(() => {
  const h = helper();
  return h.phase === "ready" ? h.status.kinds : [];
});

// Session binding is serialized: an unbind (sign-out, account switch) always
// lands before the next bind is sent.
let chain: Promise<unknown> = Promise.resolve();
let bound = false;
/** Includes a dispatched POST whose response is still pending or failed. */
let bindDispatched = false;
/** A failed DELETE cannot be treated as proof that the previous binding ended. */
let releasePending = false;
let statusSeq = 0;

function serial<T>(job: () => Promise<T>): Promise<T> {
  const next = chain.then(job, job);
  chain = next.catch(() => {});
  return next;
}

export function webSearchBinding(server: string, account: string) {
  const h = helper();
  const s = h.phase === "ready" ? h.status.session : null;
  return s && s.accountId === account && s.serverUrl.replace(/\/+$/, "") === server.replace(/\/+$/, "") ? s : null;
}

/** Settings reads and local changes use the helper's current binding.
 * Keep them ordered with bind/unbind and reject queued work after a change. */
export function withWebSearchBridge<T>(server: string, account: string, job: (b: Bridge) => Promise<T>): Promise<T> {
  const current = sessionGuard();
  return serial(() => {
    if (!current() || !bridge || !webSearchBinding(server, account)) {
      throw new BridgeError("no_session", "This window isn't serving the selected account and server.");
    }
    return job(bridge);
  });
}

/** A slow vendor test never holds the binding queue. Its required generation
 * is compared at the helper before it can read any credentials or send data. */
export async function testWebSearchOnCurrentBinding(server: string, account: string, id: WebProvider) {
  const binding = webSearchBinding(server, account);
  if (!bridge || !binding) throw new BridgeError("no_session", "The web search session changed.");
  if (binding.generation === undefined) throw new BridgeError("unsupported", "Update Metro to test providers.");
  return bridge.testWebSearchProvider(id, binding.generation);
}

function message(error: unknown, what: string): string {
  if (error instanceof BridgeError) return error.message;
  return what;
}

/** Worker state per connection id, to tell which ones changed between two statuses. */
function workers(status: BridgeStatus | null): Record<string, string> {
  const out: Record<string, string> = {};
  for (const c of status?.connections ?? []) out[c.connectionId] = `${c.worker}|${c.credential}|${c.models.length}`;
  return out;
}

function applyStatus(status: BridgeStatus) {
  noteInfo(status);
  const h = helper();
  const before = workers(h.phase === "ready" ? h.status : null);
  const after = workers(status);
  // A worker that moved on (starting -> online): the server's paired/online view follows.
  const moved = Object.keys(after).filter((id) => id in before && before[id] !== after[id]);
  if (moved.length) void Promise.resolve().then(() => moved.forEach(notify));
  const server = connectedTo();
  if (status.instance === "secondary") {
    helper.set({ phase: "secondary", status });
    return;
  }
  // Only ever show what belongs to this server's session.
  if (!status.session || !server || status.session.accountId !== me() ||
      status.session.serverUrl.replace(/\/+$/, "") !== server.replace(/\/+$/, "")) {
    helper.set({ phase: "unbound", status, message: "Metro's device helper isn't signed in to this server yet." });
    return;
  }
  helper.set({ phase: "ready", status });
}

/** Replaces one connection in the last status (after a connection endpoint answered). */
function applyConnection(c: LocalConnection) {
  const h = helper();
  if (h.phase !== "ready") return;
  statusSeq++; // an older status answer must not undo this
  const rest = h.status.connections.filter((x) => x.connectionId !== c.connectionId);
  helper.set({ phase: "ready", status: { ...h.status, connections: [...rest, c] } });
}

function dropConnection(id: string) {
  const h = helper();
  if (h.phase !== "ready") return;
  statusSeq++;
  helper.set({ phase: "ready", status: { ...h.status, connections: h.status.connections.filter((x) => x.connectionId !== id) } });
}

/** Reloads the helper's status for this session. */
export async function refreshHelper(): Promise<void> {
  const b = bridge;
  if (!b || !bound) return;
  const seq = ++statusSeq;
  const ok = sessionGuard();
  try {
    const status = await b.status();
    if (!ok() || seq !== statusSeq) return;
    applyStatus(status);
  } catch (error) {
    if (!ok() || seq !== statusSeq) return;
    helper.set({ phase: "unreachable", message: message(error, "Metro's device helper didn't answer.") });
  }
}

// ------------------------------------------------------------------ following a starting worker

/** How often, and how many times at most per stretch, a starting worker's status is re-read. */
export const STARTING_POLL_MS = 1500;
export const STARTING_POLL_MAX = 40;

/** Connections on this computer whose worker is still starting. */
const startingIds = computed(() => {
  const h = helper();
  return h.phase === "ready" ? h.status.connections.filter((c) => c.worker === "starting").map((c) => c.connectionId).sort().join(",") : "";
});
/** Model providers is on screen (its list, a row or the setup page): the only view that shows these states. */
const providersVisible = computed(() => place() === "settings" && settingsSection() === "providers");

// While a worker is starting and Model providers is open, re-read the bridge's
// GET /v1/status (no vendor or model call) until it settles: one request at a
// time, spaced, at most STARTING_POLL_MAX per stretch. Leaving the page,
// signing out or switching account (session change) ends it.
effect(() => {
  const ids = startingIds();
  if (!ids || !providersVisible()) return;
  const ok = untracked(() => sessionGuard());
  let left = STARTING_POLL_MAX;
  let timer: number | undefined;
  let stopped = false;
  const tick = async () => {
    timer = undefined;
    if (stopped || !ok() || left-- <= 0) return;
    await refreshHelper();
    if (!stopped && ok()) timer = setTimeout(() => void tick(), STARTING_POLL_MS);
  };
  timer = setTimeout(() => void tick(), STARTING_POLL_MS);
  cleanup(() => {
    stopped = true;
    if (timer !== undefined) clearTimeout(timer);
  });
});

function resetLocal() {
  statusSeq++;
  bound = false;
  bindDispatched = false;
  pairBusy.set(null);
  pairError.set(null);
  pairNotice.set(null);
  availableModels.set({});
}

async function releasePreviousBinding(b: Bridge) {
  if (!releasePending) return;
  const status = await b.unbindSession();
  if (status.session) throw new BridgeError("protocol", "Metro's device helper did not end the previous sign-in.");
  releasePending = false;
}

onSessionStart(({ baseUrl, token }) => {
  const b = bridge;
  if (!b) return;
  const ok = sessionGuard();
  helper.set({ phase: "checking" });
  void serial(async () => {
    if (!ok()) return;
    try {
      // Retry a failed cleanup on the person's next sign-in. Never overwrite
      // an uncertain old binding, even when the account and server match.
      await releasePreviousBinding(b);
    } catch {
      if (ok()) helper.set({ phase: "unbound", status: null, message: "Metro couldn't confirm that the previous device sign-in ended. Reopen Metro and sign in again." });
      return;
    }
    if (!ok()) return;
    bindDispatched = true;
    try {
      const status = await b.bindSession(baseUrl, token);
      if (!ok()) return;
      bound = true;
      statusSeq++;
      applyStatus(status);
    } catch (error) {
      if (!ok()) return;
      if (error instanceof BridgeError && error.code === "other_instance") {
        bound = true; // status still answers
        await refreshHelper();
        return;
      }
      if (error instanceof BridgeError && error.code === "unreachable") {
        helper.set({ phase: "unreachable", message: error.message });
        return;
      }
      helper.set({ phase: "unbound", status: null, message: message(error, "Metro's device helper couldn't sign in to this server.") });
    }
  });
});

onSessionEnd(() => {
  const b = bridge;
  releasePending ||= bindDispatched;
  resetLocal();
  if (!b) return;
  helper.set(SIGNED_OUT);
  if (releasePending) void serial(() => releasePreviousBinding(b)).catch(() => {
    // Retain the obligation; the launcher also stops workers on app exit.
  });
});

// ------------------------------------------------------------------ pairing and local actions

/** What is in flight: "pair:<id>", "signin:<id>", "models:<id>", "rotate:<id>", "signout:<id>", "forget:<id>", "endpoint:<id>", "list:<id>". */
export const pairBusy = signal<string | null>(null);
export const pairError = signal<string | null>(null);
export const pairNotice = signal<string | null>(null);
/** Models the provider lists from this computer, per connection. */
export const availableModels = signal<Record<string, { state: "loading" } | { state: "ok"; models: string[] } | { state: "error"; message: string }>>({});

/** Listeners told when this computer's pairing of a connection changed (settings.ts reloads the server status). */
const changed: ((connectionId: string) => void)[] = [];
export function onLocalPairingChanged(fn: (connectionId: string) => void) {
  changed.push(fn);
}
const notify = (id: string) => {
  for (const fn of changed) fn(id);
};

async function act<T>(busy: string, id: string, job: (b: Bridge) => Promise<T>, what: string): Promise<T | null> {
  const b = bridge;
  if (!b || pairBusy()) return null;
  const ok = sessionGuard();
  batch(() => {
    pairBusy.set(busy);
    pairError.set(null);
    pairNotice.set(null);
  });
  try {
    const result = await job(b);
    if (!ok()) return null;
    return result;
  } catch (error) {
    if (!ok()) return null;
    if (error instanceof BridgeError && (error.code === "not_paired" || error.code === "not_your_connection")) void refreshHelper();
    if (error instanceof BridgeError && error.code === "no_session") bound = false;
    pairError.set(`${what}: ${message(error, "Metro's device helper didn't answer")}`);
    return null;
  } finally {
    if (ok()) {
      pairBusy.set(null);
      notify(id);
    }
  }
}

/**
 * Pairs this computer for a connection, after the user agreed. The bridge
 * checks the connection is the user's own and of this kind, then gets the
 * device token from the Metro server itself.
 */
export async function pairHere(connectionId: string, kind: string, baseUrl?: string): Promise<boolean> {
  if (isEndpointKind(kind) && !baseUrl?.trim()) {
    pairError.set("Enter the endpoint's URL first.");
    return false;
  }
  const c = await act(`pair:${connectionId}`, connectionId, (b) => b.pair(connectionId, { kind, ...(isEndpointKind(kind) ? { base_url: baseUrl!.trim() } : {}) }), "Couldn't pair this computer");
  if (!c) return false;
  applyConnection(c);
  return true;
}

/** A new device token for this computer (the old one stops working). */
export async function rotateHere(connectionId: string): Promise<boolean> {
  const c = await act(`rotate:${connectionId}`, connectionId, (b) => b.rotate(connectionId), "Couldn't re-pair this computer");
  if (!c) return false;
  applyConnection(c);
  pairNotice.set("Re-paired. The previous device token no longer works.");
  return true;
}

/**
 * Signs in or saves a key on this computer. `body` holds the key only for
 * this call; the caller drops its copy whatever happens.
 */
export async function signInHere(connectionId: string, body: CredentialsBody): Promise<boolean> {
  const c = await act(`signin:${connectionId}`, connectionId, (b) => b.setCredentials(connectionId, body), "Couldn't save the sign-in");
  if (!c) return false;
  applyConnection(c);
  return true;
}

export async function signOutHere(connectionId: string): Promise<boolean> {
  const c = await act(`signout:${connectionId}`, connectionId, (b) => b.clearCredentials(connectionId), "Couldn't sign out on this computer");
  if (!c) return false;
  applyConnection(c);
  pairNotice.set("Signed out on this computer. It stopped serving this connection; sign in again to resume.");
  return true;
}

export async function manageLoginHere(id: string, action: "open" | "cancel"): Promise<void> {
  const ok = sessionGuard();
  const c = await act(`${action}:${id}`, id, (b) => action === "open" ? b.openLogin(id) : b.cancelLogin(id),
    action === "open" ? "Couldn't open the browser" : "Couldn't cancel sign-in");
  if (ok() && c) applyConnection(c);
}

export async function setEndpointHere(connectionId: string, baseUrl: string): Promise<boolean> {
  if (!baseUrl.trim()) {
    pairError.set("Enter the endpoint's URL.");
    return false;
  }
  const c = await act(`endpoint:${connectionId}`, connectionId, (b) => b.setEndpoint(connectionId, baseUrl.trim()), "Couldn't change the endpoint");
  if (!c) return false;
  applyConnection(c);
  return true;
}

/** Lists what the provider offers, asked from this computer. */
export async function listModelsHere(connectionId: string): Promise<void> {
  const b = bridge;
  if (!b) return;
  const ok = sessionGuard();
  availableModels.set({ ...availableModels(), [connectionId]: { state: "loading" } });
  try {
    const models = await b.availableModels(connectionId);
    if (!ok()) return;
    availableModels.set({ ...availableModels(), [connectionId]: { state: "ok", models } });
  } catch (error) {
    if (!ok()) return;
    availableModels.set({ ...availableModels(), [connectionId]: { state: "error", message: message(error, "The provider didn't list its models") } });
  }
}

/** The models this computer may serve for the connection (1–64). */
export async function approveModelsHere(connectionId: string, models: string[]): Promise<boolean> {
  const list = [...new Set(models.map((m) => m.trim()).filter(Boolean))];
  if (list.length === 0 || list.length > 64) {
    pairError.set("Approve between 1 and 64 models.");
    return false;
  }
  const c = await act(`models:${connectionId}`, connectionId, (b) => b.setModels(connectionId, list), "Couldn't save the approved models");
  if (!c) return false;
  applyConnection(c);
  return true;
}

/** Deletes everything for this connection on this computer; `revoke` also stops its device token on the server. */
export async function forgetHere(connectionId: string, revoke: boolean): Promise<boolean> {
  const done = await act(`forget:${connectionId}`, connectionId, async (b) => {
    await b.forget(connectionId, revoke);
    return true;
  }, "Couldn't forget this computer");
  if (!done) return false;
  dropConnection(connectionId);
  const { [connectionId]: _gone, ...rest } = availableModels();
  availableModels.set(rest);
  pairNotice.set(revoke ? "This computer no longer serves the connection, and its device token was revoked." : "Forgot this computer's sign-in and settings for the connection.");
  return true;
}

/** Best effort, no UI: used after the connection itself was deleted. */
export async function forgetQuietly(connectionId: string): Promise<void> {
  const b = bridge;
  if (!b || !localConnections()[connectionId]) return;
  try {
    await b.forget(connectionId, false);
  } catch {
    /* already gone on the server; the bridge drops it at its next session check */
  }
  dropConnection(connectionId);
}

export function clearPairingMessages() {
  pairError.set(null);
  pairNotice.set(null);
}

// ------------------------------------------------------------------ what a connection needs next on this computer

export type LocalStep = "pair" | "credentials" | "signing-in" | "models" | "ready";

export function localStep(c: LocalConnection | undefined): LocalStep {
  if (!c) return "pair";
  if (c.login?.state === "waiting") return "signing-in";
  if (c.credential === "signed_out" || c.credential === "missing") return "credentials";
  if (c.models.length === 0) return "models";
  return "ready";
}

/** A short state of a connection on this computer, for a badge. */
export function localBadge(c: LocalConnection | undefined): { tone: "success" | "warning" | "danger" | "neutral"; label: string } {
  if (!c) return { tone: "neutral", label: "Not set up here" };
  const step = localStep(c);
  if (step === "signing-in") return { tone: "warning", label: "Signing in…" };
  if (step === "credentials") return { tone: "warning", label: c.credential === "signed_out" ? "Signed out here" : "Needs a key here" };
  if (step === "models") return { tone: "warning", label: "Choose models" };
  switch (c.worker) {
    case "online":
      return { tone: "success", label: "Serving from here" };
    case "starting":
      return { tone: "neutral", label: "Starting…" };
    case "unauthorized":
      return { tone: "danger", label: "Re-pair needed" };
    case "error":
      return { tone: "danger", label: "Error here" };
    case "needs_setup":
      return { tone: "warning", label: "Needs setup here" };
    default:
      return { tone: "warning", label: "Not serving" };
  }
}
