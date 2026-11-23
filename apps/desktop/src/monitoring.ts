// Settings › Monitoring: state only, no UI.
//
// Recent monitoring events from exactly one explicit source at a time:
// - `device`: this computer's own Metro helper, through the authenticated
//   desktop bridge (only when the packaged launcher runs the app).
// - `me`: "My Chat", the signed-in user's own private personal Chat failures
//   from the connected server (`?scope=me`). The server resolves whose from
//   the session; no user id is sent, so nobody (an admin included) can read
//   another person's. It is a server source, never this computer's.
// - `space`: one Space the signed-in user owns, from the connected server
//   (`?space_id=SID`). Spaces they're only a member of are never offered.
// - `instance`: the connected server itself, offered to org admins only.
// There is no personal or all-Spaces aggregate, and events of two sources are
// never shown together: a result is tagged with its source (and, for a
// server source, the server and account it came from) and shown only while
// that is still the selected source on the same server and account. A source
// a server doesn't offer shows as unsupported; nothing falls back to another.
//
// Races: one sequence for every load. An answer is applied only if it is the
// latest load, the session (and, for a Space, its access epoch) is unchanged,
// and its source is still the one selected. Sign-out drops everything.

import { batch, computed, effect, signal, untracked } from "nsh";
import { ApiError } from "./api/client";
import type { MonitoringEvent, MonitoringList, ServerMonitoringScope } from "./api/monitoring";
import { BridgeError } from "./device/bridge";
import { desktopBridge } from "./device/state";
import { apiClient, connectedTo, disconnect, isAborted, me, onSessionEnd, orgAdmin, place, sessionGuard, settingsSection, spaces } from "./store";

export type { MonitoringEvent };
export { MAX_MONITORING_EVENTS } from "./api/monitoring";

export type MonitoringSource =
  | { kind: "device"; key: "device"; label: string }
  | { kind: "me"; key: "me"; label: string }
  | { kind: "space"; key: string; label: string; spaceId: string }
  | { kind: "instance"; key: "instance"; label: string };

export type MonitoringResult = {
  /** The source this answer belongs to. */
  key: string;
  /** For a server source, the server that answered; null for this computer. */
  server: string | null;
  /** For a server source, the account it answered; null for this computer. */
  account: string | null;
  phase: "loading" | "ready" | "unsupported" | "error";
  events: MonitoringEvent[];
  dropped: number;
  truncated: boolean;
  /** Unsupported or error: why, in words. */
  message: string | null;
};

export const spaceSourceKey = (spaceId: string) => `space:${spaceId}`;

/** The sources the signed-in user may monitor here, in a fixed order. */
export const monitoringSources = computed<MonitoringSource[]>(() => {
  const out: MonitoringSource[] = [];
  if (desktopBridge()) out.push({ kind: "device", key: "device", label: "This computer" });
  if (connectedTo() && me()) out.push({ kind: "me", key: "me", label: "My Chat" });
  for (const s of spaces()) if (s.role === "owner") out.push({ kind: "space", key: spaceSourceKey(s.id), label: s.name, spaceId: s.id });
  if (orgAdmin()) out.push({ kind: "instance", key: "instance", label: "This server (instance)" });
  return out;
});

/** What the user picked (a source key); null: the first source. */
const wanted = signal<string | null>(null);

/** The source shown: the one picked while it is still offered, else the first one. */
export const monitoringSource = computed<MonitoringSource | null>(() => {
  const list = monitoringSources();
  const w = wanted();
  return list.find((s) => s.key === w) ?? list[0] ?? null;
});

const result = signal<MonitoringResult | null>(null);

/** The answer for the selected source only, from the server connected now; never another source's. */
export const monitoringResult = computed<MonitoringResult | null>(() => {
  const r = result();
  const s = monitoringSource();
  if (!r || !s || r.key !== s.key) return null;
  if (s.kind !== "device" && (r.server !== connectedTo() || r.account !== me())) return null;
  return r;
});

let seq = 0;

export function selectMonitoringSource(key: string) {
  if (!monitoringSources().some((s) => s.key === key)) return;
  wanted.set(key);
}

