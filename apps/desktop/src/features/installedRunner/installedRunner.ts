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
// - Metro never attaches it: the Space owner does that in Manage Space. The
//   folder is offered only for Spaces this user owns whose runner, by stable
//   id, is this one.
// - Separate from the per-Space dedicated "Local tools" switch.
// - Every answer is applied only if the session (server and account) and the
//   request generation are still the same.

import { signal } from "nsh";
import { BridgeError, scrub } from "../../device/bridge";
import { apiClient, connectedTo, me, sessionGuard, spaces } from "../../store";
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
/** Owned Spaces whose runner is this one, by stable id. */
export const runnerSpaces = signal<{ id: string; name: string }[]>([]);
export const installedBusy = signal<null | "reconnect" | `folder:${string}`>(null);
export const installedError = signal<string | null>(null);
export const installedNotice = signal<string | null>(null);
export const folders = signal<Record<string, RunnerFolder>>({});

let gen = 0;
const sessionKey = () => `${connectedTo() ?? ""}\n${me() ?? ""}`;

/** Still the same session and the latest request. */
function stillValid(): () => boolean {
  const mine = ++gen;
  const key = sessionKey();
  const ok = sessionGuard();
  return () => mine === gen && key === sessionKey() && ok();
}

function reset() {
  installedRunner.set(null);
  runnerSpaces.set([]);
  folders.set({});
  installedError.set(null);
  installedNotice.set(null);
}

function message(error: unknown, what: string): string {
  return error instanceof BridgeError ? error.message : `${what} didn't work.`;
}

/** Which Spaces this user owns use this runner, by stable id (read-only GETs). */
async function loadAttached(runnerId: string, valid: () => boolean): Promise<void> {
  const c = apiClient();
  if (!c) return;
  const owned = spaces().filter((s) => s.role === "owner" && SPACE_ID_RE.test(s.id));
  const checks = await Promise.allSettled(owned.map(async (s) => ({ s, r: await c.getSpaceRunner(s.id) })));
  if (!valid() || installedRunner()?.runner?.id !== runnerId) return;
  const list: { id: string; name: string }[] = [];
  for (const res of checks) {
    if (res.status === "fulfilled" && res.value.r?.kind === "registered" && res.value.r.id === runnerId) list.push({ id: res.value.s.id, name: res.value.s.name });
  }
  runnerSpaces.set(list);
}

// One read per session at a time: the page may mount more than once as it opens.
let inflight: { key: string; done: Promise<void> } | null = null;

/** Opening the page: read the status only. Never throws; unknown hides. */
export function loadInstalledRunner(): Promise<void> {
  const key = sessionKey();
  if (inflight && inflight.key === key) return inflight.done;
  const done = readStatus().finally(() => {
    if (inflight?.done === done) inflight = null;
  });
  inflight = { key, done };
  return done;
}

async function readStatus(): Promise<void> {
  const call = localRunnerCall();
  const valid = stillValid();
  reset();
  if (!call || !apiClient()) return;
  let next: InstalledRunner | null;
  try {
    next = parseInstalledRunner(await call("GET", "/v1/local-tools/runner"));
  } catch {
    next = null;
  }
  if (!valid()) return;
  installedRunner.set(next);
  if (next?.runner) await loadAttached(next.runner.id, valid);
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
    installedRunner.set(next);
    folders.set({});
    installedNotice.set("Reconnected. Attach it to a Space in that Space's Manage view if it isn't attached already.");
    if (next?.runner) await loadAttached(next.runner.id, valid);
    else runnerSpaces.set([]);
  } catch (error) {
    if (valid()) installedError.set(message(error, "Reconnecting this computer's runner"));
  } finally {
    installedBusy.set(null);
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
    folders.set({ ...folders(), [sid]: f });
  } catch (error) {
    if (valid()) installedError.set(message(error, "Opening the Space's folder"));
  } finally {
    installedBusy.set(null);
  }
}
