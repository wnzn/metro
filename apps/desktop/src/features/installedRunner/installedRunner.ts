// This computer's personal registered runner, requested by the installer
// (crates/connector desktop_tools/runner.rs), for Settings › Runners.
//
// - Asked only of the packaged launcher's bridge (`/v1/local-tools/runner…`,
//   see ../localtools/localTools.ts `localRunnerCall`), never of a Metro
//   server. The bridge answers `available: true` only when the installer
//   asked for the runner *and* this session is the desktop's own hosted
//   server; anything else (no launcher, older launcher, a remote server, no
//   or rejected marker, an unreadable answer) hides the section.
// - Opening the page only reads. Reconnect (`{}`: rotates the recorded
//   runner id's token if it's still this person's, else creates a new
//   runner; never by name) and a Space's folder (which creates it on this
//   computer) are POSTs, sent only from an explicit press. Adopting a chosen
//   runner (`{runner_id}`) isn't offered here.
// - Metro never attaches it without an explicit owner action. The
//   folder is offered only for Spaces this user owns whose runner, by stable
//   id, is this one.
// - Separate from the per-Space dedicated "Local tools" switch.
// - Every answer is applied only if the session (server and account) and the
//   request generation are still the same.

import { batch, signal } from "nsh";
import { BridgeError, scrub } from "../../device/bridge";
import { beginSpaceRunnerObservation, onSpaceRunnerObserved } from "../../spaceRunnerObservations";
import { apiClient, connectedTo, me, onSessionEnd, sessionGuard, spaces } from "../../store";
import { localRunnerCall } from "../localtools/localTools";

export type InstalledRunnerState =
  | "idle"
  | "starting"
  | "online"
  | "offline"
  | "revoked"
  | "deleted"
  | "interrupted"
  | "failed"
  | "stopped";

export type InstalledRunner = {
  state: InstalledRunnerState;
  detail: string | null;
  /** The registered runner, once created (stable id and name). */
  runner: { id: string; name: string } | null;
  /**
   * The name an unconfirmed creation asked for, when no id is known: a hint
   * only. Names aren't unique, so it never identifies a runner.
   */
  unconfirmedName: string | null;
  tools: string[];
  canReconnect: boolean;
};

export type RunnerFolder = { spaceId: string; folder: string; attached: boolean };

const STATES: readonly string[] = ["idle", "starting", "online", "offline", "revoked", "deleted", "interrupted", "failed", "stopped"];
const SPACE_ID_RE = /^[a-z0-9][a-z0-9_-]{0,63}$/;
const isObj = (v: unknown): v is Record<string, unknown> => typeof v === "object" && v !== null && !Array.isArray(v);

/** The bridge's runner status, or null when there's nothing to show here. */
export function parseInstalledRunner(v: unknown): InstalledRunner | null {
  if (!isObj(v) || v.requested !== true || v.available !== true) return null;
  if (typeof v.state !== "string" || !STATES.includes(v.state)) return null;
  const r = v.runner;
  const runner = isObj(r) && typeof r.id === "string" && typeof r.name === "string" ? { id: r.id, name: scrub(r.name) } : null;
  const tools = Array.isArray(v.tools) ? v.tools.filter((t): t is string => typeof t === "string") : [];
  return {
    state: v.state as InstalledRunnerState,
    detail: typeof v.detail === "string" ? scrub(v.detail) : null,
    runner,
    unconfirmedName: runner === null && typeof v.unconfirmed_name === "string" ? scrub(v.unconfirmed_name) : null,
    tools,
    canReconnect: v.can_reconnect === true,
  };
}

export function parseFolder(v: unknown, sid: string): RunnerFolder {
  if (!isObj(v) || v.space_id !== sid || typeof v.folder !== "string" || typeof v.attached !== "boolean") {
    throw new BridgeError("protocol", "Metro's device helper sent folder data this app doesn't understand.");
  }
  return { spaceId: sid, folder: v.folder, attached: v.attached };
}

/** The status, or null (hidden: no launcher, not requested, not the own server, unknown). */
export const installedRunner = signal<InstalledRunner | null>(null);
/** A bridge id verified against the current account's personal runners. */
export const ownedInstalledRunner = signal<{ id: string; attachIfEmpty: boolean } | null>(null);
export const checkingInstalledScope = signal(false);
/** A provisioned (or temporarily unreachable) launcher worth watching for the offer. */
export const installedRunnerStatusWatchable = signal(false);
/** This session has had a supported local runner status, even if its bridge context is now missing. */
export const installedRunnerSeen = signal(false);
/** Owned Spaces whose runner is this one, by stable id. */
export const runnerSpaces = signal<{ id: string; name: string }[]>([]);
/** Serial of the last complete folder read for the current verified runner, if any. */
export const installedFolderSnapshot = signal<number | null>(null);
export const installedBusy = signal<null | "reconnect" | `folder:${string}`>(null);
export const installedError = signal<string | null>(null);
export const installedNotice = signal<string | null>(null);
export const folders = signal<Record<string, RunnerFolder>>({});

