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
import { ApiError, createClient, isCanonicalMessageRow, parseReaction } from "./api/client";
import type { Client, Me, Member, Mention, Room, RoomEvent, Run, Space, SpaceMessageBody, TurnModelChoice } from "./api/client";
export { ApiError };
import { DEFAULT_SERVER_URL, parseServerUrl, serverHost } from "./api/serverUrl";
import { plainHttpAccepted } from "./plainHttp";
import {
  captureTypedMentions, editMentionDraft, mentionIdentityKey, pickMention, prepareMentionSend, scanMentionDraft, updateMentionDraft,
} from "./features/mentions/draft";
import type { MentionDirectory, MentionDraft, MentionRecipient, MentionSelection, MentionSend } from "./features/mentions/draft";
import { confirmChangedAutomaticMentions } from "./features/mentions/preflight";
import type { PostedSenderNotes, SenderNoticeDetails } from "./features/senderNotices/response";
import { renderedRecipientsKey } from "./features/senderNotices/response";

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
export type Place = "chat" | "space" | "settings" | "search" | "library" | "pages" | "schedules";
/** Where the main panel is under any modal: never Settings. */
export type MainPlace = Exclude<Place, "settings">;
/** The pages of Settings. Reachable with or without any Space. */
export type SettingsExtension = "websearch" | "about";
export type SettingsSection = "profile" | "runners" | "connections" | "models" | "monitoring" | "appearance" | SettingsExtension;
type SettingsAlias = "providers" | "routing";
const settingsExtensions = signal<readonly SettingsExtension[]>([]);
export const settingsExtensionAvailable = (section: SettingsExtension) => settingsExtensions().includes(section);
/** Called by the Settings shell when a real extension page is installed. */
export function enableSettingsExtension(section: SettingsExtension) {
  if (!settingsExtensionAvailable(section)) settingsExtensions.set([...settingsExtensions(), section]);
}

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
export const settingsSection = signal<SettingsSection>("profile");
export const settingsAnchor = signal<"privacy" | null>(null);
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
/** Why personal Chat is unavailable, until a newer check can read or post to it. */
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
const memberFailures = signal<Record<string, string | null>>({});
export const membersError = computed(() => memberFailures()[activeSpaceId() ?? ""] ?? null);
const memberRequests = new Map<string, Promise<void>>();
const memberVersions = new Map<string, number>();
const memberEvidence = signal<Record<string, { version: number; roster: object }>>({});
type MentionVisit = { sid: string; scope: string; navigation: number; token: object; verified: boolean };
const mentionVisit = signal<MentionVisit | null>(null);
type MemberAlias = { order: number; displayName: string | null };
const aliasVisit = computed(() => {
  const visit = mentionVisit();
  return visit?.verified && currentMentionVisit(visit) ? visit.token : null;
});
const aliasRoster = computed(() => {
  const sid = activeSpaceId();
  return sid && !memberFailures()[sid] && members()?.some(m => m.userId === me()) ? memberEvidence()[sid]?.roster : null;
});
// Only the current verified visit and roster can share name evidence. Identical
// roster replies preserve the scope; cached profiles from elsewhere cannot seed it.
const memberAliasScope = computed(() => aliasVisit() && aliasRoster()
  ? { entries: signal<ReadonlyMap<string, MemberAlias>>(new Map()), next: 0 } : null);

const EMPTY_DIRECTORY: MentionDirectory = { members: [], agents: [] };
const EMPTY_MENTION_DRAFT: MentionDraft = { text: "", anchors: [] };
export const mentionDraft = signal<MentionDraft>(EMPTY_MENTION_DRAFT);
const editDirectory = signal<MentionDirectory | null>(null);
type MentionConfirmation = { directory: MentionDirectory; current: () => boolean };
const mentionConfirmation = signal<MentionConfirmation | null>(null);
export const mentions = computed(() =>
  [...new Map(mentionDraft().anchors.map(a => [mentionIdentityKey(a.mention), a])).values()]);

// The composer supplies the locally verified names and handles; the store
// independently verifies stable recipients before the POST.
let directoryForMentions: (sid: string) => MentionDirectory | null = () => null;
let verifiedMentionAgents: (sid: string) => MentionDirectory["agents"] | null = () => null;
let refreshAgentsForMentions: ((sid: string, live: () => boolean) => Promise<MentionDirectory["agents"] | null>) | undefined;
export function registerMentionDirectory(
  get: (sid: string) => MentionDirectory | null,
  refreshAgents: NonNullable<typeof refreshAgentsForMentions>,
  getAgents: typeof verifiedMentionAgents,
) {
  directoryForMentions = get;
  refreshAgentsForMentions = refreshAgents;
  verifiedMentionAgents = getAgents;
}

// Space management (owners)
export const manageError = signal<string | null>(null);
export const memberBusy = signal<string | null>(null);

// ------------------------------------------------------------------ session internals

let client: Client | null = null;
let noticeSenderId: string | null = null; // canonical raw /me id, not a legacy coercion
let abort: AbortController | null = null;
let session = 0;
let navigationGeneration = 0;
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

// Access evidence is ordered by request start, not response arrival. A room
// success proves both the room and its Space; a sibling proves only the Space.
let accessSequence = 0;
const verifiedAccess = new Map<string, number>();
// Keep Space denials past purgeSpace: an older positive list may still be
// in flight. A newer authoritative list can grant access again.
const deniedSpaceAccess = new Map<string, number>();
// Room losses likewise outlive cache eviction and rejoin: even after a
// fresh positive, a read started before the loss must not select that room.
const deniedRoomAccess = new Map<string, number>();
const accessProbes = new Map<string, { request: number }>();
let chatAvailabilityRequest = 0;
let chatProbeAfter = 0;
const nextAccessRequest = () => ++accessSequence;
const newerAccess = (key: string, request: number) => (verifiedAccess.get(key) ?? 0) > request;
const roomDeniedAtOrAfter = (key: string, request: number) => (deniedRoomAccess.get(key) ?? 0) >= request;
function verifyAccess(key: string, request: number) {
  if (!newerAccess(key, request)) verifiedAccess.set(key, request);
}
function verifyRoomAccess(sid: string, rid: string, request: number) {
  verifyAccess(scopeKey(sid, ""), request);
  const key = scopeKey(sid, rid);
  if (!roomDeniedAtOrAfter(key, request)) verifyAccess(key, request);
}
/** Positive rows usable by this request, not rows verified by another read. */
function verifyRoomList(sid: string, list: readonly Room[], request: number): Room[] {
  verifyAccess(scopeKey(sid, ""), request);
  return list.filter(room => {
    if (roomDeniedAtOrAfter(scopeKey(sid, room.id), request)) return false;
    verifyRoomAccess(sid, room.id, request);
    return true;
  });
}

function keepNewerRooms(sid: string, list: readonly Room[], request: number): Room[] {
  const listed = new Set(list.map(r => r.id));
  return [...list, ...roomsOf(sid).filter(r => !listed.has(r.id) && newerAccess(scopeKey(sid, r.id), request))];
}
const deniedAtOrAfter = (sid: string, request: number) => (deniedSpaceAccess.get(sid) ?? 0) >= request;
function recordSpaceDenial(sid: string, request: number) {
  if (deniedAtOrAfter(sid, request) || newerAccess(scopeKey(sid, ""), request)) return false;
  deniedSpaceAccess.set(sid, request);
  return true;
}

