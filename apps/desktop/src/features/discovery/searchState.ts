// Semantic search state: the query, its scope, the last answer and indexing
// progress. Only Spaces the user is a member of now can be chosen or shown;
// everything is dropped when the session ends.

import { batch, computed, effect, signal, untracked } from "nsh";
import type { IndexProgress, SearchHit, SearchRanking, SearchScope } from "../../api/discovery";
import { onSessionEnd, openLocation, sessionGuard, spaces } from "../../store";
import { discovery, failure, serverDetail, statusOf } from "./access";

export type { SearchHit, SearchRanking, SearchScope };

export const SEARCH_LIMIT = 20;
export const MAX_QUERY_CHARS = 1000;

export const searchQuery = signal("");
export const searchScope = signal<SearchScope>({ kind: "all" });

/** The last answer, with the query and scope it answered. */
export type SearchAnswer = { query: string; scope: SearchScope; results: SearchHit[]; ranking: SearchRanking; partial: boolean; notice: string | null };
export const searchAnswer = signal<SearchAnswer | null>(null);
export const searching = signal(false);
export const searchError = signal<{ message: string; needsEmbeddings: boolean } | null>(null);

export const indexing = signal(false);
export const indexProgress = signal<IndexProgress | null>(null);
export const indexError = signal<string | null>(null);
/** Set when a result's place could not be opened. */
export const openError = signal<string | null>(null);

let seq = 0;

/** Results from Spaces the user is still a member of (or their Chat). Anything else is never shown. */
export const visibleHits = computed(() => {
  const answer = searchAnswer();
  if (!answer) return [];
  const joined = new Set(spaces().map((s) => s.id));
  return answer.results.filter((hit) => hit.spaceId === null || joined.has(hit.spaceId));
});
/** Results the server returned from a Space the user is no longer a member of. */
export const hiddenHits = computed(() => (searchAnswer()?.results.length ?? 0) - visibleHits().length);

/**
 * A readable result with no measurable embedding similarity: a score of 0
 * in an answer ordered by embeddings only. In a System One (or mixed) answer
 * a score may be System One's, so nothing is folded there.
 */
export const isLessRelevant = (hit: SearchHit, ranking: SearchRanking) => ranking === "embeddings" && !(hit.score > 0);
const rankingNow = () => searchAnswer()?.ranking ?? "embeddings";
/** Readable results shown at once. */
export const relevantHits = computed(() => visibleHits().filter((h) => !isLessRelevant(h, rankingNow())));
/** Readable results folded under "Less relevant", closed until the user opens it. */
export const lessRelevantHits = computed(() => visibleHits().filter((h) => isLessRelevant(h, rankingNow())));
export const lessRelevantOpen = signal(false);

export function scopeLabel(scope: SearchScope): string {
  if (scope.kind === "all") return "Everything you can read";
  if (scope.kind === "chat") return "Your personal Chat";
  return spaceName(scope.spaceId);
}

export const spaceName = (spaceId: string) => spaces().find((s) => s.id === spaceId)?.name ?? "a Space";

const STOP_WORDS = new Set(["the", "and", "for", "with", "what", "where", "which", "who", "how", "has", "have", "was", "are", "about", "from", "that", "this", "into", "does", "did", "our", "your", "you"]);

/**
 * `text` cut into parts, the words of `query` (three letters or more, not
 * filler words) marked wherever they appear, case-insensitively. Only the
 * literal words are marked: a result found by meaning may mark nothing.
 */
export function markTerms(text: string, query: string): { text: string; mark: boolean }[] {
  const terms = [...new Set(query.toLowerCase().match(/[\p{L}\p{N}]{3,}/gu) ?? [])].filter((t) => !STOP_WORDS.has(t)).slice(0, 12);
  if (!text || terms.length === 0) return [{ text, mark: false }];
  const escaped = terms.sort((a, b) => b.length - a.length).map((t) => t.replace(/[.*+?^${}()|[\]\\]/g, "\\$&"));
  const parts: { text: string; mark: boolean }[] = [];
  const re = new RegExp(escaped.join("|"), "giu");
  let at = 0;
  for (const m of text.matchAll(re)) {
    const i = m.index ?? 0;
    if (i > at) parts.push({ text: text.slice(at, i), mark: false });
    parts.push({ text: m[0], mark: true });
    at = i + m[0].length;
  }
  if (at < text.length) parts.push({ text: text.slice(at), mark: false });
  return parts.length ? parts : [{ text, mark: false }];
}

