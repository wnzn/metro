// Settings › Runners: one registered runner's shell policy (its default and
// per-Space overrides), read and changed only by whoever manages it: a
// personal runner's owner, or org admins for an organization runner.
//
// - Reads happen when the person opens a runner's Shell commands section;
//   saves only on Save. The server answers with the whole policy, and that
//   answer replaces the shown one.
// - An override exists only while the runner is attached to that Space: the
//   server refuses it otherwise (404) and the list is read again.
// - Every answer is applied only if the session, account and request are
//   still current.

import { batch, signal } from "nsh";
import { ApiError } from "../../api/client";
import type { RunnerShellPolicy, ShellApprover, ShellMode, ShellPolicy } from "../../api/shell";
import { apiClient, disconnect, isAborted, onSessionEnd, readHostedAccess, sessionGuard } from "../../store";
import { runnerSessionKey } from "../../runnersState";

type Entry = { policy: RunnerShellPolicy | null; loading: boolean; error: string | null };

const entries = signal<Record<string, Entry>>({});
const notices = signal<Record<string, { ok: boolean; message: string }>>({});
/** `default:<runner id>` or `space:<runner id>:<space id>` while it saves. */
export const runnerShellBusy = signal<string | null>(null);
const seq = new Map<string, number>();

onSessionEnd(() => {
  seq.clear();
  batch(() => {
    entries.set({});
    notices.set({});
    runnerShellBusy.set(null);
  });
});

export const runnerShell = (runnerId: string) => entries()[runnerId] ?? null;
export const runnerShellNotice = (runnerId: string) => notices()[runnerId] ?? null;

function put(runnerId: string, entry: Entry) {
  entries.set({ ...entries(), [runnerId]: entry });
}

function notice(runnerId: string, value: { ok: boolean; message: string } | null) {
  const next = { ...notices() };
  if (value) next[runnerId] = value;
  else delete next[runnerId];
  notices.set(next);
}

/** Still the same session, account and the latest request for this runner. */
function guard(runnerId: string) {
  const mine = (seq.get(runnerId) ?? 0) + 1;
  seq.set(runnerId, mine);
  const key = runnerSessionKey();
  const ok = sessionGuard();
  return () => ok() && key === runnerSessionKey() && seq.get(runnerId) === mine;
}

function readError(error: unknown): string {
  if (error instanceof ApiError && error.status === 403) return "Only whoever manages this runner can see its shell policy.";
  if (error instanceof ApiError && error.status === 404) return "This runner is no longer available, or this server doesn't support shell policies.";
  return "Couldn't load the shell policy.";
}

function saveError(error: unknown, organization: boolean): string {
  const status = error instanceof ApiError ? error.status : null;
  if (status === 400) return organization ? "Organization runners can't use “Ask when System One flags it”. Nothing was saved." : "The server refused this policy. Nothing was saved.";
  if (status === 403) return "Only whoever manages this runner can change this.";
  if (status === 404) return "This runner isn't attached to that Space any more, or is no longer available. Nothing was saved.";
  return "The server did not confirm the change. Check the policy before trying again.";
}

export async function loadRunnerShell(runnerId: string): Promise<void> {
  const c = apiClient();
  if (!c) return;
  const valid = guard(runnerId);
  const before = runnerShell(runnerId);
  put(runnerId, { policy: before?.policy ?? null, loading: true, error: null });
  try {
    const policy = await readHostedAccess(c, () => c.getRunnerShellPolicy(runnerId));
    if (valid()) put(runnerId, { policy, loading: false, error: null });
  } catch (error) {
    if (!valid() || isAborted(error)) return;
    put(runnerId, { policy: null, loading: false, error: readError(error) });
  }
}

async function save(runnerId: string, busy: string, send: () => Promise<RunnerShellPolicy>, done: string) {
  const c = apiClient();
  const current = runnerShell(runnerId)?.policy;
  if (!c || !current || runnerShellBusy()) return;
  const valid = guard(runnerId);
  runnerShellBusy.set(busy);
  notice(runnerId, null);
  try {
    const policy = await send();
    if (!valid()) return;
    batch(() => {
      put(runnerId, { policy, loading: false, error: null });
      notice(runnerId, { ok: true, message: done });
    });
  } catch (error) {
    if (!valid() || isAborted(error)) return;
    if (error instanceof ApiError && error.kind === "auth") {
      disconnect(error.message);
      return;
    }
    notice(runnerId, { ok: false, message: saveError(error, current.visibility === "organization") });
    // The attachments or management may have changed: show what's true now.
    if (error instanceof ApiError && (error.status === 404 || error.status === 409)) await loadRunnerShell(runnerId);
  } finally {
    if (runnerShellBusy() === busy) runnerShellBusy.set(null);
  }
}

export function saveRunnerShellDefault(runnerId: string, policy: ShellPolicy) {
  const c = apiClient();
  if (!c) return Promise.resolve();
  return save(runnerId, `default:${runnerId}`, () => c.setRunnerShellPolicy(runnerId, policy), "Default saved.");
}

/** Both null: inherit the runner default. */
export function saveRunnerShellSpace(runnerId: string, spaceId: string, policy: { mode: ShellMode | null; approver: ShellApprover | null }) {
  const c = apiClient();
  if (!c || !runnerShell(runnerId)?.policy?.spaces.some((s) => s.spaceId === spaceId)) return Promise.resolve();
  return save(runnerId, `space:${runnerId}:${spaceId}`, () => c.setRunnerSpaceShellPolicy(runnerId, spaceId, policy),
    policy.mode === null && policy.approver === null ? "This Space now inherits the default." : "Space override saved.");
}
