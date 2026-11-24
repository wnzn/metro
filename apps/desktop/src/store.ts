// App state and the sync loop. Pages read these signals and call these
// functions; nothing here knows about the UI.
//
// Model
// - A user signs in to a server with a bearer token, sees the Spaces they own
//   or belong to (GET /v1/spaces) and works in one Space at a time. Each Space
//   has its own rooms (one is its lobby) and its own optional paired runner.
//   There is no shared server-wide lobby: an account with no Space sees how to
//   create one or get added.
//
// Sync
// - One loop per connection ("session"). Each tick refreshes the Space list
//   and the active Space's rooms now and then, and polls the active room with
//   ?after=<cursor>.
// - Everything cached is keyed by Space + room (every Space's lobby is called
//   "lobby"), and every request checks on return that its session is current
//   and its Space was not purged meanwhile (a per-Space epoch), so a late
//   answer never lands in another Space or after access was lost.
// - Only event ids above a room's cursor are accepted, so repeated or
//   overlapping responses never duplicate events.
// - 401: the session ends and the connect screen says why. 403/404 on a
//   Space's endpoints: the Space list is reloaded; a Space we lost access to is
//   purged (rooms, events, cursors, in-flight work) and we move elsewhere.
// - Network errors, timeouts, 5xx and 429 back off exponentially (1s .. 30s)
//   and keep what is on screen.
//
// Secrets
// - The access token lives in this module's memory only (never Storage, logs
//   or UI) and is dropped on disconnect.
// - A runner token is shown once, right after the owner issues it, and is
//   cleared when the owner hides it, leaves the management view, switches
//   Space or room, or disconnects. The same holds for registered runner and
//   device tokens shown in Settings (src/settings.ts registers its clearers
//   here with onTransientClear / onSessionEnd).

import { batch, computed, effect, signal, Storage, untracked } from "nsh";
import { ApiError, createClient, parseReaction } from "./api/client";
import type { Client, Me, Member, Mention, Room, RoomEvent, Run, Space, TurnModelChoice } from "./api/client";
export { ApiError };
import { DEFAULT_SERVER_URL, parseServerUrl } from "./api/serverUrl";

export type { Member, Mention, Room, RoomEvent, Run, Space };

export const LOBBY = "lobby";
export const POLL_MS = 1500;
const LISTS_REFRESH_MS = 10_000;
const RUNS_REFRESH_MS = 3000;
const CHAT = "chat"; // the personal Chat's cache key (room keys always contain \u0001)
const MAX_BACKOFF_MS = 30_000;
const MAX_EVENTS_PER_ROOM = 2000;
const SERVER_URL_KEY = "metro.serverUrl"; // not secret: remembered between launches
const LAST_SPACE_KEY = "metro.lastSpace"; // not secret: { server, space }

export type Status =
  | { phase: "signedOut" }
  | { phase: "connecting" }
  | { phase: "online" }
  | { phase: "retrying"; attempt: number; retryInMs: number; message: string };

export type View = "room" | "manage";
/**
 * What the user is looking at: the personal Chat, the active Space, the
 * user's own Search, Library (Documents & media) and Scheduled tasks (all
 * outside every Space), or Settings, which opens as a modal over whichever of
 * those is underneath.
 */
export type Place = "chat" | "space" | "settings" | "search" | "library" | "schedules";
/** Where the main panel is under any modal: never Settings. */
export type MainPlace = Exclude<Place, "settings">;
/** The pages of Settings. Reachable with or without any Space. */
export type SettingsSection = "profile" | "runners" | "providers" | "routing" | "monitoring" | "appearance";

// ------------------------------------------------------------------ state

// Declared before the computeds below: NSH evaluates computeds eagerly.
const EMPTY_EVENTS: readonly RoomEvent[] = [];
const EMPTY_REACTIONS: Reactions = {};

/** Reactions folded per event: emoji -> reactor keys ("user:<id>" | "agent:<id>"), in order of arrival. */
export type Reactions = Record<number, Record<string, readonly string[]>>;
export const REACTION_EMOJI = ["👍", "👀", "✅", "🙏", "❤️", "🎉"] as const;
const EMPTY_ROOMS: readonly Room[] = [];

export const status = signal<Status>({ phase: "signedOut" });
/** Why the last connect attempt or session ended; shown on the connect screen. */
export const connectError = signal<string | null>(null);
export const serverUrlInput = signal(DEFAULT_SERVER_URL);
export const connectedTo = signal<string | null>(null);
/** The signed-in user's id, which others use to add them to a Space. */
export const me = signal<string | null>(null);
/** The signed-in user is an admin of the server's one organization (METRO_ORG_ADMINS). */
export const orgAdmin = signal(false);

// The main panel's place, kept while Settings is open over it, so closing
// Settings returns to the same Chat, Space, room, view and draft.
const basePlace = signal<MainPlace>("chat");
/** Settings is open as a modal over the main panel. */
export const settingsOpen = signal(false);
/** What the user is looking at: "settings" while the Settings modal is open, else the main panel's place. */
export const place = computed<Place>(() => (settingsOpen() ? "settings" : basePlace()));
/** The main panel's place, under the Settings modal too. */
export const mainPlace = computed<MainPlace>(() => basePlace());
export const settingsSection = signal<SettingsSection>("runners");
export const spaces = signal<Space[]>([]);
export const activeSpaceId = signal<string | null>(null);
export const activeSpace = computed(() => spaces().find((s) => s.id === activeSpaceId()) ?? null);
export const switchingSpace = signal(false);

const roomsBySpace = signal<Record<string, readonly Room[]>>({});
export const rooms = computed(() => roomsBySpace()[activeSpaceId() ?? ""] ?? EMPTY_ROOMS);
export const activeRoomId = signal<string | null>(null);
export const activeRoom = computed(() => rooms().find((r) => r.id === activeRoomId()) ?? null);

const eventsByScope = signal<Record<string, readonly RoomEvent[]>>({});
const loadedScopes = signal<Record<string, true>>({});
const activeScope = computed(() => {
  if (basePlace() === "chat") return CHAT;
  const sid = activeSpaceId();
  const rid = activeRoomId();
  return sid && rid ? scopeKey(sid, rid) : "";
});
export const activeEvents = computed(() => eventsByScope()[activeScope()] ?? EMPTY_EVENTS);
export const activeLoaded = computed(() => !!loadedScopes()[activeScope()]);
const reactionsByScope = signal<Record<string, Reactions>>({});
export const activeReactions = computed(() => reactionsByScope()[activeScope()] ?? EMPTY_REACTIONS);
/** `${eventId}|${emoji}` while the user's toggle is on its way. */
export const reactionPending = signal<string | null>(null);

export const view = signal<View>("room");
/** Set when the server has no personal Chat for this user (403/404); polling it stops. */
export const chatUnavailable = signal<string | null>(null);

// Runs and dispatch (meta controls in the Chat)
export const runs = signal<Run[]>([]);
export const runsLoaded = signal(false);
export const runsError = signal<string | null>(null);
export const runBusy = signal<string | null>(null);
/** Where a task from the Chat goes: an explicit Space and room, never implied. */
export const dispatchTarget = signal<{ spaceId: string; roomId: string | null } | null>(null);
export const dispatchDraft = signal("");
export const dispatching = signal(false);
export const dispatchError = signal<string | null>(null);
export const dispatchNotice = signal<string | null>(null);
export const draft = signal("");
export const sending = signal<null | "message" | "agent">(null);
export const demoPending = signal(false);
export const creatingRoom = signal(false);
export const creatingSpace = signal(false);
/** The last failed action (send, demo, create), shown near where it happened. */
export const actionError = signal<string | null>(null);
/** A short confirmation or explanation of something that just happened. */
export const notice = signal<string | null>(null);