let gen = 0;
let folderRoot = 0;
let folderSnapshotSerial = 0;
const sessionKey = () => `${connectedTo() ?? ""}\n${me() ?? ""}`;

/** Still the same session and the latest request. */
function stillValid(): () => boolean {
  const mine = ++gen;
  const key = sessionKey();
  const ok = sessionGuard();
  return () => mine === gen && key === sessionKey() && ok();
}

function clearAttached() {
  folderRoot++;
  runnerSpaces.set([]);
  folders.set({});
  installedFolderSnapshot.set(null);
}

function reset() {
  installedRunner.set(null);
  ownedInstalledRunner.set(null);
  checkingInstalledScope.set(false);
  clearAttached();
  installedError.set(null);
  installedNotice.set(null);
}

onSessionEnd(() => {
  gen++;
  inflight = null;
  installedRunnerStatusWatchable.set(false);
  installedRunnerSeen.set(false);
  installedBusy.set(null);
  reset();
});

function message(error: unknown, what: string): string {
  return error instanceof BridgeError ? error.message : `${what} didn't work.`;
}

/** Which Spaces this user owns use this runner, by stable id (read-only GETs). */
async function loadAttached(runnerId: string, valid: () => boolean): Promise<void> {
  const c = apiClient();
  const root = folderRoot;
  const stillScoped = () => valid() && root === folderRoot &&
    installedRunner()?.runner?.id === runnerId && ownedInstalledRunner()?.id === runnerId;
  if (!c || !stillScoped()) return;
  const owned = spaces().filter((s) => s.role === "owner" && SPACE_ID_RE.test(s.id));
  const checks = await Promise.allSettled(owned.map(async ({ id }) => {
    const observation = beginSpaceRunnerObservation(id);
    return { id, observation, runner: await c.getSpaceRunner(id) };
  }));
  if (!stillScoped()) return;
  const stillOwned = new Set(spaces().filter((s) => s.role === "owner").map((s) => s.id));
  batch(() => {
    // Merge exact observations: an older answer for A doesn't discard
    // fresh answers for B. A failed read isn't a detach.
    for (const res of checks) {
      if (res.status !== "fulfilled" || !stillOwned.has(res.value.id)) continue;
      res.value.observation.publish(res.value.runner);
    }
    for (const { id } of runnerSpaces()) if (!stillOwned.has(id)) removeFolder(id);
    // Keep U5's complete-snapshot signal for the bounded refresh demand;
    // individual Space updates don't satisfy or restart that demand.
    if (checks.every((res) => res.status === "fulfilled")) installedFolderSnapshot.set(++folderSnapshotSerial);
  });
}

function removeFolder(sid: string) {
  if (runnerSpaces().some((s) => s.id === sid)) runnerSpaces.set(runnerSpaces().filter((s) => s.id !== sid));
  const all = folders();
  if (Object.prototype.hasOwnProperty.call(all, sid)) {
    const next = { ...all };
    delete next[sid];
    folders.set(next);
  }
}

// This runs as part of the same accepted observation that updates the paired
// flag and assignment cache, whether the read came from Settings or an attach.
onSpaceRunnerObserved((sid, runner) => {
  const id = ownedInstalledRunner()?.id;
  if (!id || installedRunner()?.runner?.id !== id || !SPACE_ID_RE.test(sid)) return;
  const space = spaces().find((s) => s.id === sid && s.role === "owner");
  if (!space || runner?.kind !== "registered" || runner.id !== id || runner.visibility !== "personal") {
    removeFolder(sid);
  } else if (!runnerSpaces().some((s) => s.id === sid)) {
    runnerSpaces.set([...runnerSpaces(), { id: sid, name: space.name }]);
  }
});

/** The owner deleted this registration. An older bridge read can't revive its controls. */
export function refreshAfterInstalledRunnerDeletion(id: string) {
  if (ownedInstalledRunner()?.id !== id) return;
  gen++;
  inflight = null;
  ownedInstalledRunner.set(null);
  clearAttached();
  void loadInstalledRunner();
}

type FolderRead = { runnerId: string | undefined; wanted: () => boolean };
type InstalledRead = { key: string; valid: () => boolean; folders: FolderRead[]; done: Promise<void> };
// One bridge read per session at a time. Folder demand can join a status-only read.
let inflight: InstalledRead | null = null;

/** Read status and optionally folders. A caller can withdraw its folder demand before the fetch. */
export function loadInstalledRunner(readFolders = true, wanted = () => true): Promise<void> {
  const key = sessionKey();
  if (installedBusy()) return Promise.resolve();
  const demand = readFolders ? { runnerId: installedRunner()?.runner?.id, wanted } : null;
  if (inflight?.key === key && inflight.valid()) {
    if (demand) inflight.folders.push(demand);
    return inflight.done;
  }
  const read: InstalledRead = { key, valid: stillValid(), folders: demand ? [demand] : [], done: Promise.resolve() };
  inflight = read;
  read.done = readStatus(read);
  return read.done;
}

