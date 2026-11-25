import { batch, computed, effect, signal, untracked } from "nsh";
import type { TurnModelChoice } from "./api/client";
import { isPageStatus } from "./api/pages";
import type { DraftMeta, PageMeta } from "./api/pages";
import { ApiError, apiClient, describe, disconnect, isAborted, mainPlace, onSessionEnd, openPlace, sessionGuard, spaces } from "./store";

export type PagesRoute =
  | { kind: "home"; spaceId?: string; afterDelete?: boolean }
  | { kind: "page"; id: string; spaceId?: string }
  | { kind: "draft"; id: string; spaceId?: string; returnPage?: string; returnRoom?: string };

const route = signal<PagesRoute>({ kind: "home" });
/** A primitive key for NSH's keyed view, not a new object on each evaluation. */
export const pagesRouteMount = signal(0);
const entries = signal<{ page: PageMeta; valid?: () => boolean }[]>([]);
const draftEntries = signal<{ draft: DraftMeta; valid: () => boolean }[]>([]);
// Session-only overlays; neither opening a draft nor remembering a model adds a list row.
const unavailableDrafts = signal<ReadonlySet<string>>(new Set());
const draftModels = new Map<string, TurnModelChoice>();
export const pageDraftUnavailable = (id: string) => unavailableDrafts().has(id);
export const pageDraftModel = (id: string) => draftModels.get(id);
export function rememberPageDraftModel(id: string, choice?: TurnModelChoice) {
  if (choice) draftModels.set(id, choice);
}
export const pagesRoute = computed(route);
export const pages = computed(() => entries().filter(e => scopeVisible(e.page) && (e.valid?.() ?? true)).map(e => e.page));
/** Metadata only. Review always loads the draft again by ID. */
export const pageDrafts = computed(() => draftEntries()
  .filter(e => !pageDraftUnavailable(e.draft.id) && scopeVisible(e.draft) && e.valid()).map(e => e.draft));
export const pageDraftsError = signal("");
export const pagesError = signal("");
export const pagesLoading = signal(false);
let listRequest = 0, draftsRequest = 0;

export function hasSpace(id?: string | null) {
  return !id || spaces().some(s => s.id === id);
}

export function spaceGrants() {
  return new Map(spaces().map(s => [s.id, sessionGuard(s.id)]));
}

export function sameSpaceGrants(previous: ReturnType<typeof spaceGrants>, current: ReturnType<typeof spaceGrants>) {
  return previous.size === current.size && [...previous].every(([id, valid]) => current.has(id) && valid());
}

/** The Space list gets fresh objects even when membership didn't change.
 * Forms react to their selected Space and grant, not to list identity. */
export function watchPageScope(scope: () => string | null, changed: (id: string | null, visible: boolean, switchScope: boolean) => void) {
  let previous: { id: string | null; visible: boolean; grant: () => boolean } | undefined;
  effect(() => {
    const id = scope(), visible = hasSpace(id);
    const switchScope = previous?.id !== id;
    if (previous && !switchScope && visible === previous.visible && previous.grant()) return;
    previous = { id, visible, grant: sessionGuard(id ?? undefined) };
    untracked(() => changed(id, visible, switchScope));
  });
}

export function scopeVisible(page: { scope: string; space_id: string | null }) {
  return page?.scope === "personal" ? page.space_id === null
    : page?.scope === "space" && !!page.space_id && hasSpace(page.space_id);
}

export function goPages(next: PagesRoute = { kind: "home" }) {
  if (!apiClient() || !hasSpace(next.spaceId)) return;
  batch(() => {
    route.set(next);
    pagesRouteMount.set(pagesRouteMount() + 1);
    // Keep mutation navigation guards in step even for Page A → B → A.
    openPlace("pages", { subrouteChanged: true });
  });
}

export function forgetPageDraft(id: string) {
  draftsRequest++;
  draftModels.delete(id);
  batch(() => {
    if (!pageDraftUnavailable(id)) unavailableDrafts.set(new Set(unavailableDrafts()).add(id));
    draftEntries.set(draftEntries().filter(entry => entry.draft.id !== id));
  });
}

export function pageGuard(current: PagesRoute, spaceId = current.spaceId) {
  const currentSession = sessionGuard(spaceId);
  return () => currentSession() && mainPlace() === "pages" && route() === current && hasSpace(spaceId);
}

export function pagesProblem(error: unknown) {
  if (error instanceof ApiError && error.kind === "auth") {
    disconnect(error.message);
    return "";
  }
  return isAborted(error) ? "" : describe(error, "Couldn't complete this Pages request");
}

export async function refreshPages() {
  const client = apiClient();
  if (!client) return;
  const index = ++listRequest;
  const currentSession = sessionGuard();
  const valid = () => currentSession() && index === listRequest;
  // Do not accept entries from a Space the user joined again while this
  // response was in flight; its epoch no longer matches.
  const grants = spaceGrants();
  pagesLoading.set(true);
  pagesError.set("");
  try {
    const list = await client.listPages();
    if (valid()) entries.set(list.filter(p => scopeVisible(p) && (!p.space_id || grants.get(p.space_id)?.()))
      .map(page => ({ page, valid: page.space_id ? grants.get(page.space_id) : undefined })));
  } catch (error) {
    if (valid()) {
      entries.set([]);
      pagesError.set(pagesProblem(error));
    }
  } finally {
    if (valid()) pagesLoading.set(false);
  }
}

export async function refreshPageDrafts(current: () => boolean) {
  const client = apiClient();
  if (!client) return;
  const index = ++draftsRequest;
  const session = sessionGuard(), grants = spaceGrants();
  const valid = () => current() && session() && index === draftsRequest;
  pageDraftsError.set("");
  try {
    const list = await client.listPageDrafts();
    if (valid()) draftEntries.set(list.flatMap(draft => {
      const access = draft.space_id ? grants.get(draft.space_id) : session;
      return scopeVisible(draft) && access?.() ? [{ draft, valid: access }] : [];
    }));
  } catch (error) {
    if (!valid()) return;
    draftEntries.set([]);
    // Older servers only support POST at this path.
    if (!isPageStatus(error, 404) && !isPageStatus(error, 405)) pageDraftsError.set(pagesProblem(error));
  }
}

onSessionEnd(() => {
  listRequest++;
  draftsRequest++;
  entries.set([]);
  unavailableDrafts.set(new Set());
  draftModels.clear();
  draftEntries.set([]);
  pageDraftsError.set("");
  pagesError.set("");
  pagesLoading.set(false);
  route.set({ kind: "home" });
  pagesRouteMount.set(pagesRouteMount() + 1);
});
