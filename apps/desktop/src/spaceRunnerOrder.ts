// Dispatch order shared by exact runner reads, assignment writes and the
// runner field of a Space list. Membership continues to use its own rules.
let serial = 0;
const latest = new Map<string, { stamp: number; pendingExact: boolean }>();

export const nextSpaceRunnerStamp = () => ++serial;
export const isSpaceRunnerStampCurrent = (sid: string, stamp: number) => (latest.get(sid)?.stamp ?? 0) < stamp;
export const spaceRunnerExactPending = (sid: string) => latest.get(sid)?.pendingExact === true;

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
  if (sid === undefined) latest.clear();
  else latest.delete(sid);
}