// Members per Space (any member may list them; owners manage them)
const membersBySpace = signal<Record<string, Member[]>>({});
export const members = computed(() => membersBySpace()[activeSpaceId() ?? ""] ?? null);
export const membersError = signal<string | null>(null);

// Mentions picked in the room composer. Only for the active room; cleared when
// the room or Space changes, and pruned when their text leaves the draft.
export const mentions = signal<Mention[]>([]);

// Space management (owners)
export const manageError = signal<string | null>(null);
export const memberBusy = signal<string | null>(null);

// ------------------------------------------------------------------ session internals

let client: Client | null = null;
let abort: AbortController | null = null;
let session = 0;
let timer: number | undefined;
let ticking = false;
let tickAgain = false;
let failures = 0;
let lastListsRefresh = 0;
let lastRunsRefresh = 0;
const cursors = new Map<string, number>();
const inflight = new Map<string, Promise<void>>();
const pollAgain = new Set<string>();
const lastRoomBySpace = new Map<string, string>();
const spaceEpoch = new Map<string, number>();

// Other state modules (src/settings.ts) hook into the session's lifecycle
// here instead of this module importing them.
const transientClearers: (() => void)[] = [];
const sessionEnders: (() => void)[] = [];
const manageOpeners: ((spaceId: string) => void)[] = [];
/** Called whenever the place, Space, room or view changes: drop anything shown once. */
export function onTransientClear(fn: () => void) {
  transientClearers.push(fn);
}
/** Called when the session ends (disconnect, 401): drop everything of the session. */
export function onSessionEnd(fn: () => void) {
  sessionEnders.push(fn);
}
const sessionStarters: ((session: { baseUrl: string; token: string }) => void)[] = [];
/**
 * Called once a session is online. Only src/device uses it: it hands the
 * token straight to the packaged launcher's loopback bridge (POST /v1/session)
 * and keeps no copy.
 */
export function onSessionStart(fn: (session: { baseUrl: string; token: string }) => void) {
  sessionStarters.push(fn);
}
/** Called after an owner opens a Space's management view. */
export function onManageOpen(fn: (spaceId: string) => void) {
  manageOpeners.push(fn);
}

/** The session's client, or null when signed out. Never exposes the token. */
export function apiClient(): Client | null {
  return client;
}
/** A guard for async work started now: still the same session (and Space, when given). */
export function sessionGuard(spaceId?: string) {
  return guard(spaceId);
}

function scopeKey(spaceId: string, roomId: string) {
  return `${spaceId}\u0001${roomId}`;
}
const epochOf = (spaceId: string) => spaceEpoch.get(spaceId) ?? 0;

/** A guard for async work: still the same session, and the space not purged since. */
function guard(spaceId?: string) {
  const sess = session;
  const epoch = spaceId ? epochOf(spaceId) : 0;
  return () => sess === session && (!spaceId || epochOf(spaceId) === epoch);
}

/** Loads the remembered server URL (never the token). */
export async function restorePreferences(mayRestore: () => boolean = () => true) {
  try {
    const saved = await Storage.getItem(SERVER_URL_KEY);
    if (saved && mayRestore() && status().phase === "signedOut") serverUrlInput.set(saved);
  } catch {
    /* storage unavailable: keep the default */
  }
}

// ------------------------------------------------------------------ connect / disconnect

export type PreparedConnection = Readonly<{ baseUrl: string }>;
const candidates = new WeakMap<PreparedConnection, {
  token: string; who: Me; list: Space[]; remembered: string | null; signal: AbortSignal;
}>();

function credentials(urlInput: string, tokenInput: string) {
  const parsed = parseServerUrl(urlInput);
  if (!parsed.ok) throw new Error(parsed.error);
  const token = tokenInput.trim();
  if (!token) throw new ApiError("auth", "Enter your access token (a dev token or a JWT from your identity provider).");
  if (/\s/.test(token)) throw new ApiError("auth", "The access token contains spaces or line breaks; paste only the token itself.");
  return { baseUrl: parsed.baseUrl, token };
}

/** Probe a candidate without touching the live session, including on failure. */
export async function prepareConnection(baseUrl: string, token: string, signal: AbortSignal): Promise<PreparedConnection> {
  const target = credentials(baseUrl, token);
  const c = createClient({ baseUrl: target.baseUrl, token: () => target.token, signal });
  const [who, list, remembered] = await Promise.all([c.me(), c.listSpaces(), rememberedSpace(target.baseUrl)]);
  if (signal.aborted) throw new ApiError("aborted", "Cancelled");
  const prepared = Object.freeze({ baseUrl: target.baseUrl });
  candidates.set(prepared, { token: target.token, who, list, remembered, signal });
  return prepared;
}

function beginSession(baseUrl: string, token: string) {
  endSession();
  const sess = ++session;
  abort = new AbortController();
  client = createClient({ baseUrl, token: () => (sess === session ? token : ""), signal: abort.signal });
  return { sess, c: client };
}

function publishIdentity(baseUrl: string, who: Me, list: Space[], remember?: boolean) {
  batch(() => {
    me.set(who.id);
    orgAdmin.set(who.isOrgAdmin);
    spaces.set(list);
    connectedTo.set(baseUrl);
  });
  if (remember !== false) Storage.setItem(SERVER_URL_KEY, baseUrl).catch(() => {});
}

function startOnline(baseUrl: string, token: string) {
  lastListsRefresh = Date.now();
  failures = 0;
  status.set({ phase: "online" });
  for (const fn of sessionStarters) fn({ baseUrl, token });
  kick();
}

/** Consume a verified candidate synchronously. Subsequent requests have their own session lifetime. */
export function adoptConnection(prepared: PreparedConnection, opts: { remember?: boolean } = {}): void {
  const candidate = candidates.get(prepared);
  if (!candidate) throw new Error("Connection was not prepared or was already adopted.");
  candidates.delete(prepared);
  if (candidate.signal.aborted) throw new ApiError("aborted", "Cancelled");
  batch(() => {
    beginSession(prepared.baseUrl, candidate.token);
    connectError.set(null);
    publishIdentity(prepared.baseUrl, candidate.who, candidate.list, opts.remember);
    startOnline(prepared.baseUrl, candidate.token);
    if (candidate.list.some((s) => s.id === candidate.remembered)) {
      void enterSpace(candidate.remembered!, { place: "chat", spaceId: null });
    }
  });
}

/**
 * `remember: false` (the desktop-hosted local workspace): don't save this URL
 * as the server to offer next time, so the remote choice stays as it was.
 */
