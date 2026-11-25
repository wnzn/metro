// Dispatch order shared by exact runner reads, assignment writes and the
// runner field of a Space list. Membership continues to use its own rules.
let serial = 0;
const latest = new Map<string, { stamp: number; pendingExact: boolean }>();
// Only confirmed creation or an exact successful Space read can protect a
// cached row against an older list omission. A write intent or hint cannot.
const accessProofs = new Map<string, number>();

export const nextSpaceRunnerStamp = () => ++serial;
export const isSpaceRunnerStampCurrent = (sid: string, stamp: number) => (latest.get(sid)?.stamp ?? 0) < stamp;
export const spaceRunnerExactPending = (sid: string) => latest.get(sid)?.pendingExact === true;
export const hasNewerSpaceAccessProof = (sid: string, stamp: number) => (accessProofs.get(sid) ?? 0) > stamp;

export function recordSpaceAccessProof(sid: string, stamp: number) {
  if (hasNewerSpaceAccessProof(sid, stamp)) return;
  accessProofs.set(sid, stamp);
}

export function discardSpaceAccessProof(sid: string, stamp: number) {
  if (!hasNewerSpaceAccessProof(sid, stamp)) accessProofs.delete(sid);
}

export const hasNewerSpaceRunnerStamp = (sid: string, stamp: number) => (latest.get(sid)?.stamp ?? 0) > stamp;

export function recordSpaceRunnerStamp(sid: string, stamp: number, pendingExact = false) {
  latest.set(sid, { stamp, pendingExact });
}

/** A list can update a hint, but cannot establish exact post-write state. */
export function acceptSpaceRunnerSummary(sid: string, stamp: number) {
  if (spaceRunnerExactPending(sid) || !isSpaceRunnerStampCurrent(sid, stamp)) return false;
  recordSpaceRunnerStamp(sid, stamp);
  return true;
}

export function clearSpaceRunnerOrder(sid?: string) {
  if (sid === undefined) {
    latest.clear();
    accessProofs.clear();
  } else {
    latest.delete(sid);
    accessProofs.delete(sid);
  }
}
