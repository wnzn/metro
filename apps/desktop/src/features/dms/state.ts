import { announce, batch, computed, effect, lifecycle, signal, untracked } from "nsh";
import type { DirectMessage } from "../../api/dms";
import {
  ApiError, apiClient, isAborted, mainPlace, me, onSessionEnd, onTransientClear, openPlace,
  readHostedAccess, registerDmPoll, requestDmPoll, settingsOpen, startupReadGuard,
} from "../../store";
import type { Reactions, RoomEvent } from "../../store";

export const dms = signal<DirectMessage[]>([]);
export const dmListError = signal("");
export const dmListLoaded = signal(false);
export const dmProblem = signal("");
export const dmSending = signal(false);
export const dmStarting = signal(false);
export const dmHiding = signal<string | null>(null);
export const dmReactionPending = signal<string | null>(null);
export const dmDraft = signal("");
export const dmReply = signal<RoomEvent | null>(null);
export const dmEvents = signal<readonly RoomEvent[]>([]);
export const dmReactions = signal<Reactions>({});
export const dmLoaded = signal(false);
const visit = signal<{ dm: DirectMessage; listed: boolean } | null>(null);
const drafts = signal<ReadonlyMap<string, string>>(new Map());
export const dmDraftPending = () => !!dmDraft().trim() || !!drafts().size || dmSending();
export const activeDm = computed(() => {
  const v = visit();
  return v && mainPlace() === "dm" ? dms().find(dm => dm.id === v.dm.id) ?? v.dm : null;
});
export const dmUnread = computed(() => dms().reduce((sum, dm) => sum + dm.unread, 0));

let listRead: AbortController | undefined;
let readThrough = 0;
let firstId = 0;
let tail = 0;
let reactionOffset = 0;
const denied = (error: unknown) => error instanceof ApiError && (error.kind === "not_found" || error.kind === "forbidden");
const loadFailure = "Couldn't load this conversation. Try again.";

function cancelListRead() {
  listRead?.abort();
  listRead = undefined;
}

function rememberDraft(id: string, text = "") {
  const copy = new Map(drafts());
  copy.delete(id);
  if (text.trim()) copy.set(id, text);
  if (copy.size > 200) copy.delete(copy.keys().next().value!);
  drafts.set(copy);
}

function clearTranscript() {
  batch(() => {
    dmEvents.set([]);
    dmReactions.set({});
    dmLoaded.set(false);
    dmReply.set(null);
  });
  readThrough = firstId = tail = reactionOffset = 0;
}

function leaveDm() {
  const id = visit()?.dm.id;
  if (id) rememberDraft(id, dmDraft());
  visit.set(null);
  clearTranscript();
  dmDraft.set("");
  dmSending.set(false);
  dmReactionPending.set(null);
  dmProblem.set("");
}
onTransientClear(keepReply => { if (!keepReply) leaveDm(); });
onSessionEnd(() => {
  leaveDm();
  drafts.set(new Map());
  dms.set([]);
  dmListError.set("");
  dmListLoaded.set(false);
  dmStarting.set(false);
  dmHiding.set(null);
  cancelListRead();
});

export function refreshDmList() {
  void pollList(true);
  requestDmPoll();
}

export function openDm(dm: DirectMessage) {
  if (activeDm()?.id === dm.id && !settingsOpen()) return;
  batch(() => {
    openPlace("dm", { subrouteChanged: true });
    visit.set({ dm, listed: dms().some(d => d.id === dm.id) });
    dmDraft.set(drafts().get(dm.id) ?? "");
    rememberDraft(dm.id);
  });
  requestDmPoll();
}

export function openMessages() {
  if (mainPlace() === "dm" && activeDm()) clearSelection();
  else openPlace("dm");
  refreshDmList();
}

function clearSelection() {
  // Invalidates in-flight intents and source dialogs even though the place stays "dm".
  openPlace("dm", { subrouteChanged: true });
}

function dropDm(id: string, problem?: string) {
  batch(() => {
    dms.set(dms().filter(dm => dm.id !== id));
    if (visit()?.dm.id === id) {
      leaveDm();
      if (problem) dmProblem.set(problem);
    }
    rememberDraft(id);
  });
}