export async function connect(urlInput: string, tokenInput: string, opts: { remember?: boolean } = {}): Promise<void> {
  let target: ReturnType<typeof credentials>;
  try {
    target = credentials(urlInput, tokenInput);
  } catch (error) {
    connectError.set((error as Error).message);
    return;
  }
  const { baseUrl, token } = target;
  const { sess, c } = beginSession(baseUrl, token);
  batch(() => {
    connectError.set(null);
    status.set({ phase: "connecting" });
  });

  try {
    const [who, list] = await Promise.all([c.me(), c.listSpaces()]);
    if (sess !== session) return;
    publishIdentity(baseUrl, who, list, opts.remember);
    const remembered = await rememberedSpace(baseUrl);
    if (sess !== session) return;
    // A returning user lands in their last Space; everyone else in their Chat.
    const initial = list.find((s) => s.id === remembered) ?? null;
    if (initial) await enterSpace(initial.id, { place: "chat", spaceId: null });
    if (sess !== session) return;
    startOnline(baseUrl, token);
  } catch (error) {
    if (sess !== session) return;
    endSession();
    connectError.set(describe(error, "Could not connect"));
  }
}

export function disconnect(reason?: string) {
  endSession();
  connectError.set(reason ?? null);
}

/** Retry now instead of waiting for the backoff. */
export function retryNow() {
  if (status().phase === "retrying") kick();
}

// ------------------------------------------------------------------ spaces

export function openChat() {
  if (basePlace() === "chat" && !settingsOpen()) return;
  batch(() => {
    clearTransient();
    view.set("room");
    settingsOpen.set(false);
    basePlace.set("chat");
    actionError.set(null);
    notice.set(null);
  });
  lastRunsRefresh = 0;
  kick();
}

/** Opens Search, Library or Scheduled tasks: the user's own pages, outside every Space. */
export function openPlace(where: "search" | "library" | "schedules") {
  if (basePlace() === where && !settingsOpen()) return;
  batch(() => {
    clearTransient();
    view.set("room");
    settingsOpen.set(false);
    basePlace.set(where);
    actionError.set(null);
    notice.set(null);
  });
}

/**
 * Where a search result (or anything else) points: the personal Chat when
 * `spaceId` is null, otherwise a room or thread of a Space.
 */
export type Location = { spaceId: string | null; roomId: string | null; eventId?: number };

/**
 * The event to bring into view after `openLocation`, keyed by where it is
 * (`chat` or `<space>\u0001<room>`). Conversations may read it; it is cleared
 * on any move and at sign-out.
 */
export const focusTarget = signal<{ scope: string; eventId: number } | null>(null);

/**
 * Opens what a location points at. Only Spaces the user is a member of now
 * can be opened; a room or thread that isn't visible says so instead of
 * opening something else. Resolves to whether it got there.
 */
export async function openLocation(target: Location): Promise<boolean> {
  if (!client) return false;
  if (target.spaceId === null) {
    if (basePlace() !== "chat" || settingsOpen()) openChat();
    if (target.eventId !== undefined) focusTarget.set({ scope: CHAT, eventId: target.eventId });
    return true;
  }
  const sid = target.spaceId;
  if (!spaces().some((s) => s.id === sid)) {
    notice.set("That Space isn't available to you any more.");
    return false;
  }
  const rid = target.roomId;
  if (rid) lastRoomBySpace.set(sid, rid);
  await selectSpace(sid);
  if (activeSpaceId() !== sid || basePlace() !== "space") return false;
  if (!rid) return true;
  if (!rooms().some((r) => r.id === rid)) {
    try {
      await refreshLists(true); // a thread created moments ago
    } catch {
      /* the loop reports connection problems */
    }
  }
  if (activeSpaceId() !== sid) return false;
  if (!rooms().some((r) => r.id === rid)) {
    notice.set("That room or thread isn't visible to you (yet).");
    return false;
  }
  selectRoom(rid);
  if (target.eventId !== undefined) focusTarget.set({ scope: scopeKey(sid, rid), eventId: target.eventId });
  return true;
}

/**
 * Opens Settings at `section`, as a modal over the main panel, or moves the
 * open modal to that section. Works with no Space at all. What is underneath
 * (place, Space, room, view, drafts, scroll) is kept for closeSettings; only
 * what is shown once (tokens, confirmations) is dropped, as on any move.
 */
export function openSettings(section: SettingsSection = settingsSection()) {
  if (settingsOpen() && settingsSection() === section) return;
  batch(() => {
    clearTransient();
    settingsSection.set(section);
    settingsOpen.set(true);
  });
}

/** Closes the Settings modal and returns to what was underneath, as it was. */
export function closeSettings() {
  if (!settingsOpen()) return;
  batch(() => {
    clearTransient();
    settingsOpen.set(false);
  });
}

export async function selectSpace(spaceId: string): Promise<void> {
  if (switchingSpace()) return;
  if (spaceId === activeSpaceId() && roomsBySpace()[spaceId]) {
    if (basePlace() === "space" && !settingsOpen()) return;
    batch(() => {
      clearTransient();
      view.set("room");
      settingsOpen.set(false);
      basePlace.set("space");
      actionError.set(null);
      notice.set(null);
    });
    kick();
    return;
  }
  await enterSpace(spaceId, { place: basePlace(), spaceId: activeSpaceId() });
  kick();
}

export async function createSpace(nameInput: string): Promise<boolean> {
  const name = nameInput.trim();
  const c = client;
  if (!c || creatingSpace()) return false;
  if (!name) {
    actionError.set("Space name can't be empty.");
    return false;
  }
  const ok = guard();
  batch(() => {
    creatingSpace.set(true);
    actionError.set(null);
  });
  try {
    const space = await c.createSpace(name);
    if (!ok()) return false;
    spaces.set([...spaces().filter((s) => s.id !== space.id), space]);
    await enterSpace(space.id, { place: basePlace(), spaceId: activeSpaceId() });
    kick();
    return true;
  } catch (error) {
    if (ok()) handleActionError(error, "Could not create the Space");
    return false;
  } finally {
    if (ok()) creatingSpace.set(false);
  }
}

/**
 * Makes `spaceId` active: loads its rooms if needed and opens its last room,
 * else its lobby. If the Space can't be opened, purges it on 403/404 and
 * returns to where the user was.
 */
async function enterSpace(spaceId: string, previous: { place: MainPlace; spaceId: string | null }): Promise<void> {
  const c = client;
  if (!c) return;
  const currentSession = guard();
  const ok = guard(spaceId);
  batch(() => {
    clearTransient();
    view.set("room");
    settingsOpen.set(false);
    basePlace.set("space");
    activeSpaceId.set(spaceId);
    activeRoomId.set(pickRoom(spaceId, roomsBySpace()[spaceId] ?? EMPTY_ROOMS));
    actionError.set(null);
    notice.set(null);
    switchingSpace.set(true);
  });
  try {
    if (!roomsBySpace()[spaceId]) {
      const list = await c.listRooms(spaceId);
      if (!ok()) return;
      batch(() => {
        roomsBySpace.set({ ...roomsBySpace(), [spaceId]: list });
        if (activeSpaceId() === spaceId) activeRoomId.set(pickRoom(spaceId, list));
      });
    }
    if (activeSpaceId() === spaceId) rememberSpace(spaceId);
  } catch (error) {
    if (!ok() || isAborted(error)) return;
    if (error instanceof ApiError && error.kind === "auth") {
      disconnect(error.message);
      return;
    }
    const name = spaces().find((s) => s.id === spaceId)?.name ?? spaceId;
    if (error instanceof ApiError && (error.kind === "forbidden" || error.kind === "not_found")) purgeSpace(spaceId, true);
    else purgeSpace(spaceId, false); // keep it listed; just don't keep half-loaded state
    const back = previous.spaceId && spaces().some((s) => s.id === previous.spaceId) && roomsBySpace()[previous.spaceId] ? previous.spaceId : null;
    batch(() => {
      activeSpaceId.set(back);
      activeRoomId.set(back ? pickRoom(back, roomsBySpace()[back] ?? EMPTY_ROOMS) : null);
      basePlace.set(back && previous.place === "space" ? "space" : "chat");
      actionError.set(`Couldn't open ${name}: ${describe(error, "error")}`);
    });
  } finally {
    if (currentSession()) switchingSpace.set(false);
  }
}

