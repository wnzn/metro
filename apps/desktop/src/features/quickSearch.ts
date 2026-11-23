// Quick results under the top search box, while typing.
//
// - A nonblank, focused edit asks Metro for literal matches (POST
//   /v1/search/quick, through the client's `quickSearch`), after a short
//   pause. It calls no model and indexes nothing. At most 8 come back.
// - Where it looks follows the page: on the Library page, or Search in
//   Documents & media, that mode with its scope and type filter; on Search in
//   Conversations, messages with that scope; anywhere else, files and messages
//   across everything the user can read. It never widens a page's filters.
// - An answer is shown only for the query, place and session it was asked
//   for: any edit, blur, scope or session change, or membership change drops
//   the one in flight at once. Results from a Space the user has left are
//   never shown, and the place is checked again when a row is opened.
// - "More results" (always the last row) opens full Search with the query,
//   in the page's mode (Documents & media when a global lookup found only
//   files), and runs it there once, as Return does. Return with no row
//   chosen is the full search, as before.

import { batch, computed, effect, signal, untracked } from "nsh";
import type { SearchHit } from "../api/discovery";
import type { LibraryAsset, MediaKind } from "../api/library";
import { apiClient, mainPlace, onSessionEnd, openLocation, openPlace, sessionGuard, spaces } from "../store";
import { failure, statusOf } from "./discovery/access";
import { runSearch, searchQuery, searchScope } from "./discovery/searchState";
import { libKind, libQuery, libScope, openAsset, runLibrarySearch, searchSurface } from "./library/libraryState";

/** Where a quick lookup looks. `chat`: the personal Chat; `personal`: the personal library. */
export type QuickScope = { kind: "all" | "chat" | "personal" } | { kind: "space"; spaceId: string };
export type QuickMode = "all" | "conversations" | "library";
export type QuickMessage = Omit<SearchHit, "score">;
export type QuickAnswer = { assets: LibraryAsset[]; messages: QuickMessage[] };
type QuickApi = { quickSearch(query: string, scope: QuickScope, mode: QuickMode, kind?: MediaKind): Promise<QuickAnswer> };

export const QUICK_LIMIT = 8;
/** The longest query Metro looks up quickly, in Unicode characters (the server's limit; full search takes 1000). Longer ones send no quick lookup. */
export const QUICK_MAX_CHARS = 200;
/** The pause after the last keystroke before a lookup. */
export const QUICK_DEBOUNCE_MS = 220;

export type QuickContext = { mode: QuickMode; scope: QuickScope; kind: MediaKind | null };

/** The page's search: Library and Search keep their own mode and filters; everywhere else looks at everything. */
export const quickContext = computed<QuickContext>(() => {
  const place = mainPlace();
  const files = place === "library" || (place === "search" && searchSurface() === "library");
  if (files) {
    const s = libScope();
    return { mode: "library", scope: s.kind === "space" ? { kind: "space", spaceId: s.spaceId } : { kind: s.kind }, kind: libKind() };
  }
  if (place === "search") {
    const s = searchScope();
    return { mode: "conversations", scope: s.kind === "space" ? { kind: "space", spaceId: s.spaceId } : { kind: s.kind }, kind: null };
  }
  return { mode: "all", scope: { kind: "all" }, kind: null };
});
const contextKey = computed(() => JSON.stringify(quickContext()));
/** The Spaces the user is in now, as one value: an unchanged list (a poll) changes nothing. */
const memberKey = computed(() => spaces().map((s) => s.id).sort().join("\n"));

export type QuickStatus =
  | { status: "loading" }
  | { status: "ready"; query: string; assets: LibraryAsset[]; messages: QuickMessage[] }
  | { status: "error"; message: string }
  | { status: "too-long" }
  | { status: "unavailable" };

/** Shown under the box. */
export const quickOpen = signal(false);
export const quickState = signal<QuickStatus>({ status: "loading" });
/** The highlighted row: an index into `quickRows`, or -1 for none (Return then runs the full search). */
export const quickSelected = signal(-1);
/** The box has the keyboard focus. */
export const quickFocused = signal(false);

export type QuickRow = { kind: "asset"; asset: LibraryAsset } | { kind: "message"; message: QuickMessage } | { kind: "more" };

const joined = (spaceId: string | null) => spaceId === null || spaces().some((s) => s.id === spaceId);

/** The rows shown now: files, then messages, only from places the user can read now; More last. */
export const quickRows = computed<QuickRow[]>(() => {
  const state = quickState();
  const rows: QuickRow[] = [];
  if (state.status === "ready") {
    for (const asset of state.assets) if (joined(asset.spaceId)) rows.push({ kind: "asset", asset });
    for (const message of state.messages) if (joined(message.spaceId)) rows.push({ kind: "message", message });
  }
  return [...rows.slice(0, QUICK_LIMIT), { kind: "more" }];
});

let seq = 0;
let timer: ReturnType<typeof setTimeout> | undefined;

