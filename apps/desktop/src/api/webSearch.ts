import type { RequestFn } from "./client";

export const WEB_PROVIDERS = {
  exa: "Exa", parallel: "Parallel", tavily: "Tavily", firecrawl: "Firecrawl", tinyfish: "TinyFish",
} as const;
export type WebProvider = keyof typeof WEB_PROVIDERS;
export type WebMode = "key" | "keyless";
export const isWebProvider = (v: unknown): v is WebProvider =>
  typeof v === "string" && Object.prototype.hasOwnProperty.call(WEB_PROVIDERS, v);
export const isWebMode = (v: unknown): v is WebMode => v === "key" || v === "keyless";
export const WEB_OUTCOMES = {
  ok: "worked",
  limited: "reached its rate limit",
  out_of_credits: "ran out of credits or reached its plan limit",
  key_rejected: "rejected the key",
  blocked: "blocked requests from this network",
  unavailable: "unavailable",
  timeout: "timed out",
  bad_response: "sent an unusable answer",
  skipped_cooldown: "paused after an earlier attempt",
  skipped_no_key: "needs a free key",
  skipped_disabled: "off",
  credential_changed: "credential changed",
} as const;
export type WebOutcome = keyof typeof WEB_OUTCOMES;
export const isWebOutcome = (v: unknown): v is WebOutcome =>
  typeof v === "string" && Object.prototype.hasOwnProperty.call(WEB_OUTCOMES, v);

export const isWebRecord = (v: unknown): v is Record<string, unknown> =>
  v !== null && typeof v === "object" && !Array.isArray(v);
export function webTime(v: unknown): string | null {
  if (typeof v !== "string" || !/^\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}(?:\.\d+)?(?:Z|[+-]\d\d:\d\d)$/.test(v)) return null;
  const t = Date.parse(v);
  return Number.isFinite(t) ? new Date(t).toISOString() : null;
}

const failures = ["no_device", "offline", "exhausted", "busy", "timed_out", "revoked", "refused", "bad_response", "not_allowed", "invalid", "failed"] as const;
type Failure = typeof failures[number];
export type WebTool = "web_search" | "web_fetch";
type Attempt = { provider: WebProvider; mode: WebMode; outcome: WebOutcome; until: string | null; contacted: boolean | null };
export type WebToolNote = {
  name: WebTool;
  provider: WebProvider | null;
  mode: WebMode | null;
  attempts: Attempt[];
  failure: Failure | null;
  starterId: string | null;
};

export function parseWebTool(v: unknown): WebToolNote | undefined {
  if (!isWebRecord(v) || (v.name !== "web_search" && v.name !== "web_fetch")) return undefined;
  const attempts = (Array.isArray(v.attempts) ? v.attempts.slice(0, 10) : []).flatMap((a): Attempt[] =>
    isWebRecord(a) && isWebProvider(a.provider) && isWebMode(a.mode) && isWebOutcome(a.outcome)
      ? [{ provider: a.provider, mode: a.mode, outcome: a.outcome, until: webTime(a.until),
          contacted: a.outcome === "key_rejected" && typeof a.contacted === "boolean" ? a.contacted : null }]
      : [],
  );
  return {
    name: v.name,
    provider: isWebProvider(v.provider) ? v.provider : null,
    mode: isWebMode(v.mode) ? v.mode : null,
    attempts,
    // Unknown failure codes must never make the raw refusal text visible.
    failure: v.failure == null ? null : failures.find((f) => f === v.failure) ?? "failed",
    starterId: typeof v.starter_id === "string" && v.starter_id.trim() ? v.starter_id : null,
  };
}

export function webSearchPrefsApi(request: RequestFn) {
  const path = "/v1/web-search/prefs";
  const read = async (pending: Promise<unknown>) => {
    const result = await pending;
    if (!isWebRecord(result) || typeof result.personal_chat !== "boolean") return null;
    return result.personal_chat;
  };
  return {
    webSearchChatPref: () => read(request("GET", path)),
    setWebSearchChatPref: (enabled: boolean) => read(request("PUT", path, { personal_chat: enabled })),
  };
}
