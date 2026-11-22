// One collapsed disclosure per agent turn. Events that carry the same
// `activity` metadata (lifecycle `kind: "activity"`, tool calls and results,
// provider-shared summaries) fold into a single group, placed where its first
// event is. A later step joins that group in place, so nothing else in the
// transcript moves and the group's key (and open state) never changes.
//
// People's messages, the agent's replies, errors, status, threads, Demo
// requests and reactions stay top-level, in order.
//
// Events without metadata come from older servers. Which run they belong to is
// not known, so each one stays its own collapsed group: nothing is guessed
// across runs. Domain mapping only, no UI.

import type { ActivityPhase, ActivityRoute, RoomEvent } from "../../api/client";
import { activityOf } from "./classify";
import type { Activity } from "./classify";

/** "live": started and not ended yet. "ended": no outcome was recorded (it is never shown as ongoing). */
export type GroupOutcome = "live" | "completed" | "failed" | "cancelled" | "interrupted" | "ended";
export type GroupSource = "agent" | "demo" | "runner";

export type Step = { eventId: number; activity: Activity };

export type ActivityGroup = {
  /** Stable for the life of the group: `<source>:<activity id>`, or `event:<id>` for an uncorrelated event. */
  key: string;
  source: GroupSource;
  /** The agent's display name ("Metro" when the server gave none); "Runner" for runner-only steps. */
  name: string;
  steps: Step[];
  outcome: GroupOutcome;
  /** Some step or a correlated error failed, whatever the lifecycle says. */
  failed: boolean;
  /** Tied to one turn by server metadata. */
  correlated: boolean;
  /** Which model answered and why, from the turn's `started` marker; null when the server sent none. */
  route: ActivityRoute | null;
};

export type TranscriptItem = { type: "event"; key: string; event: RoomEvent } | { type: "group"; key: string; group: ActivityGroup };

/** The built-in agent's name when a turn carries none. Never a person's name. */
export const DEFAULT_AGENT_NAME = "Metro";

const TERMINAL: Record<Exclude<ActivityPhase, "started">, GroupOutcome> = {
  completed: "completed",
  failed: "failed",
  cancelled: "cancelled",
  interrupted: "interrupted",
};

const isLifecycle = (e: RoomEvent) => e.kind.trim().toLowerCase() === "activity";

function legacySource(actor: string): { source: GroupSource; name: string } {
  const a = actor.trim().toLowerCase();
  // Only the built-in agent's own actor is attributed to it; people (a Demo's
  // caller) and the runner are never shown as thinking.
  if (a === "agent" || a === "assistant") return { source: "agent", name: DEFAULT_AGENT_NAME };
  return { source: "runner", name: "Runner" };
}

/** Lifecycle text worth a step: why a turn failed, was cancelled or interrupted. */
function lifecycleStep(e: RoomEvent, phase: ActivityPhase | undefined): Activity | null {
  const text = (e.text ?? "").trim();
  if (!text || !phase || phase === "started" || phase === "completed") return null;
  const label = phase === "failed" ? "Failed" : phase === "cancelled" ? "Cancelled" : "Interrupted";
  return { kind: "progress", summary: label, tone: phase === "failed" ? "danger" : "warning", details: text, mono: false };
}

/**
 * Splits a transcript into top-level events and activity groups, in order.
 * Pure: the same events always give the same items and keys.
 */