async function readStatus(read: InstalledRead): Promise<void> {
  const { valid } = read;
  try {
    const { runner: next, watchable } = await readBridgeStatus();
    if (!valid()) return;
    if (next) installedRunnerSeen.set(true);
    installedRunnerStatusWatchable.set(watchable);
    if (!next || next.runner?.id !== installedRunner()?.runner?.id) reset();
    installedRunner.set(next);
    const id = next?.runner?.id;
    if (!id) return;
    const wantsFolders = () => valid() && read.folders.some((d) => (d.runnerId === undefined || d.runnerId === id) && d.wanted());
    if (ownedInstalledRunner()?.id !== id || wantsFolders()) await loadInstalledScope(id, valid);
    if (wantsFolders()) await loadAttached(id, wantsFolders);
  } finally {
    // Clear before resolving, so no caller can join after the last decision.
    if (inflight === read) inflight = null;
  }
}

/** Read the bridge again at the point of an explicit action, without changing the UI. */
export async function readInstalledRunner(): Promise<InstalledRunner | null> {
  return (await readBridgeStatus()).runner;
}

async function readBridgeStatus(): Promise<{ runner: InstalledRunner | null; watchable: boolean }> {
  const call = localRunnerCall();
  if (!call || !apiClient() || !me()) return { runner: null, watchable: false };
  try {
    const body = await call("GET", "/v1/local-tools/runner");
    const runner = parseInstalledRunner(body);
    // A definitive "not installed / not this workspace" doesn't need another
    // read. An empty or incomprehensible answer may be transient.
    const unavailable = isObj(body) && (body.requested === false || body.available === false);
    return { runner, watchable: !unavailable };
  } catch (error) {
    const unsupported = error instanceof BridgeError && [404, 405].includes(error.status ?? 0);
    return { runner: null, watchable: !unsupported };
  }
}

async function loadInstalledScope(runnerId: string, valid: () => boolean) {
  const c = apiClient();
  if (!c) return;
  checkingInstalledScope.set(true);
  try {
    const list = await c.listRunnerInventory();
    if (!valid()) return;
    if (list.runners.some((r) => r.id === runnerId && r.visibility === "personal" && r.canManage)) {
      ownedInstalledRunner.set({ id: runnerId, attachIfEmpty: list.attachIfEmpty });
    } else {
      ownedInstalledRunner.set(null);
      clearAttached();
    }
  } catch {
    if (valid()) {
      ownedInstalledRunner.set(null);
      clearAttached();
    }
  } finally {
    if (valid()) checkingInstalledScope.set(false);
  }
}

/** The person pressed Reconnect and confirmed. */
export async function reconnectInstalledRunner(): Promise<void> {
  const call = localRunnerCall();
  if (!call || installedBusy() || !installedRunner()?.canReconnect) return;
  const valid = stillValid();
  installedBusy.set("reconnect");
  installedError.set(null);
  installedNotice.set(null);
  try {
    const next = parseInstalledRunner(await call("POST", "/v1/local-tools/runner/reconnect"));
    if (!valid()) return;
    clearAttached();
    installedRunner.set(next);
    installedNotice.set("Reconnected. Choose Spaces without a runner below to attach it.");
    if (next?.runner) {
      await loadInstalledScope(next.runner.id, valid);
      await loadAttached(next.runner.id, valid);
    } else {
      ownedInstalledRunner.set(null);
    }
  } catch (error) {
    if (valid()) installedError.set(message(error, "Reconnecting this computer's runner"));
  } finally {
    if (valid()) installedBusy.set(null);
  }
}

/** The person pressed Show folder for a Space they own that uses this runner. */
export async function showRunnerFolder(sid: string): Promise<void> {
  const call = localRunnerCall();
  if (!call || installedBusy() || !SPACE_ID_RE.test(sid)) return;
  if (!runnerSpaces().some((s) => s.id === sid)) return;
  const valid = stillValid();
  installedBusy.set(`folder:${sid}`);
  installedError.set(null);
  installedNotice.set(null);
  try {
    const f = parseFolder(await call("POST", `/v1/local-tools/runner/spaces/${sid}/folder`), sid);
    if (!valid()) return;
    if (!f.attached) {
      installedError.set("This Space no longer uses this computer's runner.");
      return;
    }
    if (runnerSpaces().some((s) => s.id === sid)) folders.set({ ...folders(), [sid]: f });
  } catch (error) {
    if (valid()) installedError.set(message(error, "Opening the Space's folder"));
  } finally {
    if (valid()) installedBusy.set(null);
  }
}