export function setScope(scope: SearchScope) {
  if (scope.kind === "space" && !spaces().some((s) => s.id === scope.spaceId)) return;
  batch(() => {
    searchScope.set(scope);
    indexProgress.set(null);
    indexError.set(null);
  });
}

export async function runSearch(): Promise<void> {
  const query = searchQuery().trim();
  if (!query) {
    searchError.set({ message: "Type what you're looking for.", needsEmbeddings: false });
    return;
  }
  if (query.length > MAX_QUERY_CHARS) {
    searchError.set({ message: `Searches are at most ${MAX_QUERY_CHARS} characters.`, needsEmbeddings: false });
    return;
  }
  const scope = searchScope();
  const ok = sessionGuard();
  const mine = ++seq;
  batch(() => {
    searching.set(true);
    searchError.set(null);
    openError.set(null);
  });
  try {
    const api = discovery();
    if (!api) return;
    const answer = await api.search(query, scope, SEARCH_LIMIT);
    if (!ok() || mine !== seq) return;
    batch(() => {
      lessRelevantOpen.set(false);
      searchAnswer.set({ query, scope, ...answer });
    });
  } catch (error) {
    if (!ok() || mine !== seq) return;
    const status = statusOf(error);
    const detail = serverDetail(error);
    if (status === 404 && scope.kind === "space") {
      batch(() => {
        searchScope.set({ kind: "all" });
        searchAnswer.set(null);
        searchError.set({ message: "You're not a member of that Space (any more), so it can't be searched. The scope is back to everything you can read.", needsEmbeddings: false });
      });
      return;
    }
    if (status === 409) {
      searchError.set({ message: `Search needs an embedding model. Choose one in Settings › Model choices.${detail ? ` ${detail}` : ""}`, needsEmbeddings: true });
      return;
    }
    if (status === 502) {
      searchError.set({ message: `The embedding model couldn't be reached, so nothing was searched${detail ? `: ${detail}` : ""}. Try again later.`, needsEmbeddings: false });
      return;
    }
    if (status === 503 || status === 429) {
      searchError.set({ message: `Search is busy${detail ? ` (${detail})` : ""}. Try again in a moment.`, needsEmbeddings: false });
      return;
    }
    const message = failure(error, "Search failed");
    if (message) searchError.set({ message, needsEmbeddings: false });
  } finally {
    if (ok() && mine === seq) searching.set(false);
  }
}

/** Indexes the next batch for the current scope (when results say indexing is incomplete). */
export async function indexMore(): Promise<void> {
  if (indexing()) return;
  const scope = searchScope();
  const ok = sessionGuard();
  const mine = seq;
  batch(() => {
    indexing.set(true);
    indexError.set(null);
  });
  try {
    const api = discovery();
    if (!api) return;
    const progress = await api.indexSearch(scope);
    if (!ok() || mine !== seq) return;
    indexProgress.set(progress);
  } catch (error) {
    if (!ok() || mine !== seq) return;
    if (statusOf(error) === 404) {
      indexError.set("You're not a member of that Space (any more).");
      return;
    }
    const message = failure(error, "Indexing failed");
    if (message) indexError.set(message);
  } finally {
    if (ok() && mine === seq) indexing.set(false);
  }
}

/** Opens the conversation a result came from (the personal Chat, or its room or thread) at that message. */
export async function openHit(hit: SearchHit): Promise<void> {
  openError.set(null);
  if (hit.spaceId !== null && !spaces().some((s) => s.id === hit.spaceId)) {
    openError.set("That result is from a Space you're no longer a member of.");
    return;
  }
  await openLocation({ spaceId: hit.spaceId, roomId: hit.roomId, eventId: hit.eventId });
}

// A Space the user left can't stay the scope.
effect(() => {
  const joined = spaces();
  untracked(() => {
    const scope = searchScope();
    if (scope.kind === "space" && !joined.some((s) => s.id === scope.spaceId)) {
      batch(() => {
        searchScope.set({ kind: "all" });
        indexProgress.set(null);
      });
    }
  });
});

onSessionEnd(() => {
  seq++;
  batch(() => {
    searchQuery.set("");
    searchScope.set({ kind: "all" });
    searchAnswer.set(null);
    lessRelevantOpen.set(false);
    searching.set(false);
    searchError.set(null);
    indexing.set(false);
    indexProgress.set(null);
    indexError.set(null);
    openError.set(null);
  });
});
