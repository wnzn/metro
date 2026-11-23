// Files staged in a composer, before Send. Pure state: no network, no store,
// no picker. Picking only stages a file here (its bytes stay in memory);
// nothing is uploaded until Send, so cancelling or removing a file leaves
// nothing behind on the server.
//
// Files belong to the place they were picked in ("chat", or a Space's room
// or thread key), so they never land in another place and switching away and
// back keeps them. A file Send already uploaded keeps its Library id, so a
// retry after a failed post never uploads it again.
//
// Other parts of the app (the account switcher) may read
// `hasPendingAttachments()` from here without importing any network state.

import { computed, signal } from "nsh";
import type { AssetKind } from "../../api/library";

export type StagedFile = {
  /** Unique within this run. */
  key: string;
  name: string;
  mime: string;
  kind: AssetKind;
  size: number;
  /** The picked bytes; dropped with the entry. */
  file: Blob;
  /** Set once Send uploaded it: a retry reuses it. */
  assetId: string | null;
};

const staged = signal<Record<string, readonly StagedFile[]>>({});
let nextKey = 0;

export const newStagedKey = () => `f${++nextKey}`;

/** The files staged for one place, in the order they were picked. */
export function stagedFor(scope: string): readonly StagedFile[] {
  return staged()[scope] ?? [];
}

/** Whether any place has files staged and not yet sent (sign-out or an account switch would drop them). */
export const hasPendingAttachments = computed(() => Object.values(staged()).some((list) => list.length > 0));

/** How many places have staged files (for a confirmation's wording). */
export const pendingAttachmentCount = computed(() => Object.values(staged()).reduce((n, list) => n + list.length, 0));

export function stage(scope: string, files: readonly StagedFile[]) {
  if (!files.length) return;
  staged.set({ ...staged(), [scope]: [...stagedFor(scope), ...files] });
}

export function unstage(scope: string, key: string) {
  const left = stagedFor(scope).filter((f) => f.key !== key);
  setScope(scope, left);
}

/** Remembers the Library id of a file Send uploaded. */
export function markUploaded(scope: string, key: string, assetId: string) {
  setScope(
    scope,
    stagedFor(scope).map((f) => (f.key === key ? { ...f, assetId } : f)),
  );
}

/** Drops the files a message carried (by key), keeping any picked meanwhile. */
export function dropSent(scope: string, keys: readonly string[]) {
  setScope(
    scope,
    stagedFor(scope).filter((f) => !keys.includes(f.key)),
  );
}

export function clearAllStaged() {
  staged.set({});
}

function setScope(scope: string, files: readonly StagedFile[]) {
  const next = { ...staged() };
  if (files.length) next[scope] = files;
  else delete next[scope];
  staged.set(next);
}
