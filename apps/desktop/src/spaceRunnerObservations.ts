import { batch, signal } from "nsh";
import type { SpaceRunner } from "./api/client";
import { onSessionEnd, sessionGuard, setRunnerPaired } from "./store";
import { clearSpaceRunnerOrder, hasNewerSpaceRunnerStamp, isSpaceRunnerStampCurrent, nextSpaceRunnerStamp, recordSpaceAccessProof, recordSpaceRunnerStamp, spaceRunnerExactPending } from "./spaceRunnerOrder";

/** Missing means not observed; null is an observed empty assignment. */
export const spaceRunners = signal<Record<string, SpaceRunner | null>>({});

const listeners: ((sid: string, runner: SpaceRunner | null, allowFolderAdd: boolean) => void)[] = [];

export function onSpaceRunnerObserved(listener: (sid: string, runner: SpaceRunner | null, allowFolderAdd: boolean) => void) {
  listeners.push(listener);
}

/** A local mutation invalidates older observations of just its own Space. */
export function invalidateSpaceRunnerObservations(sid: string) {
  recordSpaceRunnerStamp(sid, nextSpaceRunnerStamp(), spaceRunnerExactPending(sid));
}

/** A guarded ack disproves the pre-write assignment, not the current one. */
export function forgetDisprovedSpaceRunner(sid: string, previous: SpaceRunner | null, atDispatch: SpaceRunner | null | undefined) {
  const all = spaceRunners();
  const current = all[sid];
  if (current !== undefined && current !== previous && !(previous === null && current === atDispatch) &&
      !(current && previous?.assignmentRevision &&
      current.kind === previous.kind && current.assignmentRevision === previous.assignmentRevision)) return;
  const next = { ...all };
  delete next[sid];
  batch(() => {
    recordSpaceRunnerStamp(sid, nextSpaceRunnerStamp(), true);
    spaceRunners.set(next);
    setRunnerPaired(sid, null);
    // Hide a stale folder, but don't claim the Space is now empty or attachable.
    for (const listener of listeners) listener(sid, null, false);
  });
}

/** Take a ticket at dispatch. A newer accepted answer wins, not one merely in flight. */
export function beginSpaceRunnerObservation(sid: string) {
  const stamp = nextSpaceRunnerStamp();
  const inScope = sessionGuard(sid);
  const isCurrent = () => inScope() && isSpaceRunnerStampCurrent(sid, stamp);
  return {
    isCurrent,
    hasNewer: () => !inScope() || hasNewerSpaceRunnerStamp(sid, stamp),
    publish(runner: SpaceRunner | null, allowFolderAdd = true, exact = true) {
      if (!isCurrent()) return false;
      recordSpaceRunnerStamp(sid, stamp);
      if (exact) recordSpaceAccessProof(sid, stamp);
      batch(() => {
        spaceRunners.set({ ...spaceRunners(), [sid]: runner });
        setRunnerPaired(sid, runner !== null);
        for (const listener of listeners) listener(sid, runner, allowFolderAdd);
      });
      return true;
    },
  };
}

onSessionEnd(() => {
  clearSpaceRunnerOrder();
  spaceRunners.set({});
});
