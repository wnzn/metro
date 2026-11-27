// This computer's personal registered runner, built into the desktop app
// (crates/connector desktop_tools/runner.rs), for Settings › Runners.
//
// - Asked only of the packaged launcher's bridge (`/v1/local-tools/runner…`,
//   see ../localtools/localTools.ts `localRunnerCall`), never of a Metro
//   server. The bridge answers `available: true` for whichever server this
//   session is signed in to; anything else (no launcher, older launcher, no
//   session, an unreadable answer) hides the section.
// - Opening the page only reads. Set up (creates the runner), Reconnect
//   (`{}`: rotates the recorded runner id's token if it's still this
//   person's, else creates a new runner; never by name) and a Space's folder
//   (which creates it on this computer) are POSTs, sent only from an
//   explicit press. Deleting it in Settings also tells the bridge to forget
//   it (`{runner_id}`), so it's off here until set up again.
// - Metro never attaches it without an explicit owner action. The
//   folder is offered only for Spaces this user owns whose runner, by stable
//   id, is this one.
// - Separate from the per-Space dedicated "Local tools" switch.
// - Every answer is applied only if the session (server and account) and the
//   request generation are still the same.

import { batch, signal } from "nsh";
import { BridgeError, scrub } from "../../device/bridge";
import { beginSpaceRunnerObservation, isSpaceRunnerAccessDenied, onSpaceRunnerObserved } from "../../spaceRunnerObservations";
import { apiClient, connectedTo, me, onSessionEnd, readHostedAccess, sessionGuard, spaces } from "../../store";
import { localRunnerCall } from "../localtools/localTools";

export type InstalledRunnerState =
  | "not_set_up"
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
  canSetUp: boolean;
  canReconnect: boolean;
};

export type RunnerFolder = {
  spaceId: string;
  /** What the next job uses. */
  folder: string;
  /** Absent on a Clear answer (DELETE), which doesn't check the server. */
  attached: boolean | null;
  defaultFolder: string | null;
  /** A folder the person chose, not the default one. */
  custom: boolean;
  /** One line: the chosen folder can't be used now, and jobs fail until it's changed or cleared. */
  problem: string | null;
};

const STATES: readonly string[] = ["not_set_up", "idle", "starting", "online", "offline", "revoked", "deleted", "interrupted", "failed", "stopped"];
const SPACE_ID_RE = /^[a-z0-9][a-z0-9_-]{0,63}$/;
const isObj = (v: unknown): v is Record<string, unknown> => typeof v === "object" && v !== null && !Array.isArray(v);

/** The bridge's runner status, or null when there's nothing to show here. */
export function parseInstalledRunner(v: unknown): InstalledRunner | null {
  if (!isObj(v) || v.available !== true) return null;
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
    canSetUp: v.can_set_up === true,
    canReconnect: v.can_reconnect === true,
  };
}

export function parseFolder(v: unknown, sid: string, requireAttached = true): RunnerFolder {
  if (!isObj(v) || v.space_id !== sid || typeof v.folder !== "string" ||
      (requireAttached ? typeof v.attached !== "boolean" : v.attached !== undefined && typeof v.attached !== "boolean")) {
    throw new BridgeError("protocol", "Metro's device helper sent folder data this app doesn't understand.");
  }
  return {
    spaceId: sid,
    folder: v.folder,
    attached: typeof v.attached === "boolean" ? v.attached : null,
    defaultFolder: typeof v.default_folder === "string" ? v.default_folder : null,
    custom: v.custom === true,
    problem: typeof v.folder_problem === "string" && v.folder_problem.trim() ? scrub(v.folder_problem) : null,
  };
}

/** GET …/runner/folders: every chosen folder, attached or not. */
export function parseChosenFolders(v: unknown): Record<string, string> {
  if (!isObj(v) || !Array.isArray(v.folders)) {
    throw new BridgeError("protocol", "Metro's device helper sent folder data this app doesn't understand.");
  }
  const out: Record<string, string> = {};
  for (const f of v.folders) {
    if (isObj(f) && typeof f.space_id === "string" && SPACE_ID_RE.test(f.space_id) && typeof f.folder === "string") out[f.space_id] = f.folder;
  }
  return out;
}

