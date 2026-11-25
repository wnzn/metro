import { signal, untracked } from "nsh";

// Runner-only cache order consumes request stamps from the shared access
// sequence. Membership evidence and pending reads live in store.ts.
type RunnerOrder = { stamp: number; pendingExact: boolean };
const latest = new Map<string, ReturnType<typeof signal<RunnerOrder | undefined>>>();

function runnerOrder(sid: string) {
  let order = latest.get(sid);
  if (!order) latest.set(sid, order = signal<RunnerOrder | undefined>(undefined));
  return order;
}
const readRunnerOrder = (sid: string) => untracked(() => latest.get(sid)?.());
export const isSpaceRunnerStampCurrent = (sid: string, stamp: number) => (readRunnerOrder(sid)?.stamp ?? 0) < stamp;
export const spaceRunnerExactPending = (sid: string) => readRunnerOrder(sid)?.pendingExact === true;

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
  } else {
    latest.get(sid)?.set(undefined);
    latest.delete(sid);
  }
}
