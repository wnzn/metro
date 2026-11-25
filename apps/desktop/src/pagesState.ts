import { batch, computed, signal } from "nsh";
import type { PageMeta } from "./api/pages";
import { ApiError, apiClient, describe, disconnect, isAborted, mainPlace, onSessionEnd, openPlace, sessionGuard, spaces } from "./store";

export type PagesRoute =
  | { kind: "home"; spaceId?: string }
  | { kind: "page"; id: string; spaceId?: string }
  | { kind: "draft"; id: string; spaceId?: string; returnPage?: string };

const route = signal<PagesRoute>({ kind: "home" });
const entries = signal<{ page: PageMeta; valid?: () => boolean }[]>([]);
export const pagesRoute = computed(route);
export const pages = computed(() => entries().filter(e => scopeVisible(e.page) && (e.valid?.() ?? true)).map(e => e.page));
export const pagesError = signal("");
export const pagesLoading = signal(false);
let listRequest = 0;

export function hasSpace(id?: string | null) {
  return !id || spaces().some(s => s.id === id);
}

export function scopeVisible(page: { scope: string; space_id: string | null }) {
  return page?.scope === "personal" ? page.space_id === null
    : page?.scope === "space" && !!page.space_id && hasSpace(page.space_id);
}

export function goPages(next: PagesRoute = { kind: "home" }) {
  if (!apiClient() || !hasSpace(next.spaceId)) return;
  batch(() => {
    route.set(next);
    openPlace("pages");
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
  const grants = new Map(spaces().map(s => [s.id, sessionGuard(s.id)]));
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
  pagesError.set("");
  pagesLoading.set(false);
  route.set({ kind: "home" });
});
