import { announce, batch, computed, signal } from "nsh";
import type { SpacesAccess } from "../../api/meta";
import { ApiError, apiClient, chatEvents, chatUnavailable, describe, mainPlace, onSessionEnd, pendingChatTurns, readHostedAccess, refreshChatEvents, sessionGuard, spaces } from "../../store";

export const spaceAccess = signal<SpacesAccess | null>(null);
export const spaceAccessBusy = signal(false);
export const spaceAccessError = signal("");
export const spaceAccessUnsupported = signal(false);
export const spaceAccessSaved = signal(false);

export const NOTICE_BODY = "When you ask, Metro reads the Spaces you're in, as you, and can post for you, marked 'via Metro'. The Space content it reads goes to your Chat model connection. If someone shared that connection with you, they can see those requests.";

export async function loadSpaceAccess() {
  const c = apiClient();
  if (!c || spaceAccessBusy() || spaceAccessUnsupported()) return;
  const current = sessionGuard();
  batch(() => {
    spaceAccessBusy.set(true);
    spaceAccessError.set("");
    spaceAccessSaved.set(false);
  });
  try {
    const next = await readHostedAccess(c, c.getSpacesAccess);
    if (current()) spaceAccess.set(next);
  } catch (error) {
    if (!current()) return;
    if (error instanceof ApiError && error.kind === "not_found") {
      spaceAccess.set(null);
      spaceAccessUnsupported.set(true);
    } else spaceAccessError.set(describe(error, "Could not load Chat Space access"));
  } finally {
    if (current()) spaceAccessBusy.set(false);
  }
}

export async function saveSpaceAccess(patch: Partial<SpacesAccess>) {
  const c = apiClient();
  if (!c || !spaceAccess() || spaceAccessBusy()) return false;
  const current = sessionGuard();
  batch(() => {
    spaceAccessBusy.set(true);
    spaceAccessError.set("");
    spaceAccessSaved.set(false);
  });
  try {
    const next = await readHostedAccess(c, () => c.setSpacesAccess(patch));
    if (!current()) return false;
    spaceAccess.set(next);
    if (patch.enabled !== undefined || patch.excluded_space_ids !== undefined) {
      spaceAccessSaved.set(true);
      announce("Saved");
    }
    return true;
  } catch (error) {
    if (current()) {
      const changedMembership = !!patch.excluded_space_ids && error instanceof ApiError && error.kind === "not_found";
      spaceAccessError.set(changedMembership ? "Your Space access changed. Nothing was saved."
        : describe(error, "Could not save Chat Space access"));
      if (changedMembership) refreshChatEvents(true);
    }
    return false;
  } finally {
    if (current()) spaceAccessBusy.set(false);
  }
}

export function setSpaceIncluded(id: string, included: boolean) {
  const access = spaceAccess();
  const members = new Set(spaces().map(s => s.id));
  if (!access?.enabled || !members.has(id)) return;
  const excluded = new Set(access.excluded_space_ids.filter(s => members.has(s)));
  if (included) excluded.delete(id);
  else excluded.add(id);
  void saveSpaceAccess({ excluded_space_ids: [...excluded] });
}

const acknowledgedStops = signal<ReadonlySet<string>>(new Set());
export const chatStopBusy = signal<string | null>(null);
export const chatStopError = signal("");

/**
 * Use the most recent unfinished turn; ordinary notes and tool failures
 * aren't lifecycle markers. A turn this session sent that hasn't started
 * yet (it waits for routing or an earlier turn) counts too, so Stop works
 * before its `started` marker arrives.
 */
export const runningChatTurn = computed(() => {
  if (!spaceAccess() || chatUnavailable()) return null;
  const done = new Set(acknowledgedStops());
  const events = chatEvents();
  const seen = new Set<string>();
  for (const e of events) if (e.kind === "activity" && e.activity?.id) seen.add(e.activity.id);
  for (let i = events.length - 1; i >= 0; i--) {
    const e = events[i]!, a = e.activity;
    if (e.kind !== "activity" || a?.source !== "agent" || !a.id.startsWith("chat_") || !a.phase || done.has(a.id)) continue;
    if (a.phase === "started") return a.id;
    done.add(a.id);
  }
  const pending = pendingChatTurns();
  for (let i = pending.length - 1; i >= 0; i--) {
    const p = pending[i]!;
    if (done.has(p.id) || seen.has(p.id)) continue;
    // Not shown yet, or ended by the Chat error that names this turn.
    if (!events.some(e => e.id === p.eventId) || events.some(e => e.kind === "error" && e.turnId === p.id)) continue;
    return p.id;
  }
  return null;
});

export async function stopChatReply(id: string) {
  const c = apiClient();
  if (!c || chatStopBusy() || mainPlace() !== "chat" || runningChatTurn() !== id) return false;
  const current = sessionGuard();
  chatStopBusy.set(id);
  chatStopError.set("");
  let stopped = false;
  try {
    await readHostedAccess(c, () => c.stopChatTurn(id));
    stopped = current();
  } catch (error) {
    if (!current()) return false;
    if (error instanceof ApiError && error.kind === "not_found") {
      // It may have completed before the request arrived.
      acknowledgedStops.set(new Set([...acknowledgedStops(), id]));
      announce("This Chat turn is no longer running.");
    } else chatStopError.set(describe(error, "Could not stop Metro's reply"));
  } finally {
    if (current()) {
      if (stopped) acknowledgedStops.set(new Set([...acknowledgedStops(), id]));
      chatStopBusy.set(null);
      refreshChatEvents();
    }
  }
  if (stopped) announce("Stopped Metro's reply.");
  return stopped;
}

onSessionEnd(() => {
  spaceAccess.set(null);
  spaceAccessBusy.set(false);
  spaceAccessError.set("");
  spaceAccessUnsupported.set(false);
  spaceAccessSaved.set(false);
  chatStopBusy.set(null);
  chatStopError.set("");
  acknowledgedStops.set(new Set());
});