export async function startDm(userId: string, pickerCurrent: () => boolean) {
  const api = apiClient();
  if (!api || !userId || userId === me() || dmStarting()) return;
  const valid = startupReadGuard();
  dmStarting.set(true);
  try {
    const dm = await readHostedAccess(api, () => api.startDm(userId));
    if (valid() && pickerCurrent()) openDm(dm);
    refreshDmListIfCurrent(api);
  } finally {
    if (apiClient() === api) dmStarting.set(false);
  }
}

function refreshDmListIfCurrent(api: ReturnType<typeof apiClient>) {
  if (api && apiClient() === api) refreshDmList();
}

export async function hideDm(id: string) {
  const api = apiClient();
  if (!api || dmHiding()) return false;
  cancelListRead();
  dmHiding.set(id);
  try {
    await readHostedAccess(api, () => api.hideDm(id));
    if (apiClient() !== api) return false;
    dropDm(id);
    announce("Conversation hidden");
    return true;
  } catch (error) {
    if (apiClient() !== api) return false;
    if (denied(error)) {
      dropDm(id, "This conversation is no longer available.");
      return true;
    }
    throw error;
  } finally {
    if (apiClient() === api) {
      dmHiding.set(null);
      refreshDmList();
    }
  }
}

async function pollList(replace = false) {
  const api = apiClient();
  if (!api || dmHiding() || (listRead && !replace)) return;
  cancelListRead();
  const run = listRead = new AbortController();
  const live = () => apiClient() === api && listRead === run;
  try {
    const rows = await readHostedAccess(api, () => api.listDms(run.signal));
    if (!live()) return;
    const v = visit();
    const current = rows.find(dm => dm.id === v?.dm.id);
    if (v && current) v.listed = true;
    // Hidden or deleted elsewhere. A full capped list can't prove absence.
    if (v?.listed && !current && rows.length < 200) dropDm(v.dm.id, "This conversation is no longer available.");
    // A read receipt may have completed while the server was answering this list.
    dms.set(rows.map(dm => dm.id === v?.dm.id && dm.lastId <= readThrough ? { ...dm, unread: 0 } : dm));
    dmListError.set("");
    dmListLoaded.set(true);
  } catch (error) {
    if (live() && !isAborted(error)) {
      dmListError.set("Couldn't load direct messages. Try again.");
    }
  } finally {
    if (listRead === run) listRead = undefined;
  }
}

function adoptPage(page: { events: RoomEvent[]; reactions: Reactions }) {
  const rows = new Map(dmEvents().map(e => [e.id, e]));
  const reactions = { ...dmReactions() };
  for (const event of page.events) {
    if (!rows.has(event.id)) rows.set(event.id, event);
    if (page.reactions[event.id]) reactions[event.id] = page.reactions[event.id]!;
    else delete reactions[event.id];
  }
  const events = [...rows.values()].sort((a, b) => a.id - b.id).slice(-500);
  const oldest = events[0]?.id ?? 0;
  for (const id of Object.keys(reactions)) if (Number(id) < oldest) delete reactions[Number(id)];
  batch(() => {
    if (events.length !== dmEvents().length || events.at(-1)?.id !== dmEvents().at(-1)?.id) dmEvents.set(events);
    dmReactions.set(reactions);
    dmLoaded.set(true);
  });
}

