// Shell approvals in a Space room or thread: who may decide (from the
// server's pending list only), the decision request, and what this person's
// own decision returned until the room's `shell_approval_resolved` arrives.
//
// - Buttons are offered only for an approval the pending list says this
//   person can decide (`can_decide`), and only when that pending row is the
//   same request the card shows: approval id, run, room and the exact raw
//   command. The server rechecks everything again.
// - One request at a time; once a decision is confirmed or refused as
//   already decided, the buttons stay away (no repeat). The busy latch is
//   owned by its operation, so an older request never clears a newer one,
//   and it clears even when the Space was purged while the request ran.
// - A 403 holds until a list read that started after it says otherwise: a
//   read already in flight can't grant the buttons back.
// - Every answer is applied only if the session and Space are still current.

import { batch, computed, signal } from "nsh";
import { ApiError } from "../../api/client";
import { isApprovalId } from "../../api/shell";
import type { PendingShellApproval, ShellApprovalRequest, ShellDecision, ShellResolution } from "../../api/shell";
import { apiClient, disconnect, isAborted, onSessionEnd, readHostedAccess, sessionGuard } from "../../store";

type PendingRead = { loadedAt: number; byId: ReadonlyMap<string, PendingShellApproval> };

/** The last pending list per Space. */
const pendingBySpace = signal<Record<string, PendingRead>>({});
/** A decision this person made here, confirmed by the server. */
const ownDecisions = signal<Record<string, ShellResolution>>({});
/** Per approval: the last refusal or failure, said on the card. */
const errors = signal<Record<string, string>>({});
/** Approvals the server says are no longer open (409/404), until the room catches up. */
const closed = signal<Record<string, "decided" | "gone">>({});
/** The decision being sent, with the operation that owns the latch. */
const deciding = signal<{ approvalId: string; op: object } | null>(null);
/** The approval whose decision is being sent. */
export const decidingApproval = computed(() => deciding()?.approvalId ?? null);

const inflight = new Map<string, Promise<void>>();
/** Each list read's number, in start order. */
let readSeq = 0;
/** Approval id → the last read number started before its 403; older reads can't grant it. */
const refusedBefore = new Map<string, number>();
const RELOAD_AFTER_MS = 5_000;

onSessionEnd(() => {
  inflight.clear();
  refusedBefore.clear();
  batch(() => {
    pendingBySpace.set({});
    ownDecisions.set({});
    errors.set({});
    closed.set({});
    deciding.set(null);
  });
});

export const ownShellDecision = (approvalId: string) => ownDecisions()[approvalId] ?? null;
export const shellApprovalError = (approvalId: string) => errors()[approvalId] ?? null;
export const shellApprovalClosed = (approvalId: string) => closed()[approvalId] ?? null;

/** The card's request, from a system event in a known room. */
export type ShownApproval = { request: ShellApprovalRequest; roomId: string };

/**
 * True only when the server's current pending list says this person may
 * decide it, and its row is exactly the shown request: same id, run, room
 * and raw command.
 */
export function canDecideShell(spaceId: string, shown: ShownApproval): boolean {
  const row = pendingBySpace()[spaceId]?.byId.get(shown.request.approvalId);
  return row?.canDecide === true && row.runId === shown.request.runId && row.roomId === shown.roomId &&
    row.command === shown.request.command;
}

const setError = (approvalId: string, message: string | null) => {
  const next = { ...errors() };
  if (message) next[approvalId] = message;
  else delete next[approvalId];
  errors.set(next);
};

/**
 * Reads the Space's pending approvals; joins a read in flight and skips a
 * fresh one. `force` always ends with a read that started after the call.
 */