/** Loads the selected source's events. Late or superseded answers are dropped. */
export async function loadMonitoring(): Promise<void> {
  const source = untracked(monitoringSource);
  const my = ++seq;
  if (!source) {
    result.set(null);
    return;
  }
  const server = source.kind === "device" ? null : untracked(connectedTo);
  const account = source.kind === "device" ? null : untracked(me);
  const ok = sessionGuard(source.kind === "space" ? source.spaceId : undefined);
  const current = () =>
    ok() &&
    my === seq &&
    untracked(monitoringSource)?.key === source.key &&
    (source.kind === "device" || (untracked(connectedTo) === server && untracked(me) === account));
  const previous = untracked(result);
  // A refresh of the same source keeps its rows until the answer; another source starts empty.
  const same = previous && previous.key === source.key && previous.server === server && previous.account === account;
  result.set({
    key: source.key,
    server,
    account,
    phase: "loading",
    events: same ? previous.events : [],
    dropped: same ? previous.dropped : 0,
    truncated: same ? previous.truncated : false,
    message: null,
  });
  try {
    let list: MonitoringList;
    if (source.kind === "device") {
      const bridge = desktopBridge();
      if (!bridge) throw new BridgeError("unreachable", "Metro's device helper isn't running with this window.");
      list = await bridge.monitoringEvents();
    } else {
      const client = apiClient();
      if (!client) return;
      const scope: ServerMonitoringScope =
        source.kind === "space" ? { kind: "space", spaceId: source.spaceId } : source.kind === "me" ? { kind: "me" } : { kind: "instance" };
      list = await client.monitoringEvents(scope);
    }
    if (!current()) return;
    result.set({ key: source.key, server, account, phase: "ready", events: list.events, dropped: list.dropped, truncated: list.truncated, message: null });
  } catch (error) {
    if (!current()) return;
    if (isAborted(error)) return;
    if (error instanceof ApiError && error.kind === "auth") {
      disconnect(error.message);
      return;
    }
    const { phase, message } = explain(source, server, error);
    result.set({ key: source.key, server, account, phase, events: [], dropped: 0, truncated: false, message });
  }
}

/** What a failure means for this source, in words. Older servers and helpers show as unsupported. */
export function explain(source: MonitoringSource, server: string | null, error: unknown): { phase: "unsupported" | "error"; message: string } {
  if (source.kind === "device") {
    if (error instanceof BridgeError) {
      if (error.status === 404 || error.status === 405 || error.status === 501 || error.code === "unsupported" || error.code === "not_found") {
        return { phase: "unsupported", message: "This computer's Metro helper doesn't report monitoring events. It may be an older version." };
      }
      if (error.code === "unauthorized") {
        return { phase: "error", message: "Metro's device helper didn't accept this window. Quit and reopen Metro, then try again." };
      }
      return { phase: "error", message: `Could not load this computer's events: ${error.message}` };
    }
    return { phase: "error", message: "Could not load this computer's events." };
  }
  const where = server ?? "The server";
  if (error instanceof ApiError) {
    // An older server: no route (404), or a query it doesn't know (400).
    if (source.kind === "me" && (error.status === 400 || error.status === 404 || error.status === 405 || error.status === 501)) {
      return { phase: "unsupported", message: `${where} doesn't report your Chat's monitoring events. It may run an older Metro version.` };
    }
    if (error.status === 404 || error.status === 405 || error.status === 501) {
      return {
        phase: "unsupported",
        message:
          source.kind === "space"
            ? `${where} doesn't report monitoring events for “${source.label}”. It may run an older Metro version, or the Space is no longer yours.`
            : `${where} doesn't report instance monitoring events. It may run an older Metro version.`,
      };
    }
    if (error.kind === "forbidden") {
      return {
        phase: "error",
        message: source.kind === "me"
          ? "The server refused to show your Chat's monitoring events."
          : source.kind === "space" ? `Only the owner of “${source.label}” can see its monitoring events.` : "Only organization admins can see this server's monitoring events.",
      };
    }
    return { phase: "error", message: `Could not load monitoring events: ${error.message}` };
  }
  return { phase: "error", message: "Could not load monitoring events." };
}

// Opening Monitoring, or picking another source while it's open, loads that
// source fresh. Leaving it lets any answer still on its way fall away.
// The key (a string) is tracked, not the source object: the Space list's
// periodic refresh must not reload an unchanged source.
const monitoringOpen = computed(() => place() === "settings" && settingsSection() === "monitoring");
const selectedKey = computed(() => monitoringSource()?.key ?? null);
effect(() => {
  const open = monitoringOpen();
  selectedKey();
  connectedTo();
  me();
  untracked(() => {
    if (open) void loadMonitoring();
    else seq++;
  });
});

onSessionEnd(() => {
  seq++;
  batch(() => {
    result.set(null);
    wanted.set(null);
  });
});
