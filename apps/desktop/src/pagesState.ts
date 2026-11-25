import { batch, computed, effect, signal, untracked } from "nsh";
import type { PageMeta } from "./api/pages";
import { ApiError, apiClient, describe, disconnect, isAborted, mainPlace, onSessionEnd, openPlace, sessionGuard, spaces } from "./store";

export type PagesRoute =
  | { kind: "home"; spaceId?: string }
  | { kind: "page"; id: string; spaceId?: string }
  | { kind: "draft"; id: string; spaceId?: string; returnPage?: string; returnRoom?: string };

const route = signal<PagesRoute>({ kind: "home" });
const entries = signal<{ page: PageMeta; valid?: () => boolean }[]>([]);
type DraftRoute = Extract<PagesRoute, { kind: "draft" }>;
const draftLinks = signal<{ route: DraftRoute; valid: () => boolean }[]>([]);
export const pagesRoute = computed(route);
export const pages = computed(() => entries().filter(e => scopeVisible(e.page) && (e.valid?.() ?? true)).map(e => e.page));
/** Session-only handles; no draft source, prompt, or preview is cached here. */
export const recentPageDrafts = computed(() => draftLinks().filter(entry => hasSpace(entry.route.spaceId) && entry.valid()).map(entry => entry.route));
export const pagesError = signal("");
export const pagesLoading = signal(false);
let listRequest = 0;

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
    if (next.kind === "draft") draftLinks.set([{ route: { ...next }, valid: sessionGuard(next.spaceId) }, ...draftLinks().filter(entry => entry.route.id !== next.id && entry.valid())].slice(0, 20));
    route.set(next);
    openPlace("pages");
  });
}

export function forgetPageDraft(id: string) {
  draftLinks.set(draftLinks().filter(entry => entry.route.id !== id));
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

onSessionEnd(() => {
  listRequest++;
  entries.set([]);
  draftLinks.set([]);
  pagesError.set("");
  pagesLoading.set(false);
  route.set({ kind: "home" });
});