/** Loads a Space's rooms into the cache if they aren't there (for dispatch and runs). */
async function ensureRooms(spaceId: string): Promise<readonly Room[]> {
  const cached = roomsBySpace()[spaceId];
  if (cached) return cached;
  const c = client;
  if (!c) return EMPTY_ROOMS;
  const ok = guard(spaceId);
  const list = await c.listRooms(spaceId);
  if (!ok()) return EMPTY_ROOMS;
  roomsBySpace.set({ ...roomsBySpace(), [spaceId]: list });
  return list;
}

function pickRoom(spaceId: string, list: readonly Room[]): string | null {
  const last = lastRoomBySpace.get(spaceId);
  if (last && list.some((r) => r.id === last)) return last;
  return (
    list.find((r) => r.id === LOBBY)?.id ??
    list.find((r) => r.name.trim().toLowerCase() === LOBBY)?.id ??
    list[0]?.id ??
    null
  );
}

/** Forgets everything cached for a Space; `unlist` also removes it from the list. */
function purgeSpace(spaceId: string, unlist: boolean) {
  spaceEpoch.set(spaceId, epochOf(spaceId) + 1);
  const prefix = scopeKey(spaceId, "");
  for (const key of [...cursors.keys()]) if (key.startsWith(prefix)) cursors.delete(key);
  for (const key of [...inflight.keys()]) if (key.startsWith(prefix)) inflight.delete(key);
  for (const key of [...pollAgain]) if (key.startsWith(prefix)) pollAgain.delete(key);
  lastRoomBySpace.delete(spaceId);
  const strip = <T,>(record: Record<string, T>) => {
    const next: Record<string, T> = {};
    for (const key of Object.keys(record)) if (!key.startsWith(prefix)) next[key] = record[key];
    return next;
  };
  batch(() => {
    stashedDrafts.set(strip(stashedDrafts()));
    const { [spaceId]: _gone, ...restRooms } = roomsBySpace();
    roomsBySpace.set(restRooms);
    eventsByScope.set(strip(eventsByScope()));
    loadedScopes.set(strip(loadedScopes()));
    reactionsByScope.set(strip(reactionsByScope()));
    if (unlist) spaces.set(spaces().filter((s) => s.id !== spaceId));
    if (dispatchTarget()?.spaceId === spaceId) dispatchTarget.set(null);
    const { [spaceId]: _members, ...restMembers } = membersBySpace();
    membersBySpace.set(restMembers);
    if (activeSpaceId() === spaceId) clearTransient();
  });
}

async function rememberedSpace(server: string): Promise<string | null> {
  try {
    const raw = await Storage.getItem(LAST_SPACE_KEY);
    const saved = raw ? JSON.parse(raw) : null;
    return saved && saved.server === server && typeof saved.space === "string" ? saved.space : null;
  } catch {
    return null;
  }
}

function rememberSpace(spaceId: string) {
  const server = connectedTo();
  if (server) Storage.setItem(LAST_SPACE_KEY, JSON.stringify({ server, space: spaceId })).catch(() => {});
}

// ------------------------------------------------------------------ rooms

export function selectRoom(roomId: string) {
  const sid = activeSpaceId();
  if (!sid || (roomId === activeRoomId() && view() === "room" && basePlace() === "space" && !settingsOpen())) return;
  lastRoomBySpace.set(sid, roomId);
  batch(() => {
    clearTransient();
    view.set("room");
    settingsOpen.set(false);
    basePlace.set("space");
    activeRoomId.set(roomId);
    actionError.set(null);
    notice.set(null);
  });
  kick();
}

export async function createRoom(nameInput: string): Promise<boolean> {
  const name = nameInput.trim();
  const c = client;
  const sid = activeSpaceId();
  if (!c || !sid || creatingRoom()) return false;
  if (!name) {
    actionError.set("Room name can't be empty.");
    return false;
  }
  const ok = guard(sid);
  batch(() => {
    creatingRoom.set(true);
    actionError.set(null);
  });
  try {
    const room = await c.createRoom(sid, name);
    if (!ok()) return false;
    const list = roomsBySpace()[sid] ?? EMPTY_ROOMS;
    batch(() => {
      if (!list.some((r) => r.id === room.id)) roomsBySpace.set({ ...roomsBySpace(), [sid]: [...list, room] });
    });
    if (activeSpaceId() === sid) selectRoom(room.id);
    lastListsRefresh = 0; // pick up rooms others created, in server order
    kick();
    return true;
  } catch (error) {
    if (ok()) handleActionError(error, "Could not create the room", sid);
    return false;
  } finally {
    if (ok()) creatingRoom.set(false);
  }
}

// ------------------------------------------------------------------ attachments (hook)

/**
 * Where a message goes: the personal Chat (files to the sender's personal
 * library) or a Space's room or thread (files to that Space's library).
 */
export type SendDestination = { kind: "chat" } | { kind: "room"; spaceId: string; roomId: string };

/**
 * Files staged in the composer, uploaded at Send. Registered by
 * src/features/attachments so this module needs no library code. `upload`
 * puts every staged file of `scope` in the destination's library (reusing
 * ids a failed attempt already got) and resolves to their ids, or to the
 * message to show when one failed (nothing is posted then, the draft and
 * files stay). `sent` drops the files a message carried.
 */
export type AttachmentHook = {
  pending(scope: string): boolean;
  upload(scope: string, to: SendDestination, ok: () => boolean): Promise<{ ids: string[]; keys: string[] } | { error: string }>;
  sent(scope: string, keys: string[], to: SendDestination): void;
};
let attachments: AttachmentHook | null = null;
export function registerAttachments(hook: AttachmentHook) {
  attachments = hook;
}
/** The active place's key for staged files ("chat", or a room's); "" when none is open. */
export function composerScope(): string {
  return activeScope();
}

/** Asking the agent needs a typed question: files alone are saved to the Library, not sent to a model. */
export const ASK_NEEDS_TEXT = "Type a question to ask the agent. Attached files are saved to the Library; they aren't sent to the agent.";

/**
 * Posts the draft (and any staged files) to the active place; `invokeAgent`
 * asks the agent to answer. Files are uploaded to the destination's library
 * first; if one fails nothing is posted and the draft and files stay. A
 * message may be files only, unless it asks the agent.
 */
