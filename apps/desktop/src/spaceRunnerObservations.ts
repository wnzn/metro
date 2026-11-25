import { batch, signal } from "nsh";
import type { SpaceRunner } from "./api/client";
import { onSessionEnd, sessionGuard, setRunnerPaired } from "./store";

/** Missing means not observed; null is an observed empty assignment. */
export const spaceRunners = signal<Record<string, SpaceRunner | null>>({});

let serial = 0;
const latest = new Map<string, number>();
const listeners: ((sid: string, runner: SpaceRunner | null, allowFolderAdd: boolean) => void)[] = [];

export function onSpaceRunnerObserved(listener: (sid: string, runner: SpaceRunner | null, allowFolderAdd: boolean) => void) {
  listeners.push(listener);
}

/** A local mutation invalidates older observations of just its own Space. */
export function invalidateSpaceRunnerObservations(sid: string) {
  latest.set(sid, ++serial);
}

/** A guarded ack disproves the pre-write assignment, not the current one. */
export function forgetDisprovedSpaceRunner(sid: string, previous: SpaceRunner | null) {
  const all = spaceRunners();
  const current = all[sid];
  if (current !== previous && !(current && previous?.assignmentRevision &&
      current.kind === previous.kind && current.assignmentRevision === previous.assignmentRevision)) return;
  const next = { ...all };
  delete next[sid];
  batch(() => {
    spaceRunners.set(next);
    // Hide a stale folder, but don't claim the Space is now empty or attachable.
    for (const listener of listeners) listener(sid, null, false);
  });
}

/** Take a ticket at dispatch. A newer accepted answer wins, not one merely in flight. */
export function beginSpaceRunnerObservation(sid: string) {
  const stamp = ++serial;
  const inScope = sessionGuard(sid);
  const isCurrent = () => inScope() && (latest.get(sid) ?? 0) < stamp;
  return {
    isCurrent,
    publish(runner: SpaceRunner | null, allowFolderAdd = true) {
      if (!isCurrent()) return false;
      latest.set(sid, stamp);
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
  latest.clear();
  spaceRunners.set({});
});