export function loadShellApprovals(spaceId: string, force = false): Promise<void> {
  const c = apiClient();
  if (!c) return Promise.resolve();
  const valid = sessionGuard(spaceId);
  const running = inflight.get(spaceId);
  // A forced read waits for the older one, then reads again only for the
  // same session and Space: never for a later sign-in.
  if (running) return force ? running.then(() => (valid() && apiClient() === c ? loadShellApprovals(spaceId, true) : undefined)) : running;
  const last = pendingBySpace()[spaceId];
  if (!force && last && Date.now() - last.loadedAt < RELOAD_AFTER_MS) return Promise.resolve();
  const mine = ++readSeq;
  let read: Promise<void> | undefined;
  read = (async () => {
    await Promise.resolve();
    try {
      const list = await readHostedAccess(c, () => c.listShellApprovals(spaceId));
      if (!valid()) return;
      const byId = new Map<string, PendingShellApproval>();
      for (const a of list) {
        const refused = refusedBefore.get(a.approvalId);
        if (refused !== undefined && mine <= refused) byId.set(a.approvalId, { ...a, canDecide: false });
        else {
          refusedBefore.delete(a.approvalId);
          byId.set(a.approvalId, a);
        }
      }
      pendingBySpace.set({ ...pendingBySpace(), [spaceId]: { loadedAt: Date.now(), byId } });
    } catch (error) {
      if (!valid() || isAborted(error)) return;
      // No list: no buttons. A refused or unknown Space offers nothing to decide.
      const next = { ...pendingBySpace() };
      delete next[spaceId];
      pendingBySpace.set(next);
    } finally {
      if (inflight.get(spaceId) === read) inflight.delete(spaceId);
    }
  })();
  inflight.set(spaceId, read);
  return read;
}

function dropPending(spaceId: string, approvalId: string) {
  const read = pendingBySpace()[spaceId];
  if (!read?.byId.has(approvalId)) return;
  const byId = new Map(read.byId);
  byId.delete(approvalId);
  pendingBySpace.set({ ...pendingBySpace(), [spaceId]: { ...read, byId } });
}

/** Approve or deny, once. The caller shows the result from the signals above. */
export async function decideShell(spaceId: string, shown: ShownApproval, decision: ShellDecision): Promise<void> {
  const c = apiClient();
  const approvalId = shown.request.approvalId;
  if (!c || !isApprovalId(approvalId) || deciding() || ownDecisions()[approvalId] || closed()[approvalId] ||
      !canDecideShell(spaceId, shown)) return;
  const valid = sessionGuard(spaceId);
  const op = {};
  batch(() => {
    deciding.set({ approvalId, op });
    setError(approvalId, null);
  });
  try {
    const result = await c.decideShellApproval(spaceId, approvalId, decision);
    if (!valid()) return;
    batch(() => {
      ownDecisions.set({ ...ownDecisions(), [approvalId]: result });
      dropPending(spaceId, approvalId);
    });
  } catch (error) {
    if (!valid() || isAborted(error)) return;
    if (error instanceof ApiError && error.kind === "auth") {
      disconnect(error.message);
      return;
    }
    const status = error instanceof ApiError ? error.status : null;
    batch(() => {
      if (status === 409) {
        closed.set({ ...closed(), [approvalId]: "decided" });
        dropPending(spaceId, approvalId);
        setError(approvalId, "This request was already decided, expired or was cancelled.");
      } else if (status === 404) {
        closed.set({ ...closed(), [approvalId]: "gone" });
        dropPending(spaceId, approvalId);
        setError(approvalId, "This request is no longer available.");
      } else if (status === 403) {
        // Reads started before now can't grant it back.
        refusedBefore.set(approvalId, readSeq);
        dropPending(spaceId, approvalId);
        setError(approvalId, "You can't decide this request.");
      } else if (error instanceof ApiError && (error.kind === "network" || error.kind === "timeout" || error.kind === "protocol")) {
        setError(approvalId, "The server did not confirm the decision. Check the room before trying again.");
      } else {
        setError(approvalId, "The server refused the decision.");
      }
    });
    // Who may decide can change with the live policy: read it again, without holding the latch.
    if (deciding()?.op === op) deciding.set(null);
    await loadShellApprovals(spaceId, true);
  } finally {
    // Only this operation releases its own latch, whatever happened to the Space.
    if (deciding()?.op === op) deciding.set(null);
  }
}