export async function send(invokeAgent: boolean, choice?: TurnModelChoice, agentId?: string): Promise<void> {
  const chat = basePlace() === "chat";
  const text = draft().trim();
  const sid = activeSpaceId();
  const rid = activeRoomId();
  const c = client;
  const scope = activeScope();
  if (!c || sending() || !scope || (!chat && (!sid || !rid))) return;
  const files = !!attachments?.pending(scope);
  if (!text && !files) return;
  if (invokeAgent && !text) {
    batch(() => {
      actionError.set(ASK_NEEDS_TEXT);
      notice.set(null);
    });
    return;
  }
  const to: SendDestination = chat ? { kind: "chat" } : { kind: "room", spaceId: sid!, roomId: rid! };
  const sameSession = guard();
  const ok = chat ? sameSession : guard(sid!);
  // Feedback only where it happened: never in a place the user moved to.
  const here = () => ok() && activeScope() === scope;
  batch(() => {
    sending.set(invokeAgent ? "agent" : "message");
    actionError.set(null);
    notice.set(null);
  });
  const sentMentions = chat
    ? []
    : mentions().filter((m) => {
        const literal = mentionLiteral(m);
        return !literal || text.includes(literal);
      });
  try {
    let ids: string[] = [];
    let keys: string[] = [];
    if (files && attachments) {
      const up = await attachments.upload(scope, to, ok);
      if (!ok()) return;
      if ("error" in up) {
        if (here()) actionError.set(up.error);
        return;
      }
      ids = up.ids;
      keys = up.keys;
    }
    if (chat) await c.sendChatMessage(text, invokeAgent, ids, choice);
    else await c.sendMessage(sid!, rid!, text, invokeAgent, sentMentions, ids, choice, invokeAgent ? agentId : undefined);
    if (!ok()) return;
    if (keys.length) attachments?.sent(scope, keys, to);
    if (activeScope() === scope && draft().trim() === text) {
      // unless the user typed something new meanwhile
      batch(() => {
        draft.set("");
        if (!chat) mentions.set([]);
      });
    } else if (stashedDrafts()[scope]?.text.trim() === text) {
      // Sent from a place the user has since left: its stashed draft is gone too.
      const { [scope]: _sent, ...rest } = stashedDrafts();
      stashedDrafts.set(rest);
    }
    if (chat) {
      if (chatUnavailable()) chatUnavailable.set(null);
      lastRunsRefresh = 0;
    }
    kick();
  } catch (error) {
    if (!here()) return;
    handleActionError(error, invokeAgent ? (chat ? "Could not ask your agent" : "Could not ask the agent") : "Could not send the message", chat ? undefined : sid!);
    const shown = actionError();
    if (files && shown) actionError.set(`${shown} Its files are already in the Library; Send again won't upload them twice.`);
  } finally {
    if (sameSession()) sending.set(null);
  }
}

// ------------------------------------------------------------------ runs and dispatch (Chat meta controls)

/** Opens the room or thread a run works in. */
export async function openRun(run: Run): Promise<void> {
  const space = spaces().find((s) => s.id === run.spaceId);
  if (!space) {
    runsError.set("That run belongs to a Space you're no longer a member of.");
    return;
  }
  lastRoomBySpace.set(run.spaceId, run.roomId);
  await selectSpace(run.spaceId);
  if (activeSpaceId() !== run.spaceId) return;
  if (!rooms().some((r) => r.id === run.roomId)) {
    try {
      await refreshLists(true); // a thread created moments ago
    } catch {
      /* the loop reports connection problems */
    }
  }
  if (rooms().some((r) => r.id === run.roomId)) selectRoom(run.roomId);
  else notice.set("That room or thread isn't visible to you (yet).");
}

/** Targets the dispatch composer at a run's room, for a follow-up. */
export async function followUp(run: Run): Promise<void> {
  await setDispatchSpace(run.spaceId, run.roomId);
}

export async function setDispatchSpace(spaceId: string, roomId?: string): Promise<void> {
  if (!spaces().some((s) => s.id === spaceId)) return;
  dispatchError.set(null);
  dispatchNotice.set(null);
  dispatchTarget.set({ spaceId, roomId: roomId ?? null });
  try {
    const list = await ensureRooms(spaceId);
    const current = dispatchTarget();
    if (current?.spaceId !== spaceId) return;
    const chosen = roomId && list.some((r) => r.id === roomId) ? roomId : pickRoom(spaceId, list);
    dispatchTarget.set({ spaceId, roomId: chosen });
  } catch (error) {
    if (dispatchTarget()?.spaceId === spaceId) dispatchError.set(describe(error, "Could not load that Space's rooms"));
  }
}

export function setDispatchRoom(roomId: string) {
  const t = dispatchTarget();
  if (t) dispatchTarget.set({ spaceId: t.spaceId, roomId });
}

/** Sends the dispatch draft to the chosen Space and room and asks that Space's agent. */
export async function sendDispatch(): Promise<void> {
  const text = dispatchDraft().trim();
  const target = dispatchTarget();
  const c = client;
  if (!text || !target?.roomId || !c || dispatching()) return;
  const space = spaces().find((s) => s.id === target.spaceId);
  const room = roomsBySpace()[target.spaceId]?.find((r) => r.id === target.roomId);
  if (!space || !room) {
    dispatchError.set("Choose a Space and room you belong to first.");
    return;
  }
  const ok = guard(target.spaceId);
  batch(() => {
    dispatching.set(true);
    dispatchError.set(null);
    dispatchNotice.set(null);
  });
  try {
    await c.sendMessage(target.spaceId, target.roomId, text, true);
    if (!ok()) return;
    if (dispatchDraft().trim() === text) dispatchDraft.set("");
    dispatchNotice.set(`Sent to the agent in ${space.name} › #${room.name}.`);
    lastRunsRefresh = 0;
    kick();
  } catch (error) {
    if (!ok()) return;
    if (error instanceof ApiError && error.kind === "auth") disconnect(error.message);
    else {
      dispatchError.set(describe(error, "Could not send the task"));
      if (error instanceof ApiError && (error.kind === "forbidden" || error.kind === "not_found")) void lostAccess(target.spaceId, null);
    }
  } finally {
    if (ok()) dispatching.set(false);
  }
}

export async function stopRun(run: Run): Promise<void> {
  const c = client;
  if (!c || runBusy()) return;
  const ok = guard();
  batch(() => {
    runBusy.set(run.id);
    runsError.set(null);
  });
  try {
    await c.stopRun(run.id);
    if (!ok()) return;
    lastRunsRefresh = 0;
    kick();
  } catch (error) {
    if (!ok()) return;
    if (error instanceof ApiError && error.kind === "auth") disconnect(error.message);
    else runsError.set(describe(error, "Could not stop the run"));
  } finally {
    if (ok()) runBusy.set(null);
  }
}

/** Whether a run is still going (so Stop makes sense). */
export function isActiveRun(run: Run) {
  return /^(queued|pending|running|active|in_progress|started|waiting)$/i.test(run.status);
}

/** Display names for a run's scope, from what is cached. */
export function runScope(run: Run): { space: string; room: string } {
  const space = spaces().find((s) => s.id === run.spaceId)?.name ?? run.spaceId;
  const room = roomsBySpace()[run.spaceId]?.find((r) => r.id === run.roomId)?.name ?? run.roomId;
  return { space, room };
}

export function roomsOf(spaceId: string): readonly Room[] {
  return roomsBySpace()[spaceId] ?? EMPTY_ROOMS;
}

/** Whether people can react to an event: human and agent messages, not system, tool or runner rows. */
export function isReactable(event: RoomEvent): boolean {
  const k = event.kind.toLowerCase();
  if (k === "message") return true;
  return (k === "agent" || k === "agent_message" || k === "assistant") && !/(tool|runner|system|error|demo|reaction)/.test(k);
}