/** The status, or null (hidden: no launcher, no session, unknown). */
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
export const installedBusy = signal<null | "setup" | "reconnect" | `folder:${string}` | `folder-set:${string}` | `folder-clear:${string}`>(null);
export const installedError = signal<string | null>(null);
export const installedNotice = signal<string | null>(null);
export const folders = signal<Record<string, RunnerFolder>>({});
/** Every folder this person chose here (space id → path), attached or not; null until read. */
export const chosenFolders = signal<Record<string, string> | null>(null);
/**
 * Owned Spaces a complete, current attachment scan read successfully and
 * found not served by this runner, for the snapshot `serial`. Null without
 * such a scan. Never inferred from a missing, denied or failed read.
 */
export const verifiedDetached = signal<{ serial: number; runnerId: string; ids: ReadonlySet<string> } | null>(null);
/** Per Space: why the last Change or Use default didn't work, said beside it. */
export const folderErrors = signal<Record<string, string>>({});

let gen = 0;
/** The folder action that owns `installedBusy`; only it releases it. */
let busyOp: object | null = null;
let folderRoot = 0;
let chosenRead = 0;
let folderSnapshotSerial = 0;
const sessionKey = () => `${connectedTo() ?? ""}\n${me() ?? ""}`;

/** Still the same session and the latest request. */
function stillValid(): () => boolean {
  const mine = ++gen;
  const key = sessionKey();
  const ok = sessionGuard();
  return () => mine === gen && key === sessionKey() && ok();
}

/**
 * Takes `installedBusy` for one folder action. The release clears it only if
 * no newer action (or a session change) took it since, even when the action's
 * own result was dropped as stale.
 */
function beginFolderBusy(value: `folder:${string}` | `folder-set:${string}` | `folder-clear:${string}`): () => void {
  const op = {};
  busyOp = op;
  installedBusy.set(value);
  return () => {
    if (busyOp !== op) return;
    busyOp = null;
    installedBusy.set(null);
  };
}

