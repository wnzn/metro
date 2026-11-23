// Type-safe wrappers for semantic search, quick (literal) search and
// scheduled prompts, built on the same `request` function as the rest of the
// client (createClient spreads `...discoveryApi(request)` into its result).
// No state, no timers, no caching, no UI. Contracts:
// crates/server/src/search/API.md, crates/server/src/quick_search.rs and
// crates/server/src/schedules.rs.

import { ApiError } from "./client";
import type { RequestFn } from "./client";
import { toAsset } from "./library";
import type { LibraryAsset, MediaKind } from "./library";

export type DiscoveryRequest = RequestFn;

// ------------------------------------------------------------------ search

/** `chat`: only the caller's personal Chat; `space`: one Space they belong to; `all`: both. */
export type SearchScope = { kind: "all" } | { kind: "chat" } | { kind: "space"; spaceId: string };

/**
 * `classifier`: every result was scored by a System One call. `embeddings`:
 * none was (retrieval order only). `mixed`: the top shortlist was reranked
 * by System One, the rest follow in retrieval order.
 */
export type SearchRanking = "classifier" | "embeddings" | "mixed";

/** One hit. `spaceId` and `roomId` are null for the personal Chat. */
export type SearchHit = { spaceId: string | null; roomId: string | null; eventId: number; actor: string; text: string; score: number };

export type SearchResponse = { results: SearchHit[]; ranking: SearchRanking; partial: boolean; notice: string | null };

export type IndexProgress = { indexed: number; skipped: number; remaining: number; done: boolean; notice: string | null };

// ------------------------------------------------------------------ quick search

/**
 * Where a quick lookup looks; it never widens. `chat`: the personal Chat's
 * messages only. `personal`: the personal library's files only. `space`: one
 * Space's rooms, threads and library (the caller must be a member now).
 * `all`: all of these.
 */
export type QuickSearchScope = { kind: "all" } | { kind: "chat" } | { kind: "personal" } | { kind: "space"; spaceId: string };

/** `conversations`: messages only; `library`: files only; `all`: both. */
export type QuickSearchMode = "all" | "conversations" | "library";

/** A message match: a `SearchHit` without a score. `text` is a snippet around the match. */
export type QuickSearchMessage = Omit<SearchHit, "score">;

/** At most 8 results together, newest first; shared between files and messages when both match. */
export type QuickSearchResponse = { assets: LibraryAsset[]; messages: QuickSearchMessage[] };

// ------------------------------------------------------------------ schedules

export type ScheduleSpec = { kind: "once"; at: string } | { kind: "cron"; expression: string; timezone: string };

/** Where a task's prompt goes: the owner's personal Chat, or a room (or thread) of a Space. */
export type ScheduleTarget = { kind: "chat" } | { kind: "room"; spaceId: string; roomId: string };

/**
 * A scheduled prompt (GET /v1/schedules). Times are RFC 3339: `nextRunAt` in
 * UTC, `nextRunLocal` (cron only) in the task's own zone.
 */
export type ScheduledTask = {
  id: string;
  name: string;
  prompt: string;
  target: ScheduleTarget;
  schedule: ScheduleSpec;
  enabled: boolean;
  nextRunAt: string | null;
  nextRunLocal: string | null;
  lastStatus: string | null;
  lastRunAt: string | null;
  lastDetail: string | null;
};

/**
 * One occurrence (GET /v1/schedules/{id}/history). Statuses the server
 * records: dispatching, dispatched, error, unknown, missed, cancelled,
 * revoked, interrupted. `dispatched` means the prompt was handed to the
 * agent, not that its work finished.
 */
export type ScheduleRun = { id: number; dueAt: string; status: string; detail: string | null; claimedAt: string | null; finishedAt: string | null };

export type ScheduleInput = { name: string; prompt: string; target: ScheduleTarget; schedule: ScheduleSpec; enabled: boolean };

// ------------------------------------------------------------------ the wrappers

