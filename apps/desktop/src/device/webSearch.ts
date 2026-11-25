import { isWebOutcome, isWebProvider, isWebRecord, WEB_PROVIDERS, webTime } from "../api/webSearch";
import type { WebOutcome, WebProvider } from "../api/webSearch";
import type { Worker } from "./bridge";

export type WebProviderRow = {
  id: WebProvider;
  enabled: boolean;
  keyless: boolean;
  keySaved: boolean;
  keyRejected: boolean;
  cooldown: { until: string; outcome: WebOutcome } | null;
  test: WebOutcome | null;
};

export type LocalWebSearch = {
  enabled: boolean;
  noticeAcknowledged: boolean;
  providers: WebProviderRow[];
  worker: Worker;
  paired: boolean;
  serverUnsupported: boolean;
  offNotReported: boolean;
  settingsError: "preferences" | "pauses" | "unknown" | null;
  keysError: boolean;
  endpointsError: boolean;
};

export type WebUpdate = {
  enabled?: boolean;
  notice_acknowledged?: boolean;
  providers?: { id: WebProvider; enabled: boolean }[];
};

export function parseWebProvider(v: unknown): WebProviderRow | null {
  if (!isWebRecord(v) || !isWebProvider(v.id) || typeof v.enabled !== "boolean" ||
      typeof v.keyless !== "boolean" || typeof v.key_saved !== "boolean") return null;
  const until = isWebRecord(v.cooldown) ? webTime(v.cooldown.until) : null;
  return {
    id: v.id, enabled: v.enabled, keyless: v.keyless, keySaved: v.key_saved,
    keyRejected: v.key_status === "rejected",
    cooldown: until && isWebRecord(v.cooldown) && isWebOutcome(v.cooldown.outcome) ? { until, outcome: v.cooldown.outcome } : null,
    test: isWebRecord(v.test) && isWebOutcome(v.test.outcome) ? v.test.outcome : null,
  };
}

export function parseLocalWebSearch(v: unknown): LocalWebSearch | null {
  if (!isWebRecord(v) || typeof v.enabled !== "boolean" || typeof v.notice_acknowledged !== "boolean" ||
      !isWebRecord(v.device) || typeof v.device.paired !== "boolean" || !Array.isArray(v.providers)) return null;
  const providers = v.providers.map(parseWebProvider);
  if (providers.some((row) => !row) || new Set(providers.map((row) => row?.id)).size !== Object.keys(WEB_PROVIDERS).length ||
      providers.length !== Object.keys(WEB_PROVIDERS).length) return null;
  const { device } = v;
  const worker = (["stopped", "needs_setup", "starting", "online", "offline", "unauthorized", "error"] as const).find((w) => w === device.worker) ?? "error";
  // v1 sends fixed text, not structured recovery codes. Recognize its wording
  // without copying any bridge diagnostics into settings state or the UI.
  const settings = v.settings_error;
  const settingsError = settings == null ? null :
    typeof settings === "string" && settings.startsWith("Your web search settings on this computer can't be read") ? "preferences" :
    typeof settings === "string" && settings.startsWith("Saved provider pauses on this computer can't be read") ? "pauses" : "unknown";
  return {
    enabled: v.enabled, noticeAcknowledged: v.notice_acknowledged, providers: providers as WebProviderRow[],
    worker, paired: v.device.paired, settingsError, keysError: v.keys_error != null, endpointsError: v.endpoints_error != null,
    serverUnsupported: device.detail === "Couldn't set up web search with Metro: This Metro server doesn't offer web search yet",
    offNotReported: typeof v.device.detail === "string" && v.device.detail.startsWith("Turned off here, but Metro couldn't be told"),
  };
}