async function pollActive(api: NonNullable<ReturnType<typeof apiClient>>) {
  const v = visit();
  if (!v || mainPlace() !== "dm" || dmHiding() === v.dm.id) return;
  const { id } = v.dm;
  const current = () => apiClient() === api && visit() === v && dmHiding() !== id;
  const page = (after: number) => readHostedAccess(api, () => api.dmEvents(id, after));
  try {
    // Revalidate the visible floor, including a hide on another device. The
    // API supplies reactions only for the messages in each returned page.
    const head = await page(0);
    if (!current()) return;
    const first = head.events[0]?.id ?? 0;
    if (firstId && first !== firstId) {
      clearTranscript();
      dmProblem.set("");
    }
    firstId = first;
    adoptPage(head);
    if (dmProblem() === loadFailure) dmProblem.set("");
    const end = head.events.at(-1)?.id ?? 0;
    tail = Math.max(tail, end);
    if (tail) {
      const more = await page(tail);
      if (!current()) return;
      adoptPage(more);
      tail = more.events.at(-1)?.id ?? tail;
      const older = dmEvents().filter(e => e.id > end && e.id <= tail);
      if (older.length) {
        const after = older[reactionOffset % older.length]!.id;
        const refresh = await page(after === older[0]!.id ? end : after - 1);
        if (!current()) return;
        adoptPage(refresh);
        reactionOffset += Math.max(1, refresh.events.length);
      }
    }
    if (tail > readThrough && lifecycle() === "active" && !settingsOpen()) {
      await readHostedAccess(api, () => api.readDm(id, tail));
      if (!current()) return;
      readThrough = tail;
      dms.set(dms().map(dm => dm.id === id && dm.lastId <= readThrough ? { ...dm, unread: 0 } : dm));
    }
  } catch (error) {
    if (!current() || isAborted(error)) return;
    if (denied(error)) {
      cancelListRead();
      dropDm(id, "This conversation is no longer available.");
    } else if (error instanceof ApiError && error.transient) throw error;
    else if (!dmLoaded()) dmProblem.set(loadFailure);
  }
}

registerDmPoll(async () => {
  const api = apiClient();
  if (api) await pollActive(api);
});

export function watchDmFocus() {
  effect(() => {
    const active = lifecycle() === "active";
    if (!me()) return;
    untracked(() => {
      if (active || !dmListLoaded()) refreshDmList();
    });
    const timer = setInterval(() => void pollList(), active ? 5_000 : 30_000);
    return () => clearInterval(timer);
  });
}

export async function sendDm() {
  const api = apiClient(), v = visit(), raw = dmDraft(), reply = dmReply();
  const text = raw.trim();
  if (!api || !v || !dmLoaded() || !text || dmSending() || dmHiding() === v.dm.id) return;
  const current = () => apiClient() === api && visit() === v;
  dmProblem.set("");
  dmSending.set(true);
  try {
    await readHostedAccess(api, () => api.postDm(v.dm.id, text, reply?.id));
    if (current()) {
      if (dmDraft() === raw) dmDraft.set("");
      if (dmReply() === reply) dmReply.set(null);
    } else if (apiClient() === api && drafts().get(v.dm.id) === raw) {
      rememberDraft(v.dm.id);
    }
    refreshDmListIfCurrent(api);
  } catch (error) {
    if (current()) {
      if (denied(error) && !reply) dropDm(v.dm.id, "This conversation is no longer available.");
      else {
        dmProblem.set(denied(error) ? "That message is no longer available to reply to." : "Couldn't send the message. Try again.");
        if (denied(error)) requestDmPoll();
      }
    }
  } finally {
    if (current()) dmSending.set(false);
  }
}

export async function reactDm(eventId: number, emoji: string) {
  const api = apiClient(), v = visit(), myId = me();
  if (!api || !v || !myId || dmReactionPending() || dmHiding() === v.dm.id) return;
  const current = () => apiClient() === api && visit() === v;
  const mine = dmReactions()[eventId]?.[emoji]?.includes(`user:${myId}`) ?? false;
  dmReactionPending.set(`${eventId}|${emoji}`);
  dmProblem.set("");
  try {
    await readHostedAccess(api, () => api.reactDm(v.dm.id, eventId, emoji, !mine));
    if (current()) {
      const byEmoji = { ...dmReactions()[eventId] };
      const existing = (byEmoji[emoji] ?? []).filter(id => id !== `user:${myId}`);
      byEmoji[emoji] = mine ? existing : [...existing, `user:${myId}`];
      dmReactions.set({ ...dmReactions(), [eventId]: byEmoji });
    }
    requestDmPoll();
  } catch {
    if (current()) {
      dmProblem.set("Couldn't update the reaction. Try again.");
      requestDmPoll();
    }
  } finally {
    if (current()) dmReactionPending.set(null);
  }
}