export function discoveryApi(request: DiscoveryRequest) {
  const task = (id: string) => `/v1/schedules/${encodeURIComponent(id)}`;

  return {
    /** POST /v1/search: membership-scoped semantic search over Chat and Spaces. */
    async search(query: string, scope: SearchScope, limit?: number): Promise<SearchResponse> {
      const body: Record<string, unknown> = { query, ...scopeBody(scope) };
      if (limit !== undefined) body.limit = limit;
      const data = await request("POST", "/v1/search", body);
      if (!isObject(data) || !Array.isArray(data.results) || !isRanking(data.ranking) || typeof data.partial !== "boolean") {
        throw new ApiError("protocol", "POST /v1/search did not return { results, ranking, partial }.");
      }
      return {
        results: data.results.filter(isHit).map(toHit),
        ranking: data.ranking,
        partial: data.partial,
        notice: text(data.notice),
      };
    },

    /**
     * POST /v1/search/quick: a literal, model-free lookup (typeahead) of
     * message text and file names the caller may read now. `kind` narrows
     * files and is not allowed with `conversations`.
     */
    async quickSearch(query: string, scope: QuickSearchScope, mode: QuickSearchMode, kind?: MediaKind): Promise<QuickSearchResponse> {
      const body: Record<string, unknown> = { query, scope: scope.kind, mode };
      if (scope.kind === "space") body.space_id = scope.spaceId;
      if (kind !== undefined) body.kind = kind;
      const data = await request("POST", "/v1/search/quick", body);
      if (!isObject(data) || !Array.isArray(data.assets) || !Array.isArray(data.messages)) {
        throw new ApiError("protocol", "POST /v1/search/quick did not return { assets, messages }.");
      }
      return {
        assets: data.assets.map(toAsset).filter((a): a is LibraryAsset => a !== null),
        messages: data.messages.filter(isHit).map(toQuickMessage),
      };
    },

    /** POST /v1/search/index: indexes the next bounded batch for this scope. */
    async indexSearch(scope: SearchScope): Promise<IndexProgress> {
      const data = await request("POST", "/v1/search/index", scopeBody(scope));
      if (!isObject(data) || !isCount(data.indexed) || !isCount(data.remaining) || typeof data.done !== "boolean") {
        throw new ApiError("protocol", "POST /v1/search/index did not return { indexed, remaining, done }.");
      }
      return { indexed: data.indexed, skipped: isCount(data.skipped) ? data.skipped : 0, remaining: data.remaining, done: data.done, notice: text(data.notice) };
    },

    async listSchedules(): Promise<ScheduledTask[]> {
      const data = await request("GET", "/v1/schedules");
      const tasks = isObject(data) ? data.tasks : undefined;
      if (!Array.isArray(tasks)) throw new ApiError("protocol", "GET /v1/schedules did not return { tasks: [...] }.");
      return tasks.map(toTask).filter((t): t is ScheduledTask => t !== null);
    },

    async createSchedule(input: ScheduleInput): Promise<ScheduledTask> {
      const data = await request("POST", "/v1/schedules", scheduleBody(input));
      const created = toTask(data);
      if (!created) throw new ApiError("protocol", "POST /v1/schedules did not return the task.");
      return created;
    },

    /** PATCH /v1/schedules/{id}. Send only what changes; `enabled` alone pauses or resumes. */
    async updateSchedule(id: string, patch: Partial<ScheduleInput>): Promise<ScheduledTask> {
      const body: Record<string, unknown> = {};
      if (patch.name !== undefined) body.name = patch.name;
      if (patch.prompt !== undefined) body.prompt = patch.prompt;
      if (patch.target !== undefined) Object.assign(body, targetBody(patch.target));
      if (patch.schedule !== undefined) body.schedule = patch.schedule;
      if (patch.enabled !== undefined) body.enabled = patch.enabled;
      const data = await request("PATCH", task(id), body);
      const updated = toTask(data);
      if (!updated) throw new ApiError("protocol", "PATCH /v1/schedules/… did not return the task.");
      return updated;
    },

    async deleteSchedule(id: string): Promise<void> {
      await request("DELETE", task(id));
    },

    async scheduleHistory(id: string): Promise<ScheduleRun[]> {
      const data = await request("GET", `${task(id)}/history`);
      const runs = isObject(data) ? data.runs : undefined;
      if (!Array.isArray(runs)) throw new ApiError("protocol", "GET /v1/schedules/…/history did not return { runs: [...] }.");
      return runs.map(toRun).filter((r): r is ScheduleRun => r !== null);
    },
  };
}