function clearAttached() {
  folderRoot++;
  chosenRead++;
  runnerSpaces.set([]);
  folders.set({});
  chosenFolders.set(null);
  folderErrors.set({});
  verifiedDetached.set(null);
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
  busyOp = null;
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
async function loadAttached(runnerId: string, scanWanted: () => boolean): Promise<void> {
  const c = apiClient();
  const root = folderRoot;
  const sameSession = sessionGuard();
  const sameRunner = () => sameSession() && root === folderRoot &&
    installedRunner()?.runner?.id === runnerId && ownedInstalledRunner()?.id === runnerId;
  const stillScanning = () => sameRunner() && scanWanted();
  if (!c || !stillScanning()) return;
  const owned = spaces().filter((s) => s.role === "owner" && SPACE_ID_RE.test(s.id));
  const checks = await Promise.all(owned.map(async ({ id }) => {
    const observation = beginSpaceRunnerObservation(id);
    try {
      const runner = await readHostedAccess(c, () => c.getSpaceRunner(id));
      observation.finish(sameRunner());
      return { id, observation, ok: true as const, runner };
    } catch (error) {
      const denied = isSpaceRunnerAccessDenied(error);
      // Withdraw this row as soon as its refusal settles. A sibling, page
      // close, or the scan budget must not keep current denied evidence alive.
      if (denied && sameRunner()) observation.denyAccess();
      return { ok: false as const, denied };
    } finally {
      observation.finish();
    }
  }));
  if (!stillScanning()) return;
  const stillOwned = new Set(spaces().filter((s) => s.role === "owner").map((s) => s.id));
  batch(() => {
    // Merge successful exact observations independently. Individual
    // denials already settled; transient failures leave existing rows alone.
    // Which reads were accepted as the Space's current assignment: publish
    // refuses one a newer read or write already superseded.
    const accepted = new Set<string>();
    for (const check of checks) {
      if (check.ok && stillOwned.has(check.id) && check.observation.publish(check.runner)) accepted.add(check.id);
    }
    for (const { id } of runnerSpaces()) if (!stillOwned.has(id)) removeFolder(id);
    // Keep U5's complete-snapshot signal for the bounded refresh demand;
    // individual Space updates don't satisfy or restart that demand.
    if (checks.every((check) => check.ok || check.denied)) {
      const serial = ++folderSnapshotSerial;
      installedFolderSnapshot.set(serial);
      // Proof of "not this runner" needs every owned Space read successfully
      // and accepted as current: a denial, failure or superseded read proves
      // nothing. (`isCurrent()` turns false once its own read is published.)
      const complete = checks.every((check) => check.ok && (accepted.has(check.id) || !stillOwned.has(check.id)));
      const detached = new Set<string>();
      if (complete) {
        for (const check of checks) {
          if (!check.ok || !accepted.has(check.id)) continue;
          const r = check.runner;
          if (!(r?.kind === "registered" && r.id === runnerId)) detached.add(check.id);
        }
      }
      verifiedDetached.set(complete ? { serial, runnerId, ids: detached } : null);
    }
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
onSpaceRunnerObserved((sid, runner, allowFolderAdd) => {
  const id = ownedInstalledRunner()?.id;
  if (!id || installedRunner()?.runner?.id !== id || !SPACE_ID_RE.test(sid)) return;
  const space = spaces().find((s) => s.id === sid && s.role === "owner");
  // The list's cached role is not proof. Only this exact reply can supply
  // an owner-only revision; a member or an old server can still report status.
  if (!space || runner?.kind !== "registered" || runner.id !== id || runner.visibility !== "personal" ||
      !runner.assignmentRevision) {
    removeFolder(sid);
  } else if (allowFolderAdd && !runnerSpaces().some((s) => s.id === sid)) {
    runnerSpaces.set([...runnerSpaces(), { id: sid, name: space.name }]);
  }
  // Any later sign that it serves this Space withdraws the "detached" proof.
  const proof = verifiedDetached();
  if (proof?.ids.has(sid) && runner?.kind === "registered" && runner.id === id) {
    const ids = new Set(proof.ids);
    ids.delete(sid);
    verifiedDetached.set({ ...proof, ids });
  }
});

/** The owner deleted this registration: turn it off here too. An older bridge read can't revive its controls. */
export function refreshAfterInstalledRunnerDeletion(id: string) {
  if (ownedInstalledRunner()?.id !== id) return;
  gen++;
  inflight = null;
  ownedInstalledRunner.set(null);
  clearAttached();
  const call = localRunnerCall();
  const valid = stillValid();
  void (async () => {
    try {
      if (call) await call("POST", "/v1/local-tools/runner/forget", { runner_id: id });
    } catch {
      // The bridge keeps its state (revoked or deleted) and offers Reconnect.
    }
    if (valid()) void loadInstalledRunner();
  })();
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
    // A definitive "no session here" doesn't need another read. An empty
    // or incomprehensible answer may be transient.
    const unavailable = isObj(body) && body.available === false;
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
    const list = await readHostedAccess(c, () => c.listRunnerInventory());
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

/** The person pressed Set up. */
export async function setupInstalledRunner(): Promise<void> {
  const call = localRunnerCall();
  if (!call || installedBusy() || !installedRunner()?.canSetUp) return;
  const valid = stillValid();
  busyOp = null; // a folder action's late release must not clear this
  installedBusy.set("setup");
  installedError.set(null);
  installedNotice.set(null);
  try {
    const next = parseInstalledRunner(await call("POST", "/v1/local-tools/runner/setup"));
    if (!valid()) return;
    clearAttached();
    installedRunner.set(next);
    installedRunnerStatusWatchable.set(true);
    installedNotice.set("Set up. Choose Spaces without a runner below to attach it.");
    if (next?.runner) {
      await loadInstalledScope(next.runner.id, valid);
      await loadAttached(next.runner.id, valid);
    }
  } catch (error) {
    if (valid()) installedError.set(message(error, "Setting up this computer's runner"));
  } finally {
    if (valid()) installedBusy.set(null);
  }
}

/** The person pressed Reconnect and confirmed. */
export async function reconnectInstalledRunner(): Promise<void> {
  const call = localRunnerCall();
  if (!call || installedBusy() || !installedRunner()?.canReconnect) return;
  const valid = stillValid();
  busyOp = null;
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
  const row = runnerSpaces().find((s) => s.id === sid);
  if (!row) return;
  const valid = stillValid();
  const stillRow = () => valid() && runnerSpaces().includes(row);
  const release = beginFolderBusy(`folder:${sid}`);
  installedError.set(null);
  installedNotice.set(null);
  try {
    const f = parseFolder(await call("POST", `/v1/local-tools/runner/spaces/${sid}/folder`), sid);
    if (!stillRow()) return;
    if (!f.attached) {
      installedError.set("This Space no longer uses this computer's runner.");
      return;
    }
    folders.set({ ...folders(), [sid]: f });
  } catch (error) {
    if (stillRow()) {
      // The local refusal belongs to this row and session, not to a hosted
      // assignment stamp. A newer list alone doesn't replace the folder row.
      if (error instanceof BridgeError && ((error.status === 403 && error.code === "not_owner") ||
          (error.status === 404 && error.code === "not_found"))) batch(() => removeFolder(sid));
      installedError.set(message(error, "Opening the Space's folder"));
    }
  } finally {
    release();
  }
}

function setFolderError(sid: string, message: string | null) {
  const next = { ...folderErrors() };
  if (message) next[sid] = message;
  else delete next[sid];
  folderErrors.set(next);
}

/** Reads which folders this person chose (a GET, no side effects). */
export async function loadChosenFolders(): Promise<void> {
  const call = localRunnerCall();
  if (!call || !installedRunner()?.runner) return;
  const mine = ++chosenRead;
  const key = sessionKey();
  const ok = sessionGuard();
  try {
    const chosen = parseChosenFolders(await call("GET", "/v1/local-tools/runner/folders"));
    if (ok() && key === sessionKey() && mine === chosenRead) chosenFolders.set(chosen);
  } catch {
    // Unknown: offer no Use default for folders it can't show.
  }
}

/**
 * The person typed a folder for an attached Space and pressed Save. The
 * bridge checks it (and again before every job); nothing is moved.
 */
export async function setRunnerFolder(sid: string, path: string): Promise<boolean> {
  const call = localRunnerCall();
  if (!call || installedBusy() || !SPACE_ID_RE.test(sid)) return false;
  const row = runnerSpaces().find((s) => s.id === sid);
  if (!row) return false;
  const trimmed = path.trim();
  if (!trimmed) {
    setFolderError(sid, "Enter the full path of an existing folder.");
    return false;
  }
  const valid = stillValid();
  const stillRow = () => valid() && runnerSpaces().includes(row);
  const release = beginFolderBusy(`folder-set:${sid}`);
  setFolderError(sid, null);
  installedError.set(null);
  installedNotice.set(null);
  try {
    const f = parseFolder(await call("PUT", `/v1/local-tools/runner/spaces/${sid}/folder`, { path: trimmed }), sid);
    if (!stillRow()) return false;
    batch(() => {
      folders.set({ ...folders(), [sid]: f });
      if (chosenFolders()) chosenFolders.set({ ...chosenFolders(), [sid]: f.folder });
    });
    return true;
  } catch (error) {
    if (stillRow()) {
      // Not a member, or this runner no longer serves the Space: the row is
      // stale. Drop it and say why above the list, where it stays visible.
      if (error instanceof BridgeError && ((error.status === 404 && error.code === "not_found") ||
          (error.status === 409 && error.code === "not_attached"))) {
        batch(() => {
          removeFolder(sid);
          setFolderError(sid, null);
          installedError.set(error.code === "not_attached"
            ? `${row.name} no longer uses this computer's runner. Nothing was changed.`
            : message(error, "Changing the folder"));
        });
      } else setFolderError(sid, message(error, "Changing the folder"));
    }
    return false;
  } finally {
    release();
  }
}

/** Use default: back to the Space's own folder. Works for Spaces no longer attached too. */
export async function clearRunnerFolder(sid: string): Promise<void> {
  const call = localRunnerCall();
  if (!call || installedBusy() || !SPACE_ID_RE.test(sid)) return;
  const valid = stillValid();
  const release = beginFolderBusy(`folder-clear:${sid}`);
  setFolderError(sid, null);
  installedError.set(null);
  installedNotice.set(null);
  try {
    const f = parseFolder(await call("DELETE", `/v1/local-tools/runner/spaces/${sid}/folder`), sid, false);
    if (!valid()) return;
    batch(() => {
      const before = folders()[sid];
      if (before || runnerSpaces().some((s) => s.id === sid)) folders.set({ ...folders(), [sid]: { ...f, attached: before?.attached ?? null } });
      const chosen = chosenFolders();
      if (chosen && Object.prototype.hasOwnProperty.call(chosen, sid)) {
        const next = { ...chosen };
        delete next[sid];
        chosenFolders.set(next);
      }
    });
  } catch (error) {
    if (valid()) setFolderError(sid, message(error, "Using the default folder"));
  } finally {
    release();
  }
}
