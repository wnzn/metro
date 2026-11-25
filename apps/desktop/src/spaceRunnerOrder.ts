import { signal, untracked } from "nsh";

// Dispatch order shared by exact runner reads, assignment writes and the
// runner field of a Space list. Membership continues to use its own rules.
let serial = 0;
type RunnerOrder = { stamp: number; pendingExact: boolean };
const latest = new Map<string, ReturnType<typeof signal<RunnerOrder | undefined>>>();

function runnerOrder(sid: string) {
  let order = latest.get(sid);
  if (!order) latest.set(sid, order = signal<RunnerOrder | undefined>(undefined));
  return order;
}
const readRunnerOrder = (sid: string) => untracked(() => latest.get(sid)?.());
// Only confirmed creation or an exact successful Space read can protect a
// cached row against an older list omission. A write intent or hint cannot.
const accessProofs = new Map<string, number>();

export const nextSpaceRunnerStamp = () => ++serial;
export const isSpaceRunnerStampCurrent = (sid: string, stamp: number) => (readRunnerOrder(sid)?.stamp ?? 0) < stamp;
export const spaceRunnerExactPending = (sid: string) => readRunnerOrder(sid)?.pendingExact === true;
export const hasNewerSpaceAccessProof = (sid: string, stamp: number) => (accessProofs.get(sid) ?? 0) > stamp;

export function recordSpaceAccessProof(sid: string, stamp: number) {
  if (hasNewerSpaceAccessProof(sid, stamp)) return;
  accessProofs.set(sid, stamp);
}

export function discardSpaceAccessProof(sid: string, stamp: number) {
  if (!hasNewerSpaceAccessProof(sid, stamp)) accessProofs.delete(sid);
}

/** Impact confirmations subscribe to the exact Spaces their proof covers. */
export const hasNewerSpaceRunnerStamp = (sid: string, stamp: number) => (runnerOrder(sid)()?.stamp ?? 0) > stamp;

export function recordSpaceRunnerStamp(sid: string, stamp: number, pendingExact = false) {
  runnerOrder(sid).set({ stamp, pendingExact });
}

/** A list can update a hint, but cannot establish exact post-write state. */
export function acceptSpaceRunnerSummary(sid: string, stamp: number) {
  if (spaceRunnerExactPending(sid) || !isSpaceRunnerStampCurrent(sid, stamp)) return false;
  recordSpaceRunnerStamp(sid, stamp);
  return true;
}

export function clearSpaceRunnerOrder(sid?: string) {
  if (sid === undefined) {
    for (const order of latest.values()) order.set(undefined);
    latest.clear();
    accessProofs.clear();
  } else {
    latest.get(sid)?.set(undefined);
    latest.delete(sid);
    accessProofs.delete(sid);
  }
}