/** Adds or removes the signed-in user's `emoji` on an event of the active Chat or room. */
export async function toggleReaction(eventId: number, emoji: string): Promise<void> {
  const c = client;
  const myId = me();
  if (!c || !myId || reactionPending()) return;
  const inChat = basePlace() === "chat";
  const sid = activeSpaceId();
  const rid = activeRoomId();
  if (!inChat && (!sid || !rid)) return;
  const key = inChat ? CHAT : scopeKey(sid!, rid!);
  const mine = (reactionsByScope()[key]?.[eventId]?.[emoji] ?? []).includes(`user:${myId}`);
  const ok = inChat ? guard() : guard(sid!);
  reactionPending.set(`${eventId}|${emoji}`);
  actionError.set(null);
  try {
    await c.react(inChat ? { chat: true } : { spaceId: sid!, roomId: rid! }, eventId, emoji, !mine);
    if (!ok()) return;
    kick(); // the server's reaction event arrives with the next poll
  } catch (error) {
    if (ok()) handleActionError(error, mine ? "Could not remove the reaction" : "Could not add the reaction", inChat ? undefined : sid!);
  } finally {
    if (ok()) reactionPending.set(null);
  }
}

async function refreshRuns() {
  const c = client;
  if (!c) return;
  const ok = guard();
  try {
    const list = await c.listRuns();
    if (!ok()) return;
    lastRunsRefresh = Date.now();
    batch(() => {
      runs.set(list);
      runsLoaded.set(true);
      runsError.set(null);
    });
    // Name the rooms of the runs shown (one request per Space, once).
    for (const spaceId of new Set(list.map((r) => r.spaceId))) {
      if (!roomsBySpace()[spaceId] && spaces().some((s) => s.id === spaceId)) void ensureRooms(spaceId).catch(() => {});
    }
  } catch (error) {
    if (!ok()) return;
    lastRunsRefresh = Date.now();
    if (error instanceof ApiError && !error.transient && error.kind !== "auth") {
      runsError.set(describe(error, "Could not load runs"));
      return;
    }
    throw error;
  }
}

/**
 * Test runner: asks the server to run the deterministic `workspace.list` on
 * this Space's runner (POST …/demo; no model is used). Its listing is posted
 * in the active room. True once the server took the request.
 */
export async function runDemo(): Promise<boolean> {
  const sid = activeSpaceId();
  const rid = activeRoomId();
  const c = client;
  if (!sid || !rid || !c || demoPending()) return false;
  const ok = guard(sid);
  batch(() => {
    demoPending.set(true);
    actionError.set(null);
    notice.set(null);
  });
  try {
    await c.runDemo(sid, rid);
    if (!ok()) return false;
    kick();
    return true;
  } catch (error) {
    if (ok()) handleActionError(error, "Could not test the runner", sid);
    return false;
  } finally {
    if (ok()) demoPending.set(false);
  }
}

// ------------------------------------------------------------------ management (owners)

export async function openManage(): Promise<void> {
  const space = activeSpace();
  if (!space || space.role !== "owner") return;
  batch(() => {
    clearTransient();
    view.set("manage");
    manageError.set(null);
    notice.set(null);
  });
  for (const fn of manageOpeners) fn(space.id);
  await reloadMembers();
}

export function closeManage() {
  batch(() => {
    clearTransient();
    view.set("room");
    manageError.set(null);
    notice.set(null);
  });
}

export async function reloadMembers(): Promise<void> {
  const sid = activeSpaceId();
  const c = client;
  if (!sid || !c) return;
  const ok = guard(sid);
  try {
    const list = await c.listMembers(sid);
    if (ok()) membersBySpace.set({ ...membersBySpace(), [sid]: list });
  } catch (error) {
    if (ok()) handleManageError(error, "Could not load members", sid);
  }
}

/** Loads the active Space's members for the mention picker (any member may). */
export async function loadMembersForMentions(): Promise<void> {
  const sid = activeSpaceId();
  const c = client;
  if (!sid || !c) return;
  const ok = guard(sid);
  membersError.set(null);
  try {
    const list = await c.listMembers(sid);
    if (ok()) membersBySpace.set({ ...membersBySpace(), [sid]: list });
  } catch (error) {
    if (!ok() || isAborted(error)) return;
    if (error instanceof ApiError && error.kind === "auth") disconnect(error.message);
    else if (activeSpaceId() === sid) membersError.set(describe(error, "Could not load members"));
  }
}

// ------------------------------------------------------------------ mentions

export function mentionLiteral(m: Mention): string | null {
  // A named agent by its @handle; the generic @agent (the default agent, or the classifier's pick) otherwise.
  if (m.kind === "agent") return m.agentId ? (m.handle && /^[a-z0-9_-]+$/.test(m.handle) ? `@${m.handle}` : null) : "@agent";
  return /^\S+$/.test(m.id) ? `@${m.id}` : null; // ids with spaces go only in the explicit list
}

export function sameMention(a: Mention, b: Mention) {
  if (a.kind === "agent" && b.kind === "agent") return (a.agentId ?? null) === (b.agentId ?? null);
  return a.kind === "user" && b.kind === "user" && a.id === b.id;
}

function escapeRegExp(text: string) {
  return text.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
}

function literalIn(text: string, literal: string) {
  return new RegExp(`(^|\\s)${escapeRegExp(literal)}(?=$|[\\s.,;:!?)])`).test(text);
}

/** The "@partial" being typed at the end of the draft, if any. */
export function mentionQuery(text: string): string | null {
  const m = /(^|\s)@([^\s@]*)$/.exec(text);
  return m ? m[2] : null;
}

/** Adds a mention: its literal replaces a trailing "@partial" or is appended. */
export function addMention(m: Mention) {
  if (basePlace() !== "space") return;
  const literal = mentionLiteral(m);
  batch(() => {
    if (!mentions().some((x) => sameMention(x, m))) mentions.set([...mentions(), m]);
    if (!literal) return;
    const text = draft();
    if (mentionQuery(text) !== null) draft.set(text.replace(/@[^\s@]*$/, `${literal} `));
    else if (!literalIn(text, literal)) draft.set(`${text}${text && !/\s$/.test(text) ? " " : ""}${literal} `);
  });
}

/** Removes a mention chip and its text from the draft. */
export function removeMention(m: Mention) {
  const literal = mentionLiteral(m);
  batch(() => {
    mentions.set(mentions().filter((x) => !sameMention(x, m)));
    if (literal) {
      const re = new RegExp(`(^|\\s)${escapeRegExp(literal)}(?=$|[\\s.,;:!?)])`, "g");
      draft.set(draft().replace(re, "$1").replace(/[ \t]{2,}/g, " ").trimStart());
    }
  });
}

/** Sets the draft and drops mention chips whose text the user deleted. */
export function setDraft(text: string) {
  batch(() => {
    draft.set(text);
    const kept = mentions().filter((m) => {
      const literal = mentionLiteral(m);
      return !literal || literalIn(text, literal);
    });
    if (kept.length !== mentions().length) mentions.set(kept);
  });
}

export async function addMember(userIdInput: string): Promise<boolean> {
  const userId = userIdInput.trim();
  const sid = activeSpaceId();
  const c = client;
  if (!sid || !c || memberBusy()) return false;
  if (!userId) {
    manageError.set("Enter the user ID of the person to add. They can copy it from their sidebar.");
    return false;
  }
  const ok = guard(sid);
  batch(() => {
    memberBusy.set(userId);
    manageError.set(null);
  });
  try {
    await c.addMember(sid, userId);
    if (!ok()) return false;
    await reloadMembers();
    return true;
  } catch (error) {
    if (ok()) handleManageError(error, `Could not add ${userId}`, sid);
    return false;
  } finally {
    if (ok()) memberBusy.set(null);
  }
}