export type DiscoveryApi = ReturnType<typeof discoveryApi>;

// ------------------------------------------------------------------ wire helpers

function scopeBody(scope: SearchScope): Record<string, unknown> {
  return scope.kind === "space" ? { scope: "space", space_id: scope.spaceId } : { scope: scope.kind };
}

function targetBody(target: ScheduleTarget): Record<string, unknown> {
  return target.kind === "chat" ? { space_id: null, room_id: null } : { space_id: target.spaceId, room_id: target.roomId };
}

function scheduleBody(input: ScheduleInput): Record<string, unknown> {
  return { name: input.name, prompt: input.prompt, ...targetBody(input.target), schedule: input.schedule, enabled: input.enabled };
}

function isObject(v: unknown): v is Record<string, unknown> {
  return typeof v === "object" && v !== null && !Array.isArray(v);
}

const isId = (v: unknown): v is string | number => (typeof v === "string" && v !== "") || (typeof v === "number" && Number.isFinite(v));
const isCount = (v: unknown): v is number => typeof v === "number" && Number.isFinite(v) && v >= 0;
const isRanking = (v: unknown): v is SearchRanking => v === "classifier" || v === "embeddings" || v === "mixed";
const text = (v: unknown): string | null => (typeof v === "string" && v.trim() ? v : null);

function isHit(v: unknown): v is Record<string, unknown> & { event_id: number; actor: string; text: string } {
  if (!isObject(v) || typeof v.event_id !== "number" || !Number.isFinite(v.event_id) || typeof v.actor !== "string" || typeof v.text !== "string") return false;
  // Chat hits have neither id; room hits have both. Anything else has no place to open.
  const chat = v.space_id == null && v.room_id == null;
  return chat || (isId(v.space_id) && isId(v.room_id));
}

function toHit(v: Record<string, unknown> & { event_id: number; actor: string; text: string }): SearchHit {
  const score = typeof v.score === "number" && Number.isFinite(v.score) ? Math.min(1, Math.max(0, v.score)) : 0;
  return {
    spaceId: v.space_id == null ? null : String(v.space_id),
    roomId: v.room_id == null ? null : String(v.room_id),
    eventId: v.event_id,
    actor: v.actor,
    text: v.text,
    score,
  };
}

function toQuickMessage(v: Record<string, unknown> & { event_id: number; actor: string; text: string }): QuickSearchMessage {
  const { score: _score, ...hit } = toHit(v);
  return hit;
}

function toSpec(v: unknown): ScheduleSpec | null {
  if (!isObject(v)) return null;
  if (v.kind === "once" && typeof v.at === "string") return { kind: "once", at: v.at };
  if (v.kind === "cron" && typeof v.expression === "string" && typeof v.timezone === "string") return { kind: "cron", expression: v.expression, timezone: v.timezone };
  return null;
}

function toTask(v: unknown): ScheduledTask | null {
  if (!isObject(v) || !isId(v.id) || typeof v.name !== "string" || typeof v.prompt !== "string" || typeof v.enabled !== "boolean") return null;
  const schedule = toSpec(v.schedule);
  if (!schedule) return null;
  let target: ScheduleTarget;
  if (v.space_id == null && v.room_id == null) target = { kind: "chat" };
  else if (isId(v.space_id) && isId(v.room_id)) target = { kind: "room", spaceId: String(v.space_id), roomId: String(v.room_id) };
  else return null;
  return {
    id: String(v.id),
    name: v.name,
    prompt: v.prompt,
    target,
    schedule,
    enabled: v.enabled,
    nextRunAt: text(v.next_run_at),
    nextRunLocal: text(v.next_run_local),
    lastStatus: text(v.last_status),
    lastRunAt: text(v.last_run_at),
    lastDetail: text(v.last_detail),
  };
}

function toRun(v: unknown): ScheduleRun | null {
  if (!isObject(v) || typeof v.id !== "number" || typeof v.due_at !== "string" || typeof v.status !== "string") return null;
  return { id: v.id, dueAt: v.due_at, status: v.status, detail: text(v.detail), claimedAt: text(v.claimed_at), finishedAt: text(v.finished_at) };
}
