import { batch, signal } from "nsh";
import { ApiError } from "./api/client";
import type { SpaceRunner } from "./api/client";
import { beginRunnerAccessRead, nextAccessRequest, onSessionEnd, onSpacePurged, setRunnerPaired } from "./store";
import { clearSpaceRunnerOrder, hasNewerSpaceRunnerStamp, isSpaceRunnerStampCurrent, recordSpaceRunnerStamp, spaceRunnerExactPending } from "./spaceRunnerOrder";

/** Missing means not observed; null is an observed empty assignment. */
export const spaceRunners = signal<Record<string, SpaceRunner | null>>({});

const listeners: ((sid: string, runner: SpaceRunner | null, allowFolderAdd: boolean) => void)[] = [];

export function onSpaceRunnerObserved(listener: (sid: string, runner: SpaceRunner | null, allowFolderAdd: boolean) => void) {
  listeners.push(listener);
}

/** A local mutation invalidates older observations of just its own Space. */
export function invalidateSpaceRunnerObservations(sid: string) {
  recordSpaceRunnerStamp(sid, nextAccessRequest(), spaceRunnerExactPending(sid));
}

export function isSpaceRunnerAccessDenied(error: unknown) {
  return error instanceof ApiError && (error.kind === "forbidden" || error.kind === "not_found");
}

/** A guarded ack disproves the pre-write assignment, not the current one. */
export function forgetDisprovedSpaceRunner(sid: string, previous: SpaceRunner | null, atDispatch: SpaceRunner | null | undefined) {
  const all = spaceRunners();
  const current = all[sid];
  if (current !== undefined && current !== previous && !(previous === null && current === atDispatch) &&
      !(current && previous?.assignmentRevision &&
      current.kind === previous.kind && current.assignmentRevision === previous.assignmentRevision)) return;
  hideSpaceRunner(sid, nextAccessRequest());
}

function forgetSpaceRunner(sid: string) {
  const all = spaceRunners();
  if (Object.prototype.hasOwnProperty.call(all, sid)) {
    const next = { ...all };
    delete next[sid];
    spaceRunners.set(next);
  }
  for (const listener of listeners) listener(sid, null, false);
}

onSpacePurged(forgetSpaceRunner);

function hideSpaceRunner(sid: string, stamp: number) {
  batch(() => {
    recordSpaceRunnerStamp(sid, stamp, true);
    setRunnerPaired(sid, null);
    // Hide a stale folder, but don't claim the Space is now empty or attachable.
    forgetSpaceRunner(sid);
  });
}

/** Hosted assignment reads only. A newer accepted answer wins, not one merely in flight. */
export function beginSpaceRunnerObservation(sid: string) {
  const access = beginRunnerAccessRead(sid);
  const stamp = access.request;
  const isCurrent = () => access.valid() && isSpaceRunnerStampCurrent(sid, stamp);
  const finish = (verified = false) => access.finish(verified && isCurrent());
  return {
    isCurrent,
    finish,
    hasNewer: () => !access.valid() || hasNewerSpaceRunnerStamp(sid, stamp),
    denyAccess() {
      if (!isCurrent()) {
        access.finish();
        return false;
      }
      access.withdraw();
      hideSpaceRunner(sid, stamp);
      return true;
    },
    publish(runner: SpaceRunner | null, allowFolderAdd = true, exact = true) {
      finish(exact);
      if (!isCurrent()) return false;
      recordSpaceRunnerStamp(sid, stamp);
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