export async function removeMember(userId: string): Promise<void> {
  const sid = activeSpaceId();
  const c = client;
  if (!sid || !c || memberBusy()) return;
  const ok = guard(sid);
  batch(() => {
    memberBusy.set(userId);
    manageError.set(null);
  });
  try {
    await c.removeMember(sid, userId);
    if (!ok()) return;
    await reloadMembers();
  } catch (error) {
    if (ok()) handleManageError(error, `Could not remove ${userId}`, sid);
  } finally {
    if (ok()) memberBusy.set(null);
  }
}

/** Marks whether anyone serves a Space (a dedicated token or an attached runner). */
export function setRunnerPaired(spaceId: string, paired: boolean) {
  spaces.set(spaces().map((s) => (s.id === spaceId ? { ...s, runnerPaired: paired } : s)));
}

function clearTransient() {
  focusTarget.set(null);
  for (const fn of transientClearers) fn();
}

// Drafts are per place: each room or thread, and the personal Chat, keeps its
// own text and mention chips. `draft` and `mentions` are the active place's;
// switching stashes them under the place left and restores the new one's.
const stashedDrafts = signal<Record<string, { text: string; mentions: Mention[] }>>({});
/** Some place other than the active one has a draft not sent yet. */
export const stashedDraftPending = computed(() => Object.values(stashedDrafts()).some((d) => d.text.trim().length > 0));
let mentionScope = "";
effect(() => {
  const scope = activeScope();
  untracked(() => {
    if (scope === mentionScope) return;
    const left = mentionScope;
    mentionScope = scope;
    const stash = { ...stashedDrafts() };
    if (left && (draft() || mentions().length)) stash[left] = { text: draft(), mentions: mentions() };
    else delete stash[left];
    const back = stash[scope];
    delete stash[scope];
    batch(() => {
      stashedDrafts.set(stash);
      draft.set(back?.text ?? "");
      mentions.set(back?.mentions ?? []);
      if (membersError()) membersError.set(null);
    });
  });
});

// ------------------------------------------------------------------ loop

function endSession() {
  session++;
  abort?.abort();
  abort = null;
  client = null;
  if (timer !== undefined) clearTimeout(timer);
  timer = undefined;
  ticking = false;
  tickAgain = false;
  failures = 0;
  lastListsRefresh = 0;
  lastRunsRefresh = 0;
  cursors.clear();
  inflight.clear();
  pollAgain.clear();
  lastRoomBySpace.clear();
  spaceEpoch.clear();
  batch(() => {
    status.set({ phase: "signedOut" });
    connectedTo.set(null);
    me.set(null);
    settingsOpen.set(false);
    basePlace.set("chat");
    focusTarget.set(null);
    chatUnavailable.set(null);
    runs.set([]);
    runsLoaded.set(false);
    runsError.set(null);
    runBusy.set(null);
    dispatchTarget.set(null);
    dispatchDraft.set("");
    dispatching.set(false);
    dispatchError.set(null);
    dispatchNotice.set(null);
    spaces.set([]);
    activeSpaceId.set(null);
    switchingSpace.set(false);
    roomsBySpace.set({});
    activeRoomId.set(null);
    eventsByScope.set({});
    loadedScopes.set({});
    reactionsByScope.set({});
    reactionPending.set(null);
    view.set("room");
    draft.set("");
    stashedDrafts.set({});
    sending.set(null);
    demoPending.set(false);
    creatingRoom.set(false);
    creatingSpace.set(false);
    actionError.set(null);
    notice.set(null);
    membersBySpace.set({});
    membersError.set(null);
    mentions.set([]);
    manageError.set(null);
    memberBusy.set(null);
    orgAdmin.set(false);
    settingsSection.set("runners");
    for (const fn of sessionEnders) fn();
  });
}

/** Runs a tick now (or right after the running one). */
function kick() {
  if (!client) return;
  if (ticking) {
    tickAgain = true;
    return;
  }
  schedule(session, 0);
}

function schedule(sess: number, delay: number) {
  if (timer !== undefined) clearTimeout(timer);
  timer = setTimeout(() => {
    timer = undefined;
    void tick(sess);
  }, delay);
}

async function tick(sess: number) {
  if (sess !== session || !client) return;
  ticking = true;
  tickAgain = false;
  let delay = POLL_MS;
  try {
    if (Date.now() - lastListsRefresh >= LISTS_REFRESH_MS) await refreshLists();
    if (sess !== session) return;
    if (basePlace() === "chat") {
      if (!chatUnavailable()) await pollChat();
      if (sess === session && Date.now() - lastRunsRefresh >= RUNS_REFRESH_MS) await refreshRuns();
    } else if (basePlace() === "space") {
      const sid = activeSpaceId();
      const rid = activeRoomId();
      if (sid && rid && view() === "room") await pollRoom(sid, rid);
    }
    if (sess !== session) return;
    if (failures > 0 || status().phase !== "online") status.set({ phase: "online" });
    failures = 0;
  } catch (error) {
    if (sess !== session || isAborted(error)) return;
    const next = await handleLoopError(error);
    if (next === null || sess !== session) return;
    delay = next;
  } finally {
    if (sess === session) {
      ticking = false;
      schedule(sess, tickAgain ? 0 : delay);
    }
  }
}

/** Returns the delay before the next tick, or null when the session ended. */
async function handleLoopError(error: unknown): Promise<number | null> {
  if (error instanceof ApiError && error.kind === "auth") {
    disconnect(error.message);
    return null;
  }
  if (error instanceof ApiError && (error.kind === "forbidden" || error.kind === "not_found") && basePlace() === "space") {
    await lostAccess(activeSpaceId(), activeRoomId());
    return 0;
  }
  failures++;
  const retryInMs = Math.min(MAX_BACKOFF_MS, 1000 * 2 ** (failures - 1));
  status.set({ phase: "retrying", attempt: failures, retryInMs, message: describe(error, "Connection problem") });
  return retryInMs;
}

/**
 * A Space endpoint said 403/404. Reload the Space list to tell "removed from
 * the Space" (purge it, move on) from "this room is gone" (drop the room).
 */
async function lostAccess(spaceId: string | null, roomId: string | null) {
  if (!spaceId) return;
  const before = spaces().find((s) => s.id === spaceId);
  try {
    await refreshLists(true);
  } catch {
    /* the next tick deals with it */
  }
  if (spaces().some((s) => s.id === spaceId)) {
    if (!roomId) return;
    const key = scopeKey(spaceId, roomId);
    cursors.delete(key);
    batch(() => {
      roomsBySpace.set({ ...roomsBySpace(), [spaceId]: (roomsBySpace()[spaceId] ?? EMPTY_ROOMS).filter((r) => r.id !== roomId) });
      const { [key]: _e, ...events } = eventsByScope();
      eventsByScope.set(events);
      if (activeSpaceId() === spaceId && activeRoomId() === roomId) {
        activeRoomId.set(pickRoom(spaceId, roomsBySpace()[spaceId] ?? EMPTY_ROOMS));
        notice.set("That room is no longer available.");
      }
    });
  } else if (before) {
    notice.set(`You no longer have access to ${before.name}.`);
  }
}

/**
 * The user just deleted or left a Space (src/features/space): reload the
 * list, which forgets it and leaves it if it was open, then say what happened.
 */
export async function spaceGone(spaceId: string, message: string): Promise<void> {
  await lostAccess(spaceId, null);
  if (!spaces().some((s) => s.id === spaceId)) notice.set(message);
}