export function groupTranscript(events: readonly RoomEvent[]): TranscriptItem[] {
  // Pass 1: every correlated group's steps and outcome, whatever their position.
  const groups = new Map<string, ActivityGroup & { started: boolean; ended: boolean; terminal: GroupOutcome | null }>();
  const memberOf = new Map<number, string>(); // event id -> group key, for events folded into a group
  for (const e of events) {
    const meta = e.activity;
    const step = activityOf(e);
    if (!meta) {
      if (isLifecycle(e)) {
        // An uncorrelated lifecycle line: keep it inspectable, never as "ongoing".
        const text = (e.text ?? "").trim();
        if (!text) {
          memberOf.set(e.id, "");
          continue;
        }
        const { source, name } = legacySource(e.actor);
        const key = `event:${e.id}`;
        groups.set(key, {
          key, source, name, correlated: false, failed: false, outcome: "ended", started: false, ended: true, terminal: null, route: null,
          steps: [{ eventId: e.id, activity: { kind: "progress", summary: "Activity update", tone: "neutral", details: text, mono: false } }],
        });
        memberOf.set(e.id, key);
      } else if (step) {
        const { source, name } = legacySource(e.actor);
        const key = `event:${e.id}`;
        groups.set(key, {
          key, source, name, correlated: false, failed: step.tone === "danger", outcome: "ended", started: false, ended: true, terminal: null, route: null,
          steps: [{ eventId: e.id, activity: step }],
        });
        memberOf.set(e.id, key);
      }
      continue;
    }

    const key = `${meta.source}:${meta.id}`;
    let g = groups.get(key);
    if (!g) {
      g = {
        key,
        source: meta.source,
        name: meta.source === "demo" ? "Runner" : meta.name || DEFAULT_AGENT_NAME,
        steps: [],
        outcome: "ended",
        failed: false,
        correlated: true,
        started: false,
        ended: false,
        terminal: null,
        route: null,
      };
      groups.set(key, g);
    } else if (meta.source === "agent" && meta.name && g.name === DEFAULT_AGENT_NAME) {
      g.name = meta.name; // a later event named the agent
    }

    if (isLifecycle(e)) {
      memberOf.set(e.id, key);
      if (meta.phase === "started") {
        g.started = true;
        if (meta.source === "agent" && meta.route) g.route = meta.route; // Demo never calls a model
      }
      else if (meta.phase) g.terminal = TERMINAL[meta.phase];
      if (meta.phase === "failed") g.failed = true;
      const extra = lifecycleStep(e, meta.phase);
      if (extra) g.steps.push({ eventId: e.id, activity: extra });
    } else if (step) {
      memberOf.set(e.id, key);
      g.steps.push({ eventId: e.id, activity: step });
      if (step.tone === "danger") g.failed = true;
    } else {
      // Stays top-level, but tells the group its turn is over.
      const k = e.kind.trim().toLowerCase();
      if (/(error|fail|denied|reject)/.test(k)) g.failed = true;
      if (k === "agent" || k === "status" || /(error|fail)/.test(k)) g.ended = true;
    }
  }

  for (const g of groups.values()) {
    if (!g.correlated) continue;
    if (g.terminal) g.outcome = g.terminal;
    else if (g.started && !g.ended) g.outcome = "live";
    else g.outcome = "ended";
    if (g.failed && (g.outcome === "completed" || g.outcome === "ended")) g.outcome = "failed";
  }

  // Pass 2: place each group at its first event; everything else in order.
  const items: TranscriptItem[] = [];
  const placed = new Set<string>();
  for (const e of events) {
    const key = memberOf.get(e.id);
    if (key === undefined) {
      items.push({ type: "event", key: `event:${e.id}`, event: e });
      continue;
    }
    if (!key || placed.has(key)) continue;
    const g = groups.get(key)!;
    // A turn that ended with nothing to inspect leaves no empty shell, unless
    // its route is worth saying (a use case, a chosen model, a System One or
    // scheduled start): then it stays one row, with nothing else in it.
    if (g.steps.length === 0 && !routeBadge(g.route) && (g.outcome === "completed" || g.outcome === "ended")) continue;
    placed.add(key);
    const { started: _s, ended: _e, terminal: _t, ...group } = g;
    items.push({ type: "group", key: `group:${key}`, group });
  }
  return items;
}

/** The group's one-line title. Only a live agent turn says it is thinking. */
export function groupTitle(g: ActivityGroup): string {
  if (g.source !== "agent") return "Runner activity";
  if (g.outcome === "live") return `${g.name} is thinking…`;
  return `${g.name}’s activity`;
}

/** A short outcome for the badge and screen readers; null when it just finished normally. */
export function groupOutcomeText(g: ActivityGroup): string | null {
  switch (g.outcome) {
    case "live":
      // A step that already failed shows, even while the turn goes on.
      return g.failed ? "failed" : g.source === "agent" ? null : "running";
    case "failed":
      return "failed";
    case "cancelled":
      return "cancelled";
    case "interrupted":
      return "interrupted";
    default:
      return g.failed ? "failed" : null;
  }
}

/**
 * The turn's route in a few words for the collapsed row, or null when there
 * is nothing worth a badge (the asker's own default, a typed @agent).
 */
export function routeBadge(r: ActivityRoute | null): string | null {
  if (!r) return null;
  if (r.source === "case" && r.caseLabel) return r.caseLabel;
  if (r.source === "override") return "model chosen";
  if (r.addressedBy === "classifier") return "started by System One";
  if (r.addressedBy === "schedule") return "scheduled";
  return null;
}

/** The turn's route in a sentence or two, for the opened details. */
export function routeText(r: ActivityRoute): string {
  const how =
    r.addressedBy === "classifier"
      ? "System One judged that the message asked the agent; nobody typed @agent."
      : r.addressedBy === "schedule"
        ? "A scheduled prompt started it."
        : r.addressedBy === "thread"
          ? "Delegated from a parent turn."
          : null;
  const where: Record<ActivityRoute["source"], string> = {
    override: "The model chosen for this message",
    case: r.caseLabel ? `Use case “${r.caseLabel}”` : "A use case",
    routing_default: "The routing fallback model",
    default: "The default chat model of the person who asked",
    environment: "The server's configured model",
    inherited: "The parent turn's model",
  };
  const model = r.modelId ? ` (${r.providerId ? `${r.providerId} · ` : ""}${r.modelId})` : "";
  const reason = r.reason ? `: ${r.reason}` : "";
  return [`${where[r.source]}${model} answered${reason}.`, how].filter(Boolean).join(" ");
}

/** What a screen reader hears for the disclosure, e.g. "Metro’s activity, failed, 3 steps". */
export function groupAccessibleLabel(g: ActivityGroup): string {
  const outcome = groupOutcomeText(g) ?? (g.outcome === "completed" ? "completed" : g.outcome === "live" ? "in progress" : null);
  const steps = g.steps.length === 1 ? "1 step" : `${g.steps.length} steps`;
  return [groupTitle(g), outcome, g.steps.length ? steps : "no steps yet"].filter(Boolean).join(", ");
}