// A successful but older POST may retire its draft, not undo a newer denial.
function observeChatAvailability(request: number, problem: string | null) {
  if (request < chatAvailabilityRequest) return;
  chatAvailabilityRequest = request;
  chatProbeAfter = problem ? Date.now() + LISTS_REFRESH_MS : 0;
  batch(() => {
    if (problem) forgetSenderNotices(CHAT);
    chatUnavailable.set(problem);
  });
}

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
  // Plain HTTP to a host that is neither loopback nor Tailscale: only once the
  // person confirmed it for this exact address (the screens ask first).
  if (parsed.needsConsent && !untracked(() => plainHttpAccepted(parsed.baseUrl))) {
    throw new Error(`Confirm the unencrypted connection to ${serverHost(parsed.baseUrl)} first.`);
  }
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
  noticeSenderId = who.canonicalId;
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
    const initial = candidate.list.find((s) => s.id === candidate.remembered);
    if (initial) {
      // Show the shell now; kick the first tick after enterSpace settles,
      // bounded by listRooms' client timeout.
      const current = guard();
      const entering = enterSpace(initial.id, { place: "chat", spaceId: null });
      const navigation = navigationGeneration; // enterSpace starts navigation before its first await
      void entering.then(() => {
        // A running tick reads the active room after any refreshLists await.
        if (!current() || ticking) return;
        const stillTarget = navigation === navigationGeneration &&
          basePlace() === "space" && activeSpaceId() === initial.id && activeRoomId() !== null && view() === "room";
        // Don't wake a later navigation, even if it returned to this Space.
        // Still start the loop if none is scheduled (e.g. after falling back to Chat).
        if (stillTarget || timer === undefined) kick();
      });
    } else {
      kick();
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
    kick();
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
  navigationGeneration++;
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
export function openPlace(where: "search" | "library" | "pages" | "schedules") {
  if (basePlace() === where && !settingsOpen()) return;
  navigationGeneration++;
  batch(() => {
    clearTransient();
    view.set("room");
    settingsOpen.set(false);
    basePlace.set(where);
    actionError.set(null);
    notice.set(null);
  });
}

/** Pages links must land on the exact room or stay where they are. Validate
 * with a fresh membership-checked list before changing navigation. */
export async function openExactRoom(spaceId: string, roomId: string, stillHere: () => boolean): Promise<boolean> {
  const c = client;
  if (!c || switchingSpace() || !roomId || !spaces().some(s => s.id === spaceId)) return false;
  const valid = guard(spaceId);
  const generation = navigationGeneration;
  const request = nextAccessRequest();
  const result = await c.listRooms(spaceId);
  if (!valid() || switchingSpace() || generation !== navigationGeneration || !stillHere()) return false;
  const list = verifyRoomList(spaceId, result, request);
  if (!list.some(r => r.id === roomId)) return false;
  navigationGeneration++;
  lastRoomBySpace.set(spaceId, roomId);
  batch(() => {
    clearTransient();
    roomsBySpace.set({ ...roomsBySpace(), [spaceId]: keepNewerRooms(spaceId, list, request) });
    activeSpaceId.set(spaceId);
    activeRoomId.set(roomId);
    view.set("room");
    settingsOpen.set(false);
    basePlace.set("space");
    actionError.set(null);
    notice.set(null);
  });
  rememberSpace(spaceId);
  kick();
  return true;
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
export function openSettings(section: SettingsSection | SettingsAlias | "about-privacy" = settingsSection()) {
  const page = section === "providers" ? "connections" : section === "routing" ? "models"
    : section === "about-privacy" ? "about" : section;
  if ((page === "about" || page === "websearch") && !settingsExtensionAvailable(page)) return;
  batch(() => {
    settingsAnchor.set(section === "about-privacy" ? "privacy" : null);
    if (settingsOpen() && settingsSection() === page) return;
    clearTransient();
    settingsSection.set(page);
    settingsOpen.set(true);
  });
}

/** Closes the Settings modal and returns to what was underneath, as it was. */
export function closeSettings() {
  if (!settingsOpen()) return;
  batch(() => {
    clearTransient();
    settingsOpen.set(false);
    settingsAnchor.set(null);
  });
}

export async function selectSpace(spaceId: string): Promise<void> {
  if (switchingSpace()) return;
  if (spaceId === activeSpaceId() && roomsBySpace()[spaceId]) {
    if (basePlace() === "space" && !settingsOpen()) return;
    navigationGeneration++;
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

type CreateSpaceOptions = {
  formIsCurrent: () => boolean;
  attachRunner: (space: Space, scope: {
    mayAttach: () => boolean;
    mayShowOutcome: () => boolean;
  }) => Promise<void>;
};

// Also catches a modal opened and closed while creating or checking the runner.
let createSpaceContext = 0;
onTransientClear(() => { createSpaceContext++; });

export async function createSpace(nameInput: string, options?: CreateSpaceOptions): Promise<boolean> {
  const name = nameInput.trim();
  const c = client;
  if (!c || creatingSpace()) return false;
  if (!name) {
    actionError.set("Space name can't be empty.");
    return false;
  }
  const ok = guard();
  const source = createSpaceContext;
  let created = false;
  batch(() => {
    creatingSpace.set(true);
    actionError.set(null);
  });
  try {
    const request = nextAccessRequest();
    const space = await c.createSpace(name);
    if (!ok()) return false;
    verifyAccess(scopeKey(space.id, ""), request);
    created = true;
    // Onboarding can unmount as soon as we publish the first Space. Check
    // the submitting form before that expected transition.
    const fromOriginalForm = source === createSpaceContext && !!options?.formIsCurrent();
    const expectedContext = createSpaceContext + 1; // enterSpace clears transients once
    const spaceOK = guard(space.id);
    spaces.set([...spaces().filter((s) => s.id !== space.id), space]);
    await enterSpace(space.id, { place: basePlace(), spaceId: activeSpaceId() });
    kick();
    // Returning to the same Space can show a missing attachment, but cannot
    // revive consent invalidated by a move or dialog.
    const mayShowOutcome = () => spaceOK() && basePlace() === "space" && view() === "room" && activeSpaceId() === space.id;
    if (options && mayShowOutcome()) {
      await options.attachRunner(space, {
        mayAttach: () => fromOriginalForm && createSpaceContext === expectedContext && mayShowOutcome(),
        mayShowOutcome,
      });
    }
    return true;
  } catch (error) {
    // A follow-up failure must not turn a confirmed creation into a retry.
    if (created) return ok();
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
  navigationGeneration++;
  const currentSession = guard();
  const ok = guard(spaceId);
  let request: number | undefined;
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
      request = nextAccessRequest();
      const result = await c.listRooms(spaceId);
      if (!ok()) return;
      const list = keepNewerRooms(spaceId, verifyRoomList(spaceId, result, request), request);
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
    if (isAccessDenial(error)) {
      if (request === undefined || !recordSpaceDenial(spaceId, request)) {
        if (activeSpaceId() === spaceId && !activeRoomId()) activeRoomId.set(pickRoom(spaceId, roomsOf(spaceId)));
        return;
      }
      purgeSpace(spaceId, true);
    } else purgeSpace(spaceId, false); // keep it listed; just don't keep half-loaded state
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
  const request = nextAccessRequest();
  let list: Room[];
  try {
    list = await c.listRooms(spaceId);
  } catch (error) {
    if (ok() && isAccessDenial(error)) dropLostSpace(spaceId, request, true);
    throw error;
  }
  if (!ok()) return EMPTY_ROOMS;
  list = keepNewerRooms(spaceId, verifyRoomList(spaceId, list, request), request);
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
  for (const key of noticeEpochs.keys()) if (key.startsWith(prefix)) noticeEpochs.delete(key);
  for (const key of verifiedAccess.keys()) if (key.startsWith(prefix)) verifiedAccess.delete(key);
  for (const key of accessProbes.keys()) if (key.startsWith(prefix)) accessProbes.delete(key);
  for (const [key, receipt] of pendingSenderNotices) {
    if (receipt.destination.kind === "room" && receipt.destination.spaceId === spaceId) pendingSenderNotices.delete(key);
  }
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
    senderNotices.set(strip(senderNotices()));
    if (unlist) spaces.set(spaces().filter((s) => s.id !== spaceId));
    if (dispatchTarget()?.spaceId === spaceId) dispatchTarget.set(null);
    const { [spaceId]: _members, ...restMembers } = membersBySpace();
    membersBySpace.set(restMembers);
    const { [spaceId]: _error, ...restErrors } = memberFailures();
    memberFailures.set(restErrors);
    const { [spaceId]: _evidence, ...restEvidence } = memberEvidence();
    memberEvidence.set(restEvidence);
    memberRequests.delete(spaceId);
    memberVersions.delete(spaceId);
    if (activeSpaceId() === spaceId) {
      if (activeScope().startsWith(prefix)) writeMentionDraft(EMPTY_MENTION_DRAFT, null);
      clearTransient();
    }
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
  navigationGeneration++;
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
    const request = nextAccessRequest();
    const room = await c.createRoom(sid, name);
    if (!ok()) return false;
    if (roomDeniedAtOrAfter(scopeKey(sid, room.id), request)) return true;
    verifyRoomAccess(sid, room.id, request);
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

// Only the initiating POST writes here. Not an event cache: it is never
// merged into a transcript, persisted, or used for model/search history.
export type SenderNotice = SenderNoticeDetails & { destination: SendDestination };
type ConfirmedSenderNotice = { note: SenderNotice; row: RoomEvent };
type PendingSenderNotice = PostedSenderNotes & { destination: SendDestination };
const senderNotices = signal<Record<string, ReadonlyMap<number, ConfirmedSenderNotice | null>>>({});
const pendingSenderNotices = new Map<string, PendingSenderNotice>();
// FIFO across the session: only uncorroborated receipts hold a copy of the
// text (at most 16k code units each), and an idle room can't block a new one.
const MAX_PENDING_SENDER_NOTICES = 32;
const noticeEpochs = new Map<string, number>();
const noticeEpoch = (scope: string) => noticeEpochs.get(scope) ?? 0;
const destinationScope = (to: SendDestination) => to.kind === "chat" ? CHAT : scopeKey(to.spaceId, to.roomId);
const pendingKey = (scope: string, id: number) => JSON.stringify([scope, id]);

function noticeDestinationVisible(to: SendDestination) {
  return to.kind === "chat" ? !chatUnavailable() :
    spaces().some(s => s.id === to.spaceId) && !!roomsBySpace()[to.spaceId]?.some(r => r.id === to.roomId);
}

function forgetSenderNotices(scope: string) {
  noticeEpochs.set(scope, noticeEpoch(scope) + 1);
  for (const [key, receipt] of pendingSenderNotices) {
    if (destinationScope(receipt.destination) === scope) pendingSenderNotices.delete(key);
  }
  const { [scope]: removed, ...rest } = senderNotices();
  if (removed) senderNotices.set(rest);
}

/** The POST belongs to this principal and destination even after navigation. */
function captureSendOrigin(c: Client, to: SendDestination) {
  const scope = destinationScope(to);
  const sender = me();
  const privateSender = noticeSenderId;
  const valid = guard(to.kind === "room" ? to.spaceId : undefined);
  const sameAccount = () => !!sender && me() === sender && client === c && valid();
  const canRetireDraft = () => sameAccount() && (to.kind === "chat" || noticeDestinationVisible(to));
  const request = nextAccessRequest();
  const epoch = noticeEpoch(scope);
  const after = cursors.get(scope) ?? 0;
  const wasVisible = noticeDestinationVisible(to);
  const current = () => sameAccount() && noticeEpoch(scope) === epoch;
  return {
    current,
    canRetireDraft,
    acknowledge(posted: PostedSenderNotes | null): boolean {
      if (!sameAccount()) return false;
      if (to.kind === "room" && noticeDestinationVisible(to)) verifyRoomAccess(to.spaceId, to.roomId, request);
      else if (to.kind === "chat") {
        verifyAccess(CHAT, request);
        observeChatAvailability(request, null);
      }
      if (!posted || posted.senderId !== privateSender || posted.eventId <= after ||
        !wasVisible || !current() || !noticeDestinationVisible(to)) return false;
      const notes = senderNotices()[scope];
      const key = pendingKey(scope, posted.eventId);
      if (notes?.has(posted.eventId) || pendingSenderNotices.has(key)) return false;
      const receipt = { ...posted, destination: to };
      if (posted.eventId <= (cursors.get(scope) ?? 0)) {
        const confirmed = corroborateSenderNotice(receipt, eventsByScope()[scope] ?? EMPTY_EVENTS);
        if (!confirmed) return false;
        senderNotices.set({ ...senderNotices(), [scope]: new Map(notes).set(posted.eventId, confirmed) });
      } else {
        if (pendingSenderNotices.size >= MAX_PENDING_SENDER_NOTICES) {
          pendingSenderNotices.delete(pendingSenderNotices.keys().next().value!);
        }
        pendingSenderNotices.set(key, receipt);
      }
      return true;
    },
    deny() {
      if (!current() || newerAccess(scope, request)) return;
      forgetSenderNotices(scope);
      if (to.kind === "room") void recheckRoomAccess(c, to, sameAccount, false);
    },
  };
}

function corroborateSenderNotice(receipt: PendingSenderNotice, rows: readonly RoomEvent[]): ConfirmedSenderNotice | null {
  const { text, recipientsKey, ...note } = receipt;
  const row = rows.find(e => e.id === note.eventId);
  return row && row.actorId === note.senderId && isCanonicalMessageRow(row) &&
    row.createdAt === note.createdAt && row.text === text &&
    renderedRecipientsKey(row.mentions) === recipientsKey ? { note, row } : null;
}

/** Resolve POST evidence once, when polling reaches the id. Polls create no receipts. */
function reconcileSenderNotices(scope: string, through: number, rows: readonly RoomEvent[]) {
  const saved = senderNotices()[scope];
  let next: Map<number, ConfirmedSenderNotice | null> | undefined;
  if (saved && rows.length) {
    for (const id of saved.keys()) if (id < rows[0]!.id) (next ??= new Map(saved)).delete(id);
  }
  for (const [key, receipt] of pendingSenderNotices) {
    if (receipt.eventId > through || destinationScope(receipt.destination) !== scope) continue;
    pendingSenderNotices.delete(key);
    const confirmed = corroborateSenderNotice(receipt, rows);
    if (confirmed) (next ??= new Map(saved)).set(receipt.eventId, confirmed);
  }
  if (next) senderNotices.set({ ...senderNotices(), [scope]: next });
}

export function activeSenderNotice(event: RoomEvent): SenderNotice | null {
  if (event.kind !== "message" || !event.actorId || event.actorId !== me()) return null;
  if (basePlace() !== "chat" && (basePlace() !== "space" || view() !== "room" || switchingSpace())) return null;
  const entry = senderNotices()[activeScope()]?.get(event.id);
  if (entry?.row !== event || !noticeDestinationVisible(entry.note.destination) || !activeEvents().includes(event)) return null;
  return entry.note;
}

/** A delayed UI callback must not act on a notice from a place already left. */
export function senderNoticeIsCurrent(note: SenderNotice) {
  const scope = destinationScope(note.destination);
  const entry = senderNotices()[scope]?.get(note.eventId);
  return me() === note.senderId && !settingsOpen() && scope === activeScope() &&
    (note.destination.kind === "chat" ? basePlace() === "chat" : inSpaceComposer()) &&
    noticeDestinationVisible(note.destination) && entry?.note === note && !!eventsByScope()[scope]?.includes(entry.row);
}

export function dismissSenderNotice(note: SenderNotice) {
  if (!senderNoticeIsCurrent(note)) return;
  const scope = destinationScope(note.destination);
  const notes = new Map(senderNotices()[scope]);
  notes.set(note.eventId, null);
  senderNotices.set({ ...senderNotices(), [scope]: notes });
}

/** Prepare a bound generic mention only in an empty Space draft. */
export function prepareSenderNoticeDraft(note: SenderNotice): number | null {
  if (note.addressing?.kind !== "classification_failed" || note.destination.kind !== "room" ||
    !senderNoticeIsCurrent(note) || sending()) return null;
  if (draft() || attachments?.pending(activeScope())) return draft().length; // focus the existing draft
  return addMention({ kind: "agent" });
}

/**
 * Files staged in the composer, uploaded on submission. Registered by
 * src/features/attachments so this module needs no library code. `upload`
 * puts every staged file of `scope` in the destination's library (reusing
 * ids a failed attempt already got) and resolves to their ids, or to the
 * message to show when one failed, naming the action to retry (nothing is
 * posted then, the draft and files stay). `sent` drops the files a message carried.
 */
export type AttachmentHook = {
  pending(scope: string): boolean;
  upload(scope: string, to: SendDestination, retryAction: "Send" | "Save note", ok: () => boolean): Promise<{ ids: string[]; keys: string[] } | { error: string }>;
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

type ModelChoice = () => { choice?: TurnModelChoice; error?: string | null };

/**
 * Posts the draft (and any staged files) to the active place; `invokeAgent`
 * asks the agent to answer. Files are uploaded to the destination's library
 * first; if one fails nothing is posted and the draft and files stay. A
 * message may be files only, unless it asks an agent in a Space.
 */
export async function send(invokeAgent: boolean, choice?: TurnModelChoice | ModelChoice, agentId?: string): Promise<void> {
  await sendDraft({ kind: "message", invokeAgent, choice, agentId });
}

export async function saveChatNote(): Promise<void> {
  await sendDraft({ kind: "chat-note" });
}

type DraftSend =
  | { kind: "message"; invokeAgent: boolean; choice?: TurnModelChoice | ModelChoice; agentId?: string }
  | { kind: "chat-note" };

async function sendDraft(intent: DraftSend): Promise<void> {
  const chat = basePlace() === "chat";
  const saveNote = intent.kind === "chat-note";
  if (saveNote && !chat) return;
  const retryAction = saveNote ? "Save note" : "Send";
  const { invokeAgent, choice: pickModel, agentId } = saveNote ? { invokeAgent: false } : intent;
  const submittedText = draft();
  const text = submittedText.trim();
  const sid = activeSpaceId();
  const rid = activeRoomId();
  const c = client;
  const scope = activeScope();
  if (!c || sending() || !scope || (!chat && (!sid || !rid || !inSpaceComposer()))) return;
  const files = !!attachments?.pending(scope);
  if (!text && !files) return;
  if (!chat && invokeAgent && !text) {
    batch(() => {
      actionError.set(ASK_NEEDS_TEXT);
      notice.set(null);
    });
    return;
  }
  const to: SendDestination = chat ? { kind: "chat" } : { kind: "room", spaceId: sid!, roomId: rid! };
  const sameSession = guard();
  const ok = chat ? sameSession : guard(sid!);
  const navigation = navigationGeneration;
  // Feedback only where it happened: never in a place the user moved to.
  const here = () => ok() && navigation === navigationGeneration && activeScope() === scope &&
    (chat ? basePlace() === "chat" : inSpaceComposer());
  let address: SpaceMessageBody | null = null;
  let submittedDraft: MentionDraft | undefined;
  if (!chat) {
    const askAgent = invokeAgent ? { agentId } : undefined;
    const directory = editDirectory();
    submittedDraft = captureForDirectory(mentionDraft(), directory, { finalize: true });
    writeMentionDraft(submittedDraft, directory);
    // Finalize against the draft's directory. The fresh roster after any
    // uploads can require confirmation, but never replaces a binding.
    const bound = submittedDraft.anchors.map(a => a.mention);
    const people = new Set((directory?.members ?? []).map(m => m.id));
    const named = new Set((directory?.agents ?? []).map(a => a.id));
    for (const recipient of bound) {
      if (recipient.kind === "user") people.add(recipient.id);
      else if (recipient.agentId) named.add(recipient.agentId);
    }
    if (askAgent?.agentId) named.add(askAgent.agentId);
    const result = prepareMentionSend(submittedDraft, {
      members: [...people].map(id => ({ id })),
      agents: [...named].map(id => ({ id, handle: "", name: "" })),
    }, { askAgent });
    if (!result.ok) {
      setMentionSendError(result);
      return;
    }
    address = result.body;
  }
  const willAsk = invokeAgent || address?.mentions?.some(m => m.kind === "agent");
  const selected = typeof pickModel === "function" ? (willAsk ? pickModel() : {}) : { choice: pickModel };
  if (selected.error) {
    actionError.set(selected.error);
    return;
  }
  batch(() => {
    sending.set(invokeAgent ? "agent" : "message");
    actionError.set(null);
    notice.set(null);
  });
  const mayPost = () => here() && mentionDraft() === submittedDraft;
  let origin: ReturnType<typeof captureSendOrigin> | undefined;
  try {
    let ids: string[] = [];
    let keys: string[] = [];
    if (files && attachments) {
      const up = await attachments.upload(scope, to, retryAction, ok);
      if (!ok()) return;
      if ("error" in up) {
        if (here()) actionError.set(up.error);
        return;
      }
      ids = up.ids;
      keys = up.keys;
    }
    if (chat) {
      origin = captureSendOrigin(c, to);
      if (saveNote) {
        await c.saveChatNote(text, ids);
        origin.acknowledge(null);
      } else origin.acknowledge(await c.sendChatMessage(text, ids, selected.choice));
    } else {
      if (!mayPost()) {
        if (here()) actionError.set("The draft or destination changed. Nothing was sent; review the draft and send again.");
        return;
      }
      const body = address!;
      const verified = await verifySpaceRecipients(c, sid!, body, submittedDraft!, files, mayPost);
      if (verified?.kind === "confirmation" && mayPost() && verified.confirmation.current()) {
        batch(() => {
          mentionConfirmation.set(verified.confirmation);
          writeMentionDraft(verified.draft, verified.confirmation.directory);
          actionError.set("A mention no longer has the same recipient. Pick who you mean. Nothing was sent.");
        });
        return;
      }
      if (verified?.kind !== "ok" || !mayPost() || !verified.current()) {
        if (here()) actionError.set(mayPost()
          ? verified?.kind !== "stale"
            ? "Could not verify the current recipient list. Nothing was sent."
            : "A recipient is no longer in this Space. Remove their mention or choose someone else. Nothing was sent."
          : "The draft or destination changed. Nothing was sent; review the draft and send again.");
        return;
      }
      origin = captureSendOrigin(c, to);
      origin.acknowledge(await c.sendMessage(sid!, rid!, body, ids, selected.choice));
    }
    // Accepted files are identified by immutable staging keys, not by the
    // current room epoch. Never leave them staged for a duplicate send after
    // rejoin, and never touch a replacement account's staging.
    if (!sameSession()) return;
    if (keys.length) attachments?.sent(scope, keys, to);
    if (!origin.canRetireDraft()) return;
    if (activeScope() === scope && (chat ? draft() === submittedText : mentionDraft() === submittedDraft)) {
      // unless the user typed something new meanwhile
      batch(() => {
        if (chat) draft.set("");
        else writeMentionDraft(EMPTY_MENTION_DRAFT, null);
      });
    } else if (chat ? stashedDrafts()[scope]?.text === submittedText : stashedDrafts()[scope]?.mention === submittedDraft) {
      // Sent from a place the user has since left: its stashed draft is gone too.
      const { [scope]: _sent, ...rest } = stashedDrafts();
      stashedDrafts.set(rest);
    }
    if (chat) lastRunsRefresh = 0;
    kick();
  } catch (error) {
    if (sameSession() && error instanceof ApiError && error.kind === "auth") {
      disconnect(error.message);
      return;
    }
    if (origin && !origin.current()) return;
    if (origin && isAccessDenial(error)) origin.deny();
    if (!here()) return;
    handleActionError(error, saveNote ? "Could not save the note" : invokeAgent ? (chat ? "Could not ask your agent" : "Could not ask the agent") : "Could not send the message", origin || chat ? undefined : sid!);
    const shown = actionError();
    if (files && shown) actionError.set(chat
      ? `${shown} Its files are already in the Library; ${retryAction} again won't upload them twice.`
      : `${shown} The uploaded files remain in the Library.`);
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
  const c = client;
  if (!c || !spaces().some((s) => s.id === spaceId)) return;
  const ok = guard(spaceId);
  const navigation = navigationGeneration;
  const pending = { spaceId, roomId: null };
  const here = () => basePlace() === "chat" && navigation === navigationGeneration && dispatchTarget() === pending;
  dispatchError.set(null);
  dispatchNotice.set(null);
  dispatchTarget.set(pending);
  const request = nextAccessRequest();
  try {
    // Follow up at the exact requested room or thread. A cached list can
    // miss a new thread or still contain one that no longer exists.
    const list = roomId === undefined ? await ensureRooms(spaceId) : await c.listRooms(spaceId);
    if (!ok()) return;
    if (roomId === undefined) {
      if (dispatchTarget() === pending) dispatchTarget.set({ spaceId, roomId: pickRoom(spaceId, list) });
    } else {
      const verified = verifyRoomList(spaceId, list, request);
      const listed = new Set(verified.map(r => r.id));
      const found = listed.has(roomId);
      const key = scopeKey(spaceId, roomId);
      const stale = !found && (roomDeniedAtOrAfter(key, request) || newerAccess(key, request));
      batch(() => {
        if (!stale) {
          // Keep newer verified rooms and leave other active room views
          // alone; their own poll is responsible for confirming a loss.
          const keep = roomsOf(spaceId).filter(r => !listed.has(r.id) &&
            (newerAccess(scopeKey(spaceId, r.id), request) ||
              (r.id !== roomId && activeSpaceId() === spaceId && activeRoomId() === r.id)));
          roomsBySpace.set({ ...roomsBySpace(), [spaceId]: [...verified, ...keep] });
          if (!found) dropLostRoom(spaceId, roomId, request, false);
        }
        if (found && dispatchTarget() === pending) dispatchTarget.set({ spaceId, roomId });
        else if (!found && here()) dispatchError.set(stale
          ? "Couldn't verify that destination. Follow up again or choose another room."
          : "That room or thread is no longer available. Choose another destination.");
      });
    }
  } catch (error) {
    if (!ok()) return;
    const show = here();
    const denied = roomId !== undefined && isAccessDenial(error);
    const stale = denied && newerAccess(scopeKey(spaceId, ""), request);
    batch(() => {
      if (denied && !stale) dropLostSpace(spaceId, request, false);
      if (show) dispatchError.set(stale
        ? "Couldn't verify that destination. Follow up again or choose another room."
        : describe(error, "Could not load that Space's rooms"));
    });
  }
}

export function setDispatchRoom(roomId: string) {
  const t = dispatchTarget();
  if (t && roomsBySpace()[t.spaceId]?.some(r => r.id === roomId)) {
    dispatchTarget.set({ spaceId: t.spaceId, roomId });
    dispatchError.set(null);
  }
}

/** Sends the dispatch draft to the chosen Space and room and asks that Space's agent. */
export async function sendDispatch(): Promise<void> {
  const text = dispatchDraft();
  const target = dispatchTarget();
  const c = client;
  if (!text.trim() || !target?.roomId || !c || dispatching()) return;
  const space = spaces().find((s) => s.id === target.spaceId);
  const room = roomsBySpace()[target.spaceId]?.find((r) => r.id === target.roomId);
  if (!space || !room) {
    dispatchError.set("Choose a Space and room you belong to first.");
    return;
  }
  const sameSession = guard();
  const navigation = navigationGeneration;
  const here = () => basePlace() === "chat" && navigation === navigationGeneration && dispatchTarget() === target;
  const origin = captureSendOrigin(c, { kind: "room", spaceId: target.spaceId, roomId: target.roomId });
  batch(() => {
    dispatching.set(true);
    dispatchError.set(null);
    dispatchNotice.set(null);
  });
  try {
    // Dispatch is an explicit request to the chosen Space's agent, not a
    // mention composer. Its text cannot add recipients implicitly.
    const hadNotice = origin.acknowledge(await c.sendMessage(target.spaceId, target.roomId, { text, invoke_agent: true }));
    if (!origin.canRetireDraft()) return;
    if (dispatchTarget() === target && dispatchDraft() === text) dispatchDraft.set("");
    if (here()) dispatchNotice.set(hadNotice ? `Posted in ${space.name} › #${room.name}.` :
      `Sent to the agent in ${space.name} › #${room.name}.`);
    lastRunsRefresh = 0;
    kick();
  } catch (error) {
    if (sameSession() && error instanceof ApiError && error.kind === "auth") disconnect(error.message);
    else if (origin.current()) {
      if (isAccessDenial(error)) origin.deny();
      if (here() && !isAborted(error)) dispatchError.set(describe(error, "Could not send the task"));
    }
  } finally {
    if (sameSession()) dispatching.set(false);
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
  const version = nextMemberVersion(sid);
  try {
    const list = await c.listMembers(sid);
    if (ok()) adoptMembers(sid, version, list);
  } catch (error) {
    if (ok() && !isAborted(error) && markMembersUnverified(sid, version, error)) {
      handleManageError(error, "Could not load members", sid);
    }
  }
}

/** Start a distinct visit even on re-entry to the same cached Space. */
export function beginMentionVisit(sid: string, scope: string) {
  const visit: MentionVisit = { sid, scope, navigation: navigationGeneration, token: {}, verified: false };
  mentionVisit.set(visit);
  void loadMembersForMentions(true);
  return () => {
    if (mentionVisit()?.token === visit.token) mentionVisit.set(null);
  };
}

function currentMentionVisit(visit: MentionVisit) {
  return visit.navigation === navigationGeneration && activeScope() === visit.scope && inSpaceComposer()
    && mentionVisit()?.token === visit.token;
}

export function mentionVisitVerified(sid: string) {
  const visit = mentionVisit();
  return !!visit?.verified && visit.sid === sid && currentMentionVisit(visit);
}

export function mentionVisitGuard(sid: string) {
  const visit = mentionVisit();
  const ok = guard(sid);
  return () => !!visit && visit.sid === sid && ok() && currentMentionVisit(visit);
}

/** Same key through background cache refreshes, different on navigation or leaving the composer. */
export function mentionVisitKey() {
  const scope = activeScope();
  return scope && inSpaceComposer() ? `${navigationGeneration}:${scope}` : null;
}

/** Read evidence for a current member, shared by preflight and profile refresh. */
export function mentionProfileEvidence(userId: string) {
  const scope = memberAliasScope();
  if (!scope || !members()?.some(m => m.userId === userId)) return null;
  return {
    scope,
    verified: scope.entries().has(userId),
    start: () => {
      if (memberAliasScope() !== scope) return null;
      const order = ++scope.next;
      return (displayName: string | null | undefined) => {
        if (memberAliasScope() !== scope) return;
        const entries = scope.entries();
        if ((entries.get(userId)?.order ?? 0) >= order) return;
        scope.entries.set(new Map(entries).set(userId, { order, displayName: displayName || null }));
      };
    },
  };
}

/** Require every current member, including verified profiles with no readable name. */
export function mentionMemberNames(sid: string): MentionDirectory["members"] | null {
  if (activeSpaceId() !== sid) return null;
  const entries = memberAliasScope()?.entries();
  const list = members();
  if (!entries || !list || !list.every(m => entries.has(m.userId))) return null;
  return list.map(({ userId }) => ({ id: userId, displayName: entries.get(userId)!.displayName }));
}

/** A new visit must not reuse an older visit's in-flight roster request. */
export function loadMembersForMentions(fresh = false): Promise<void> {
  const sid = activeSpaceId();
  const c = client;
  const visit = mentionVisit();
  if (!sid || !c || !visit || visit.sid !== sid || !currentMentionVisit(visit)) return Promise.resolve();
  const existing = memberRequests.get(sid);
  if (existing && !fresh) return existing;
  const ok = guard(sid);
  const version = nextMemberVersion(sid);
  const work = c.listMembers(sid).then(list => {
    if (ok() && currentMentionVisit(visit)) {
      batch(() => {
        if (adoptMembers(sid, version, list) && !visit.verified) mentionVisit.set({ ...visit, verified: true });
      });
    }
  }, error => {
    if (!ok() || !currentMentionVisit(visit) || isAborted(error)) return;
    if (!markMembersUnverified(sid, version, error)) return;
    if (error instanceof ApiError && error.kind === "auth") disconnect(error.message);
    else if (error instanceof ApiError && (error.kind === "forbidden" || error.kind === "not_found")) void lostAccess(sid, null);
  }).finally(() => {
    if (memberRequests.get(sid) === work) memberRequests.delete(sid);
  });
  memberRequests.set(sid, work);
  return work;
}

// ------------------------------------------------------------------ mentions

function nextMemberVersion(sid: string) {
  const version = (memberVersions.get(sid) ?? 0) + 1;
  memberVersions.set(sid, version);
  return version;
}

function sameMemberIds(a: readonly Member[] | undefined, b: readonly Member[]) {
  const ids = new Set(a?.map(m => m.userId));
  return a?.length === b.length && b.every(m => ids.has(m.userId));
}

function adoptMembers(sid: string, version: number, list: Member[]) {
  const before = memberEvidence()[sid];
  if (before && version < before.version) return false;
  // In-flight requests aren't evidence. Identical successful replies keep
  // the same roster token, including reordering or changes to a role.
  const roster = before && !memberFailures()[sid] && sameMemberIds(membersBySpace()[sid], list) ? before.roster : {};
  batch(() => {
    membersBySpace.set({ ...membersBySpace(), [sid]: list });
    memberEvidence.set({ ...memberEvidence(), [sid]: { version, roster } });
    if (memberFailures()[sid]) memberFailures.set({ ...memberFailures(), [sid]: null });
  });
  return true;
}

function markMembersUnverified(sid: string, version: number, error: unknown) {
  const before = memberEvidence()[sid];
  if (before && version < before.version) return false;
  batch(() => {
    memberEvidence.set({ ...memberEvidence(), [sid]: { version, roster: {} } });
    memberFailures.set({ ...memberFailures(), [sid]: describe(error, "Could not verify members") });
  });
  return true;
}

/** The visible, permission-scoped room/thread, never the Space left beneath another view. */
export function inSpaceComposer() {
  return basePlace() === "space" && !settingsOpen() && view() === "room" && !!activeSpace() && !!activeRoom() && !switchingSpace();
}

function captureForDirectory(value: MentionDraft, directory: MentionDirectory | null, options?: { finalize?: boolean; caret?: number }): MentionDraft {
  const captured = captureTypedMentions(value, directory ?? EMPTY_DIRECTORY, options);
  if (directory || captured === value) return captured;
  // Without a verified directory even the reserved generic token could be
  // a prefix of a longer label. Only an explicit pick may bind it now.
  const asPlain = ({ start, end, literal }: MentionSelection) => ({ start, end, literal, kind: "unresolved" as const });
  const decisions = [
    ...(captured.decisions ?? []).map(d => d.kind === "ambiguous" && !value.decisions?.includes(d) ? asPlain(d) : d),
    ...captured.anchors.filter(a => !value.anchors.includes(a)).map(asPlain),
  ];
  return { text: captured.text, anchors: value.anchors, decisions };
}

function writeMentionDraft(value: MentionDraft, directory: MentionDirectory | null) {
  batch(() => {
    if (!value.decisions?.some(d => d.kind === "ambiguous" && d.preflight)) mentionConfirmation.set(null);
    mentionDraft.set(value);
    editDirectory.set(directory);
    draft.set(value.text);
  });
}

/** Use the same snapshot for preview and finalization; never capture on roster refresh. */
export function previewMentionDraft() {
  return captureForDirectory(mentionDraft(), editDirectory(), { finalize: true });
}

export function previewMentionMatches(value: MentionDraft) {
  return scanMentionDraft(value, editDirectory() ?? EMPTY_DIRECTORY, { finalize: true });
}

/** Never fall back to cached profiles when a send required a fresh confirmation. */
export function verifiedConfirmationDirectory() {
  const confirmation = mentionConfirmation();
  return confirmation?.current() ? confirmation.directory : null;
}

/** Picking can only use current data and the exact literal the chooser showed. */
export function addMention(mention: MentionRecipient, selection?: MentionSelection): number | null {
  const sid = activeSpaceId();
  if (!sid || !inSpaceComposer()) return null;
  const value = mentionDraft();
  const end = value.text.length;
  const range = selection ?? { start: end, end, literal: "" };
  const decision = value.decisions?.find(d => d.start === range.start && d.end === range.end && d.literal === range.literal);
  const directory = decision?.kind === "ambiguous" && decision.preflight ? verifiedConfirmationDirectory() : directoryForMentions(sid);
  if (decision?.kind === "ambiguous" && decision.preflight
    && (!directory || !decision.choices.some(m => mentionIdentityKey(m) === mentionIdentityKey(mention)))) return null;
  const picked = pickMention(value, range, mention, directory ?? EMPTY_DIRECTORY);
  if (!picked) return null;
  writeMentionDraft(picked, editDirectory());
  const caret = range.end + picked.text.length - value.text.length;
  return picked.text[caret] === " " ? caret + 1 : caret;
}

/** Remove only the exact tracked occurrences of this recipient. */
export function removeMention(mention: MentionRecipient) {
  if (!inSpaceComposer()) return;
  const key = mentionIdentityKey(mention);
  let value = mentionDraft();
  for (const a of [...value.anchors].reverse()) {
    if (mentionIdentityKey(a.mention) === key) {
      const end = a.end + (value.text[a.end] === " " ? 1 : 0);
      value = editMentionDraft(value, { start: a.start, end, replacement: "" }) ?? value;
    }
  }
  writeMentionDraft(value, editDirectory());
}

export function setDraft(text: string) {
  const sid = activeSpaceId();
  if (basePlace() !== "space" || !sid) {
    draft.set(text);
    return;
  }
  const before = mentionDraft();
  if (before.text === text) return;
  // NSH gives us a value snapshot, not a native edit range. The helper
  // conservatively preserves only anchors outside ambiguous edit ranges.
  let caret = text.length;
  for (let i = before.text.length; caret > 0 && i > 0 && before.text[i - 1] === text[caret - 1]; i--) caret--;
  const directory = directoryForMentions(sid);
  writeMentionDraft(captureForDirectory(updateMentionDraft(before, text), directory, { caret }), directory);
}

function setMentionSendError(result: Extract<MentionSend, { ok: false }>) {
  const message = {
    ambiguous: "Pick who you mean before sending.",
    "invalid-text": "The draft contains invalid text. Edit it and try again.",
    "invalid-anchor": "A mention changed. Edit it or choose its recipient again.",
    "stale-recipient": "A recipient could not be verified. Remove their mention or choose someone else.",
    "too-many-spans": "A message can contain up to 64 mentions. Remove some before sending.",
    "too-many-recipients": "A message can address up to 32 recipients. Remove some before sending.",
  }[result.reason];
  batch(() => {
    actionError.set(message);
    notice.set(null);
  });
}

type SpaceRecipientCheck = { kind: "stale" | "unverified" }
  | { kind: "ok"; current: () => boolean }
  | { kind: "confirmation"; draft: MentionDraft; confirmation: MentionConfirmation };

async function verifySpaceRecipients(
  c: Client, sid: string, body: SpaceMessageBody, submitted: MentionDraft, uploaded: boolean, mayPost: () => boolean,
): Promise<SpaceRecipientCheck | null> {
  const people = body.mentions?.flatMap(m => m.kind === "user" ? [m.id] : []) ?? [];
  const agents = body.mentions?.flatMap(m => m.kind === "agent" && m.agent_id ? [m.agent_id] : []) ?? [];
  if (body.agent_id) agents.push(body.agent_id);
  const checkAliases = submitted.anchors.some(a => !a.picked);
  const checkMembers = checkAliases || uploaded || people.length > 0;
  const version = checkMembers ? nextMemberVersion(sid) : 0;
  const visit = mentionVisitGuard(sid);
  const roster = checkMembers ? c.listMembers(sid).then(list => {
    if (mayPost()) adoptMembers(sid, version, list);
    return { list, evidence: memberEvidence()[sid]?.roster };
  }, error => {
    if (mayPost() && !isAborted(error)) markMembersUnverified(sid, version, error);
    throw error;
  }) : null;
  const readAgents = () => c.spaceAgents(sid).then(list => list.agents.filter(a => a.spaceId === sid));
  const [checked, explicitAgents] = await Promise.all([roster, !checkAliases && agents.length ? readAgents() : null]);
  if (!mayPost()) return null;
  const members = checked?.list;
  let currentAgents: MentionDirectory["agents"] | null = explicitAgents;
  let aliasesCurrent = () => true;
  if (checkAliases) {
    if (!members || !members.some(m => m.userId === me())) return { kind: "unverified" };
    for (let i = 0; i < members.length; i += 8) {
      const read = await Promise.all(members.slice(i, i + 8).map(async ({ userId }) => {
        const complete = mentionProfileEvidence(userId)?.start();
        if (!complete) return false;
        const profile = await c.profileOf(userId);
        if (profile && profile.id !== userId) return false;
        if (mayPost()) complete(profile?.displayName);
        return true;
      }));
      if (!mayPost()) return null;
      if (read.some(ok => !ok)) return { kind: "unverified" };
    }
    currentAgents = await refreshAgentsForMentions?.(sid, mayPost) ?? null;
    if (!mayPost()) return null;
    const rosterCurrent = () => checked?.evidence === memberEvidence()[sid]?.roster
      && !membersError() && sameMemberIds(membersBySpace()[sid], members);
    // Use the newest completed observations, not necessarily the replies
    // from this send: a newer background read may have resolved first.
    const freshPeople = mentionMemberNames(sid);
    if (!currentAgents || !freshPeople || !rosterCurrent()) return { kind: "unverified" };
    const directory = { members: freshPeople, agents: currentAgents };
    const personNames = new Map(freshPeople.map(m => [m.id, m.displayName]));
    const agentNames = new Map(currentAgents.map(a => [a.id, a]));
    aliasesCurrent = () => {
      if (!visit() || !rosterCurrent()) return false;
      const people = mentionMemberNames(sid);
      if (!people || people.length !== personNames.size || !people.every(m => personNames.get(m.id) === m.displayName)) return false;
      const latest = verifiedMentionAgents(sid);
      return !!latest && latest.length === agentNames.size && latest.every(a => {
        const saved = agentNames.get(a.id);
        return saved?.name === a.name && saved.handle === a.handle;
      });
    };
    const confirmed = confirmChangedAutomaticMentions(submitted, directory);
    if (confirmed !== submitted) {
      let expired = false;
      const current = () => {
        if (!expired && !aliasesCurrent()) expired = true;
        return !expired;
      };
      return { kind: "confirmation", draft: confirmed, confirmation: { directory, current } };
    }
  }
  // Require both snapshots to agree on the concrete members in use.
  const ids = [me(), ...people];
  const membersCurrent = () => !checkMembers || !membersError() && ids.every(id =>
    members?.some(m => m.userId === id) && membersBySpace()[sid]?.some(m => m.userId === id));
  if (!membersCurrent() || !agents.every(id => currentAgents?.some(a => a.id === id))) return { kind: "stale" };
  return { kind: "ok", current: () => aliasesCurrent() && membersCurrent() };
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
// own text and range bindings. `draft` and `mentionDraft` are the active place's;
// switching stashes them under the place left and restores the new one's.
const stashedDrafts = signal<Record<string, { text: string; mention: MentionDraft; directory: MentionDirectory | null }>>({});
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
    if (left && draft()) stash[left] = { text: draft(), mention: mentionDraft(), directory: editDirectory() };
    else delete stash[left];
    const back = stash[scope];
    delete stash[scope];
    batch(() => {
      stashedDrafts.set(stash);
      draft.set(back?.text ?? "");
      mentionDraft.set(back?.mention ?? EMPTY_MENTION_DRAFT);
      editDirectory.set(back?.directory ?? null);
      mentionConfirmation.set(null);
    });
  });
});

// ------------------------------------------------------------------ loop

function endSession() {
  session++;
  abort?.abort();
  abort = null;
  client = null;
  noticeSenderId = null;
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
  noticeEpochs.clear();
  pendingSenderNotices.clear();
  verifiedAccess.clear();
  deniedSpaceAccess.clear();
  deniedRoomAccess.clear();
  accessProbes.clear();
  chatAvailabilityRequest = 0;
  chatProbeAfter = 0;
  memberRequests.clear();
  memberVersions.clear();
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
    senderNotices.set({});
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
    memberFailures.set({});
    memberEvidence.set({});
    mentionVisit.set(null);
    mentionConfirmation.set(null);
    mentionDraft.set(EMPTY_MENTION_DRAFT);
    editDirectory.set(null);
    manageError.set(null);
    memberBusy.set(null);
    orgAdmin.set(false);
    settingsSection.set("profile");
    settingsAnchor.set(null);
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
      if (Date.now() >= chatProbeAfter) await pollChat();
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
  failures++;
  const retryInMs = Math.min(MAX_BACKOFF_MS, 1000 * 2 ** (failures - 1));
  status.set({ phase: "retrying", attempt: failures, retryInMs, message: describe(error, "Connection problem") });
  return retryInMs;
}

function dropLostSpace(spaceId: string, request: number, show: boolean) {
  if (!recordSpaceDenial(spaceId, request)) return;
  const name = spaces().find(s => s.id === spaceId)?.name;
  batch(() => {
    purgeSpace(spaceId, true);
    if (activeSpaceId() !== spaceId) return;
    activeSpaceId.set(null);
    activeRoomId.set(null);
    view.set("room");
    if (basePlace() === "space") {
      actionError.set(null);
      notice.set(show && name ? `You no longer have access to ${name}.` : null);
      basePlace.set("chat");
    }
  });
}

/** A POST or poll denial invalidates the originating room's notes now.
 * Only a scoped, still-current access check may drop its public state. */
async function recheckRoomAccess(c: Client, to: Extract<SendDestination, { kind: "room" }>, valid: () => boolean, show: boolean) {
  const { spaceId, roomId } = to;
  const scope = scopeKey(spaceId, roomId);
  const spaceScope = scopeKey(spaceId, "");
  const inProgress = accessProbes.get(scope);
  // A stream of denials must not keep replacing its own slow probe. A
  // denial after fresh positive evidence, however, needs a fresh check.
  if (inProgress && !newerAccess(scope, inProgress.request) && !newerAccess(spaceScope, inProgress.request)) return;
  const spaceRequest = nextAccessRequest();
  const probe = { request: spaceRequest };
  accessProbes.set(scope, probe);
  const current = () => valid() && accessProbes.get(scope) === probe;
  try {
    const visible = await c.listSpaces().catch(() => null);
    if (!current() || !visible) return;
    if (visible.some(s => s.id === spaceId)) verifyAccess(spaceScope, spaceRequest);
    else if (!newerAccess(spaceScope, spaceRequest)) {
      dropLostSpace(spaceId, spaceRequest, show);
      return;
    }
    // The room read is a new observation. Access verified while the Space
    // list was in flight does not outrank a denial on this later leg.
    const roomRequest = probe.request = nextAccessRequest();
    try {
      const list = await c.listRooms(spaceId);
      if (!current()) return;
      if (list.some(r => r.id === roomId)) verifyRoomAccess(spaceId, roomId, roomRequest);
      else dropLostRoom(spaceId, roomId, roomRequest, show);
    } catch (error) {
      if (current() && isAccessDenial(error)) dropLostSpace(spaceId, roomRequest, show);
    }
  } finally {
    if (accessProbes.get(scope) === probe) accessProbes.delete(scope);
  }
}

function recordRoomDenial(key: string, request: number) {
  if (roomDeniedAtOrAfter(key, request) || newerAccess(key, request)) return false;
  deniedRoomAccess.set(key, request);
  return true;
}

function dropLostRoom(spaceId: string, roomId: string, request: number, show: boolean) {
  const key = scopeKey(spaceId, roomId);
  if (!recordRoomDenial(key, request)) return;
  cursors.delete(key);
  batch(() => {
    forgetSenderNotices(key);
    roomsBySpace.set({ ...roomsBySpace(), [spaceId]: roomsOf(spaceId).filter(r => r.id !== roomId) });
    const target = dispatchTarget();
    if (target?.spaceId === spaceId && target.roomId === roomId) {
      dispatchTarget.set({ spaceId, roomId: null });
    }
    const { [key]: _e, ...events } = eventsByScope();
    eventsByScope.set(events);
    if (activeSpaceId() === spaceId && activeRoomId() === roomId) {
      activeRoomId.set(pickRoom(spaceId, roomsOf(spaceId)));
      if (show && basePlace() === "space") notice.set("That room is no longer available.");
      else if (basePlace() === "space") actionError.set(null);
    }
  });
}

/**
 * A Space endpoint said 403/404. Reload the Space list to tell "removed from
 * the Space" (purge it, move on) from "this room is gone" (drop the room).
 */
async function lostAccess(spaceId: string | null, roomId: string | null) {
  const c = client;
  if (!spaceId || !c) return;
  if (roomId) {
    forgetSenderNotices(scopeKey(spaceId, roomId));
    await recheckRoomAccess(c, { kind: "room", spaceId, roomId }, guard(spaceId), true);
    return;
  }
  try {
    await refreshLists(true);
  } catch {
    /* the next tick deals with it */
  }
}

/**
 * The user just deleted or left a Space (src/features/space): reload the
 * list, which forgets it and leaves it if it was open, then say what happened.
 */
export async function spaceGone(spaceId: string, message: string): Promise<void> {
  const current = guard();
  await lostAccess(spaceId, null);
  if (current() && !spaces().some((s) => s.id === spaceId)) notice.set(message);
}

/** A Space's name changed (src/features/space): show it everywhere at once. */
export function spaceRenamed(spaceId: string, name: string) {
  spaces.set(spaces().map((s) => (s.id === spaceId ? { ...s, name } : s)));
}

/** Merge a same-session membership snapshot with newer scoped evidence.
 * Startup publishing a snapshot into an existing session must use the
 * same request order; only a genuinely new session starts with no evidence. */
function applySpaceList(list: readonly Space[], request: number) {
  const previous = new Map(spaces().map(s => [s.id, s]));
  const listed = new Set(list.map(s => s.id));
  const next: Space[] = [];
  batch(() => {
    for (const s of list) {
      if (deniedAtOrAfter(s.id, request)) continue;
      const current = previous.get(s.id);
      const key = scopeKey(s.id, "");
      // A snapshot older than newer verified access doesn't replace its
      // current listing (including role), even when both include the Space.
      if (current && newerAccess(key, request)) next.push(current);
      else {
        verifyAccess(key, request);
        next.push(s);
      }
    }
    for (const [sid, s] of previous) {
      if (listed.has(sid)) continue;
      if (newerAccess(scopeKey(sid, ""), request)) next.push(s);
      else dropLostSpace(sid, request, true);
    }
    // Even after eviction, a newer omission is evidence the Space is
    // still gone, not a chance for an intermediate positive reply to win.
    for (const sid of deniedSpaceAccess.keys()) {
      if (!listed.has(sid)) recordSpaceDenial(sid, request);
    }
    spaces.set(next);
  });
}

/** Reloads the Space list and the active Space's rooms. */
async function refreshLists(force = false) {
  const c = client;
  if (!c) return;
  if (!force && Date.now() - lastListsRefresh < LISTS_REFRESH_MS) return;
  const ok = guard();
  const request = nextAccessRequest();
  const list = await c.listSpaces();
  if (!ok()) return;
  lastListsRefresh = Date.now();
  applySpaceList(list, request);
  const active = activeSpaceId();
  if (active) {
    const okSpace = guard(active);
    const roomRequest = nextAccessRequest();
    let rooms: Room[];
    try {
      rooms = await c.listRooms(active);
    } catch (error) {
      if (!okSpace()) return;
      if (isAccessDenial(error)) {
        dropLostSpace(active, roomRequest, true);
        return;
      }
      throw error;
    }
    if (!okSpace()) return;
    const snapshotIds = new Set(rooms.map(r => r.id));
    rooms = verifyRoomList(active, rooms, roomRequest);
    const previous = roomsBySpace()[active] ?? EMPTY_ROOMS;
    const current = activeRoomId();
    const next = keepNewerRooms(active, rooms, roomRequest);
    batch(() => {
      // Newer room access, including a successful create, wins over an
      // older omission. An authoritative loss must also clear Dispatch.
      roomsBySpace.set({ ...roomsBySpace(), [active]: next });
      for (const r of previous) {
        if (!snapshotIds.has(r.id)) dropLostRoom(active, r.id, roomRequest, true);
      }
      // An absent room can have an intermediate positive read still in
      // flight. Advance existing tombstones even when no row is cached.
      const prefix = scopeKey(active, "");
      for (const key of deniedRoomAccess.keys()) {
        if (key.startsWith(prefix) && !snapshotIds.has(key.slice(prefix.length))) recordRoomDenial(key, roomRequest);
      }
      if (activeSpaceId() === active && (!current || !roomsBySpace()[active].some((r) => r.id === current))) {
        activeRoomId.set(pickRoom(active, roomsBySpace()[active]));
      }
    });
  }
}

async function pollRoom(spaceId: string, roomId: string): Promise<void> {
  const scope = scopeKey(spaceId, roomId);
  const sameSpace = guard(spaceId);
  const epoch = noticeEpoch(scope);
  const ok = () => sameSpace() && noticeEpoch(scope) === epoch;
  let request = 0;
  try {
    await poll(scope, ok, async (c, after) => {
      request = nextAccessRequest();
      const events = await c.listEvents(spaceId, roomId, after);
      if (ok()) verifyRoomAccess(spaceId, roomId, request);
      return events;
    });
  } catch (error) {
    if (!ok()) return;
    if (isAccessDenial(error)) {
      // Do not park the polling loop behind the recheck; fresh room access
      // must be able to supersede a slow denial.
      if (request && !newerAccess(scope, request)) void lostAccess(spaceId, roomId);
    } else throw error;
  }
}

async function pollChat(): Promise<void> {
  const sameSession = guard();
  const epoch = noticeEpoch(CHAT);
  const ok = () => sameSession() && noticeEpoch(CHAT) === epoch;
  let request = 0;
  try {
    await poll(CHAT, ok, async (c, after) => {
      request = nextAccessRequest();
      const events = await c.listChatEvents(after);
      if (ok()) {
        verifyAccess(CHAT, request);
        observeChatAvailability(request, null);
      }
      return events;
    });
  } catch (error) {
    if (!ok()) return;
    if (error instanceof ApiError && (error.kind === "forbidden" || error.kind === "not_found" || error.kind === "protocol")) {
      if (!request || newerAccess(CHAT, request)) return;
      observeChatAvailability(request, `Your Chat isn't available on this server: ${error.message}`);
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
    const through = fresh[fresh.length - 1].id;
    cursors.set(key, through);
    if (reactions) reactionsByScope.set({ ...reactionsByScope(), [key]: reactions });
    const current = eventsByScope()[key] ?? EMPTY_EVENTS;
    let rows = current;
    if (shown.length) {
      rows = current.concat(shown).slice(-MAX_EVENTS_PER_ROOM);
      eventsByScope.set({ ...eventsByScope(), [key]: rows });
    }
    reconcileSenderNotices(key, through, rows);
  });
  return fresh.length;
}

// ------------------------------------------------------------------ errors

const isAccessDenial = (error: unknown) => error instanceof ApiError &&
  (error.kind === "forbidden" || error.kind === "not_found");

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