function stop() {
  seq++;
  if (timer) clearTimeout(timer);
  timer = undefined;
}

/** The query the box shows now (Documents & media or messages). */
export function boxQuery(): string {
  return mainPlace() === "library" || searchSurface() === "library" ? libQuery() : searchQuery();
}

function quickApi(): QuickApi | null | "missing" {
  const c = apiClient() as unknown as Partial<QuickApi> | null;
  if (!c) return null;
  return typeof c.quickSearch === "function" ? (c as QuickApi) : "missing";
}

/** The user edited the box: drop what's in flight, then look again after a pause. */
export function quickEdited(text: string) {
  stop();
  if (!text.trim() || !untracked(quickFocused)) {
    batch(() => {
      quickOpen.set(false);
      quickSelected.set(-1);
    });
    return;
  }
  const long = [...text.trim()].length > QUICK_MAX_CHARS; // Unicode characters, as the server counts
  batch(() => {
    quickOpen.set(true);
    quickSelected.set(-1);
    quickState.set(long ? { status: "too-long" } : { status: "loading" });
  });
  if (long) return;
  const mine = seq;
  timer = setTimeout(() => void lookup(text.trim(), mine), QUICK_DEBOUNCE_MS);
}

async function lookup(query: string, mine: number) {
  if (mine !== seq) return;
  const ctx = untracked(quickContext);
  const ok = sessionGuard();
  const current = () => ok() && mine === seq && untracked(contextKey) === JSON.stringify(ctx);
  const api = quickApi();
  if (!api) return;
  if (api === "missing") {
    quickState.set({ status: "unavailable" });
    return;
  }
  try {
    const answer = await api.quickSearch(query, ctx.scope, ctx.mode, ctx.kind ?? undefined);
    if (!current()) return;
    batch(() => {
      quickSelected.set(-1);
      quickState.set({ status: "ready", query, assets: answer.assets.slice(0, QUICK_LIMIT), messages: answer.messages.slice(0, QUICK_LIMIT) });
    });
  } catch (error) {
    if (!current()) return;
    const status = statusOf(error);
    if (status === 404 || status === 405 || status === 501) {
      quickState.set({ status: "unavailable" });
      return;
    }
    const message = failure(error, "Quick results failed");
    if (message) quickState.set({ status: "error", message });
  }
}

export function quickFocus(focused: boolean) {
  quickFocused.set(focused);
  if (!focused) quickClose();
}

/** Closes the list (Escape, a press outside, blur): what's in flight is dropped. The query stays. */
export function quickClose() {
  stop();
  batch(() => {
    quickOpen.set(false);
    quickSelected.set(-1);
  });
}

/** Up and Down: through the rows, More included, wrapping. */
export function quickMove(step: 1 | -1) {
  const n = quickRows().length;
  if (n === 0) return;
  const at = quickSelected();
  quickSelected.set(at < 0 ? (step > 0 ? 0 : n - 1) : (at + step + n) % n);
}

/** The full search, as Return did before: in the page's mode, with its own scope and filters. */
export function fullSearch() {
  quickClose();
  const files = mainPlace() === "library" || searchSurface() === "library";
  if (files) searchSurface.set("library");
  if (mainPlace() !== "search") openPlace("search");
  if (!boxQuery().trim()) return;
  if (files) void runLibrarySearch();
  else void runSearch();
}

/** More results: full Search with this query. A global lookup that found only files goes to Documents & media. */
export function quickMore() {
  const state = quickState();
  const query = boxQuery();
  const fileOnly = quickContext().mode === "all" && state.status === "ready" && state.assets.length > 0 && state.messages.length === 0;
  if (fileOnly && searchSurface() !== "library") {
    batch(() => {
      libQuery.set(query);
      searchSurface.set("library");
    });
  }
  fullSearch();
}

/** Opens a row: the message's conversation, the file in the Library viewer (which reads it with an access check), or More. */
export function quickActivate(row: QuickRow) {
  if (row.kind === "more") return quickMore();
  quickClose();
  if (row.kind === "message") {
    const m = row.message;
    if (!joined(m.spaceId)) return;
    void openLocation({ spaceId: m.spaceId, roomId: m.roomId, eventId: m.eventId });
    return;
  }
  if (!joined(row.asset.spaceId)) return;
  openPlace("library");
  openAsset(row.asset);
}

/** Return in the box: the highlighted row, or the full search when none is. */
export function quickSubmit() {
  const at = quickSelected();
  const row = quickOpen() && at >= 0 ? quickRows()[at] : undefined;
  if (row) quickActivate(row);
  else fullSearch();
}

// The place's mode or scope changed, or a Space was left, while the list is open: drop the answer and look again.
effect(() => {
  contextKey();
  memberKey();
  untracked(() => {
    if (!quickOpen()) return;
    quickEdited(boxQuery());
  });
});

onSessionEnd(() => {
  stop();
  batch(() => {
    quickOpen.set(false);
    quickFocused.set(false);
    quickSelected.set(-1);
    quickState.set({ status: "loading" });
  });
});