/** A Space's name changed (src/features/space): show it everywhere at once. */
export function spaceRenamed(spaceId: string, name: string) {
  spaces.set(spaces().map((s) => (s.id === spaceId ? { ...s, name } : s)));
}

/** Reloads the Space list and the active Space's rooms. */
async function refreshLists(force = false) {
  const c = client;
  if (!c) return;
  if (!force && Date.now() - lastListsRefresh < LISTS_REFRESH_MS) return;
  const ok = guard();
  const list = await c.listSpaces();
  if (!ok()) return;
  lastListsRefresh = Date.now();
  const known = new Set(list.map((s) => s.id));
  const lost = spaces().filter((s) => !known.has(s.id));
  for (const space of lost) purgeSpace(space.id, true);
  spaces.set(list);
  const active = activeSpaceId();
  if (active && !known.has(active)) {
    const lostSpace = lost.find((s) => s.id === active);
    const wasHere = basePlace() === "space";
    batch(() => {
      activeSpaceId.set(null);
      activeRoomId.set(null);
      view.set("room");
      if (lostSpace) notice.set(`You no longer have access to ${lostSpace.name}.`);
    });
    // Never fall back to another Space silently: go to the Chat, which says what happened.
    if (wasHere) basePlace.set("chat");
    return;
  }
  if (active) {
    const okSpace = guard(active);
    const rooms = await c.listRooms(active);
    if (!okSpace()) return;
    const previous = roomsBySpace()[active] ?? EMPTY_ROOMS;
    const current = activeRoomId();
    batch(() => {
      // Keep a room we just created even if the list is a moment behind; the
      // events poll will say 404 if it really is gone.
      const local = current ? previous.find((r) => r.id === current && !rooms.some((x) => x.id === r.id)) : undefined;
      roomsBySpace.set({ ...roomsBySpace(), [active]: local ? [...rooms, local] : rooms });
      if (activeSpaceId() === active && (!current || !roomsBySpace()[active].some((r) => r.id === current))) {
        activeRoomId.set(pickRoom(active, roomsBySpace()[active]));
      }
    });
  }
}

function pollRoom(spaceId: string, roomId: string): Promise<void> {
  return poll(scopeKey(spaceId, roomId), guard(spaceId), (c, after) => c.listEvents(spaceId, roomId, after));
}

async function pollChat(): Promise<void> {
  try {
    await poll(CHAT, guard(), (c, after) => c.listChatEvents(after));
  } catch (error) {
    if (error instanceof ApiError && (error.kind === "forbidden" || error.kind === "not_found" || error.kind === "protocol")) {
      chatUnavailable.set(`Your Chat isn't available on this server: ${error.message}`);
      return;
    }
    throw error;
  }
}

/** Fetches events after the scope's cursor; one request per scope at a time. */
function poll(key: string, ok: () => boolean, fetch: (c: Client, after: number) => Promise<RoomEvent[]>): Promise<void> {
  const running = inflight.get(key);
  if (running) {
    pollAgain.add(key);
    return running;
  }
  const run = (async () => {
    let pages = 0;
    for (;;) {
      pollAgain.delete(key);
      const c = client;
      if (!c || !ok()) return;
      const firstLoad = !loadedScopes()[key];
      const events = await fetch(c, cursors.get(key) ?? 0);
      if (!ok()) return;
      const added = merge(key, events);
      pages++;
      // Catch up a long history in a few quick pages on first load; otherwise
      // poll once more only if someone asked for it while we were busy.
      const more = firstLoad && added > 0 && pages < 20;
      if (!more && !pollAgain.has(key)) return;
    }
  })().finally(() => {
    if (inflight.get(key) === run) inflight.delete(key);
  });
  inflight.set(key, run);
  return run;
}

/**
 * Appends events newer than the scope's cursor and folds `reaction` events
 * into the reactions of their target (in id order, so replays are harmless).
 * Returns how many events were new.
 */
function merge(key: string, events: RoomEvent[]): number {
  const cursor = cursors.get(key) ?? 0;
  const seen = new Set<number>();
  const fresh = events
    .filter((e) => e.id > cursor && !seen.has(e.id) && (seen.add(e.id), true))
    .sort((a, b) => a.id - b.id);
  const shown: RoomEvent[] = [];
  let reactions: Reactions | null = null;
  for (const event of fresh) {
    if (event.kind !== "reaction") {
      shown.push(event);
      // A delegated thread was created or reported: pick up the room list now.
      if (event.kind === "thread") lastListsRefresh = 0;
      continue;
    }
    const change = parseReaction(event);
    if (!change) continue; // a malformed reaction is never shown as a line either
    reactions ??= { ...(reactionsByScope()[key] ?? EMPTY_REACTIONS) };
    const reactor = `${change.reactorKind}:${change.reactorId}`;
    const byEmoji = { ...(reactions[change.targetEventId] ?? {}) };
    const list = byEmoji[change.emoji] ?? [];
    const has = list.includes(reactor);
    if (change.present && !has) byEmoji[change.emoji] = [...list, reactor];
    else if (!change.present && has) {
      const rest = list.filter((r) => r !== reactor);
      if (rest.length) byEmoji[change.emoji] = rest;
      else delete byEmoji[change.emoji];
    }
    reactions[change.targetEventId] = byEmoji;
  }
  batch(() => {
    if (!loadedScopes()[key]) loadedScopes.set({ ...loadedScopes(), [key]: true });
    if (fresh.length === 0) return;
    cursors.set(key, fresh[fresh.length - 1].id);
    if (reactions) reactionsByScope.set({ ...reactionsByScope(), [key]: reactions });
    if (shown.length === 0) return;
    const current = eventsByScope()[key] ?? EMPTY_EVENTS;
    const next = current.concat(shown);
    eventsByScope.set({
      ...eventsByScope(),
      [key]: next.length > MAX_EVENTS_PER_ROOM ? next.slice(next.length - MAX_EVENTS_PER_ROOM) : next,
    });
  });
  return fresh.length;
}

// ------------------------------------------------------------------ errors

function handleActionError(error: unknown, what: string, spaceId?: string) {
  if (isAborted(error)) return;
  if (error instanceof ApiError && error.kind === "auth") {
    disconnect(error.message);
    return;
  }
  actionError.set(describe(error, what));
  if (spaceId && error instanceof ApiError && (error.kind === "forbidden" || error.kind === "not_found")) {
    void lostAccess(spaceId, activeSpaceId() === spaceId ? activeRoomId() : null);
  } else if (error instanceof ApiError && error.transient) {
    kick();
  }
}

export function handleManageError(error: unknown, what: string, spaceId: string) {
  if (isAborted(error)) return;
  if (error instanceof ApiError && error.kind === "auth") {
    disconnect(error.message);
    return;
  }
  manageError.set(describe(error, what));
  if (error instanceof ApiError && (error.kind === "forbidden" || error.kind === "not_found")) {
    void lostAccess(spaceId, null).then(() => {
      // Ownership may have changed: leave the management view if no longer the owner.
      if (activeSpace()?.role !== "owner" && view() === "manage") closeManage();
    });
  }
}

export function isAborted(error: unknown) {
  return error instanceof ApiError && error.kind === "aborted";
}

export function describe(error: unknown, what: string): string {
  if (error instanceof ApiError) return error.message;
  return `${what}: ${String((error as Error)?.message ?? error)}`;
}
