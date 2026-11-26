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

import type { ActivityPhase, ActivityRoute, EventAgent, RoomEvent } from "../../api/client";
import { activityOf, toolName } from "./classify";
import type { Activity } from "./classify";
import { webRefusal } from "./web";

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
  /** The Space agent working the turn, when its events name one. */
  agent: EventAgent | null;
  /** The server's id for the turn (a Space run's `run_…`); null for an uncorrelated event. */
  activityId: string | null;
  compactionMiss?: boolean;
  /** When its first and last events were recorded (ms), when the server dated them. */
  firstMs: number | null;
  lastMs: number | null;
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
export const COMPACTION_MISS = "Couldn't summarize earlier messages";
export const isRoomCompactionMiss = (e: RoomEvent) => e.activity?.source === "agent" &&
  e.kind === "status" && e.text === `${COMPACTION_MISS}; the agent used the most recent messages.`;

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
          key, source, name, correlated: false, failed: false, outcome: "ended", started: false, ended: true, terminal: null, route: null, agent: null, activityId: null, firstMs: null, lastMs: null,
          steps: [{ eventId: e.id, activity: { kind: "progress", summary: "Activity update", tone: "neutral", details: text, mono: false } }],
        });
        memberOf.set(e.id, key);
      } else if (step) {
        const { source, name } = legacySource(e.actor);
        const key = `event:${e.id}`;
        groups.set(key, {
          key, source, name, correlated: false, failed: step.tone === "danger", outcome: "ended", started: false, ended: true, terminal: null, route: null, agent: null, activityId: null, firstMs: null, lastMs: null,
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
        agent: meta.agent ?? null,
        activityId: meta.id,
        firstMs: null,
        lastMs: null,
      };
      groups.set(key, g);
    } else if (meta.source === "agent" && meta.name && g.name === DEFAULT_AGENT_NAME) {
      g.name = meta.name; // a later event named the agent
    }
    if (!g.agent && meta.agent) g.agent = meta.agent;

    const ms = e.createdAt ? Date.parse(e.createdAt) : NaN;
    if (Number.isFinite(ms)) {
      if (g.firstMs === null || ms < g.firstMs) g.firstMs = ms;
      if (g.lastMs === null || ms > g.lastMs) g.lastMs = ms;
    }
    if (isRoomCompactionMiss(e)) {
      g.compactionMiss = true;
      memberOf.set(e.id, key);
    } else if (meta.source === "agent" && e.kind === "status" && e.text.startsWith(`${COMPACTION_MISS}: `)) {
      // Chat carries its private reason as a regular status row. Neither
      // status ends the agent's activity: the current run keeps answering.
    } else if (isLifecycle(e)) {
      memberOf.set(e.id, key);
      if (meta.phase === "started") {
        g.started = true;
        if (meta.source === "agent" && meta.route) g.route = meta.route; // Demo never calls a model
      }
      else if (meta.phase) {
        g.terminal = TERMINAL[meta.phase];
      }
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
    if (!g.compactionMiss && g.steps.length === 0 && !routeBadge(g.route) && (g.outcome === "completed" || g.outcome === "ended")) continue;
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

/** System One's "(p 0.91)" in a route's reason: its probability, 0..1. */
const PROBABILITY = /\s*\(p\s*(\d+(?:\.\d+)?|\.\d+)\)/i;

/** The probability a route's reason gives, as a whole percent; null when it gives none (or one out of range). */
export function routeConfidence(r: ActivityRoute | null): number | null {
  const m = r ? PROBABILITY.exec(r.reason) : null;
  if (!m) return null;
  const p = Number(m[1]);
  return Number.isFinite(p) && p >= 0 && p <= 1 ? Math.round(p * 100) : null;
}

/** Legacy text that says an agent was picked, or its run started, by the classifier. */
const CLASSIFIER_ATTRIBUTION = /picked by (the )?classifier|detected by system one|started by system one|judged that the message asked|classifier was unsure|the classifier\b/i;

/** A route's reason with "(p 0.91)" said in words ("with 91% probability"); a malformed one is dropped. */
function readableReason(r: ActivityRoute): string {
  // How a run was picked is never attributed to a classifier, even in text an older
  // server stored. Use-case routing's own explanations and failures ("System One
  // matched the use case …", "routing failed: System One took too long") stay.
  if (CLASSIFIER_ATTRIBUTION.test(r.reason)) return "";
  const pct = routeConfidence(r);
  return r.reason.replace(PROBABILITY, pct === null ? "" : ` with ${pct}% probability`).trim().replace(/[.\s]+$/, "");
}

/**
 * The turn's route in a few words for the collapsed row ("Routed: Code
 * review · 91%"), or null when there is nothing worth a badge (the asker's own
 * default, a typed @agent). The full sentence is only in the opened details.
 */
export function routeBadge(r: ActivityRoute | null): string | null {
  if (!r) return null;
  if (r.source === "case" && r.caseLabel) {
    const pct = routeConfidence(r);
    return `Routed: ${r.caseLabel}${pct === null ? "" : ` · ${pct}%`}`;
  }
  if (r.source === "override") return "Routed: chosen model";
  if (r.addressedBy === "schedule") return "Scheduled";
  return null;
}

/** The turn's route in a sentence or two, for the opened details. Never the raw "(p 0.91)". */
export function routeText(r: ActivityRoute): string {
  const how =
    r.addressedBy === "schedule"
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
  const said = r.reason ? readableReason(r) : "";
  const reason = said ? `: ${said}` : "";
  return [`${where[r.source]}${model} answered${reason}.`, how].filter(Boolean).join(" ");
}

/** How many tools the turn used: its calls, or its results where no call was recorded. */
export function toolCount(g: ActivityGroup): number {
  const calls = g.steps.filter((s) => s.activity.kind === "tool_call").length;
  return calls || g.steps.filter((s) => s.activity.kind === "tool_result").length;
}

/** "thought 12s" for an agent turn that ended (runner work "took"); null while live or undated. */
export function durationText(g: ActivityGroup): string | null {
  if (g.outcome === "live" || g.firstMs === null || g.lastMs === null) return null;
  const s = Math.round((g.lastMs - g.firstMs) / 1000);
  const verb = g.source === "agent" ? "thought" : "took";
  if (s < 1) return null;
  return s < 90 ? `${verb} ${s}s` : `${verb} ${Math.round(s / 60)}m`;
}

/**
 * The collapsed line's parts after the name: how long, how many tools, the
 * route it took, and how it stopped when that wasn't a plain finish
 * ("cancelled"). A failure is its own pill, not a part.
 */
export function summaryParts(g: ActivityGroup): { duration: string | null; tools: string | null; route: string | null; stopped: string | null } {
  const n = toolCount(g);
  const outcome = groupOutcomeText(g);
  return {
    duration: durationText(g),
    tools: n === 0 ? null : n === 1 ? "1 tool" : `${n} tools`,
    route: routeBadge(g.route),
    stopped: outcome === "cancelled" || outcome === "interrupted" ? outcome : null,
  };
}

/** A tool call's arguments in a few words: a path, else the values, else the raw text; at most 60 characters. */
export function shortArgs(raw: string): string {
  const text = raw.trim();
  if (!text) return "";
  let out = text;
  try {
    const v = JSON.parse(text) as unknown;
    if (v && typeof v === "object" && !Array.isArray(v)) {
      const o = v as Record<string, unknown>;
      const first = o.path ?? o.file ?? o.name ?? o.tool ?? o.query;
      const values = Object.values(o).filter((x) => typeof x === "string" || typeof x === "number");
      out = typeof first === "string" ? first : values.length ? values.join(" ") : Object.keys(o).length ? text : "";
    }
  } catch {
    /* not JSON: shown as it came */
  }
  out = out.replace(/\s+/g, " ");
  return out.length > 60 ? `${out.slice(0, 59)}…` : out;
}

/** A tool call's text ("workspace.read {…}") as its raw name and arguments. */
export function splitCall(details: string): { tool: string; args: string } {
  const m = /^(\S+)(?:\s+([\s\S]*))?$/.exec(details.trim());
  return m ? { tool: m[1]!, args: m[2] ?? "" } : { tool: "", args: "" };
}

/** What a live turn is doing now, in a few words ("Reading README.md…"). */
export function currentStep(g: ActivityGroup): string {
  const last = g.steps[g.steps.length - 1]?.activity;
  if (g.source !== "agent") return "Runner is working…";
  if (last?.kind === "tool_call") {
    if (last.web) return `Using ${last.summary}…`;
    if (last.meta) return `${last.summary}…`;
    const { tool, args } = splitCall(last.details);
    const what = shortArgs(args);
    switch (tool.replace(/^workspace_/, "workspace.")) {
      case "workspace.read":
        return what ? `Reading ${what}…` : "Reading a file…";
      case "workspace.list":
        return !what || what === "." ? "Listing the workspace…" : `Listing ${what}…`;
      case "workspace.check":
        return "Running a check…";
      case "workspace.mcp_list":
        return "Listing MCP tools…";
      case "workspace.mcp":
        return "Calling an MCP tool…";
      default:
        return `Using ${toolName(tool)}…`;
    }
  }
  return `${g.name} is thinking…`;
}

/** A turn's steps as a timeline: each tool call with its result, thoughts and notes in order. */
export type TimelineItem =
  | { type: "tool"; key: string; call: Step | null; result: Step | null; name: string; args: string; failed: boolean }
  | { type: "thought"; key: string; step: Step }
  | { type: "note"; key: string; step: Step };

export function timeline(g: ActivityGroup): TimelineItem[] {
  const out: TimelineItem[] = [];
  for (const step of g.steps) {
    const a = step.activity;
    if (a.kind === "tool_call") {
      if (a.web || a.meta) {
        out.push({ type: "tool", key: `step:${step.eventId}`, call: step, result: null, name: a.summary, args: "", failed: false });
        continue;
      }
      const { tool, args } = splitCall(a.details);
      out.push({ type: "tool", key: `step:${step.eventId}`, call: step, result: null, name: tool ? toolName(tool) : "Tool", args: shortArgs(args), failed: false });
    } else if (a.kind === "tool_result") {
      const name = a.web || a.meta ? a.summary : /^Tool · (.+) · (?:complete|failed)$/.exec(a.summary)?.[1] ?? "Tool";
      const open = a.web && webRefusal(a.web.note) ? undefined :
        [...out].reverse().find((i): i is Extract<TimelineItem, { type: "tool" }> =>
          i.type === "tool" && !i.result && i.name === name &&
          i.call?.activity.web?.note.name === a.web?.note.name && i.call?.activity.meta?.tool === a.meta?.tool);
      if (open) {
        open.result = step;
        open.failed = a.tone === "danger";
      } else out.push({ type: "tool", key: `step:${step.eventId}`, call: null, result: step, name, args: "", failed: a.tone === "danger" });
    } else if (a.kind === "thinking") out.push({ type: "thought", key: `step:${step.eventId}`, step });
    else out.push({ type: "note", key: `step:${step.eventId}`, step });
  }
  return out;
}

/** A live turn's one line: "Scout is thinking…", "Metro · Reading README.md…", "Runner is working…". */
export function liveTitle(g: ActivityGroup, name = g.name): string {
  if (g.source !== "agent") return "Runner is working…";
  const step = currentStep({ ...g, name });
  return step === `${name} is thinking…` ? step : `${name} · ${step}`;
}

/** What a screen reader hears for the disclosure, e.g. "Metro, 2 tools, Routed: chosen model, completed". */
export function groupAccessibleLabel(g: ActivityGroup, name = g.name): string {
  const p = summaryParts(g);
  const outcome = groupOutcomeText(g) ?? (g.outcome === "completed" ? "completed" : g.outcome === "live" ? "in progress" : null);
  const head = g.outcome === "live" && !g.failed ? liveTitle(g, name) : g.source === "agent" ? name : "Runner activity";
  return [head, p.duration, p.tools, p.route, outcome].filter(Boolean).join(", ");
}
