// Which transcript events are agent *activity* (tool calls, tool output,
// provider-shared thinking summaries) and how their one-line summary reads.
// Activity starts collapsed; everything else (people, the agent's replies,
// errors, status, thread results, Demo) stays visible. Domain mapping only.
//
// This labels one event from its own text only. Folding a turn's events into
// one group (by their `activity` metadata) happens in ./group.

import type { Tone } from "../../ui";
import { metaToolLabel, webOfflineOwner, webResultCount, webToolLabel } from "./web";
import type { WebToolNote } from "../../api/webSearch";
import type { MetaNote } from "../../api/meta";

export type ActivityKind = "tool_call" | "tool_result" | "thinking" | "progress";

export type Activity = {
  kind: ActivityKind;
  /** The collapsed one-liner, e.g. "Tool · List workspace · complete". */
  summary: string;
  tone: Tone;
  /** What the details show once opened; "" when the event has nothing more. */
  details: string;
  /** Monospace details (tool arguments and output). */
  mono: boolean;
  web?: { note: WebToolNote; offlineOwner?: string | null };
  meta?: MetaNote;
};

const TOOL_NAMES: Record<string, string> = {
  "workspace.list": "List workspace",
  "workspace.read": "Read file",
  "workspace.check": "Run check",
  "workspace.mcp_list": "List MCP tools",
  "workspace.mcp": "Call MCP tool",
};

/** "workspace.list" or the model's "workspace_list" → "List workspace"; unknown names are spelled out. */
export function toolName(raw: string): string {
  const name = raw.trim();
  const known = TOOL_NAMES[name] ?? TOOL_NAMES[name.replace(/^workspace_/, "workspace.")];
  if (known) return known;
  const spelled = name.replace(/[._-]+/g, " ").trim();
  return spelled ? spelled.charAt(0).toUpperCase() + spelled.slice(1) : "Tool";
}

function norm(kind: string): string {
  return kind.trim().toLowerCase().replace(/[.\-\s]+/g, "_");
}

const TOOL_CALL_KINDS = new Set(["tool_call", "tool_use", "tool_request", "runner_call"]);
const TOOL_RESULT_KINDS = new Set(["tool_result", "tool_output", "runner_result", "runner_output"]);
// Only events a server or provider actually labels as thinking: nothing is inferred.
const THINKING_KINDS = new Set(["thinking", "reasoning", "reasoning_summary", "thinking_summary", "agent_thinking", "agent_reasoning"]);
const PROGRESS_KINDS = new Set(["progress", "working", "agent_progress", "agent_working"]);

/** Where a step stands, for its icon: called, complete, failed, a thought, progress, or stopped (cancelled, interrupted). */
export function stepPhase(a: Activity): "called" | "complete" | "failed" | "thinking" | "progress" | "stopped" {
  if (a.tone === "danger") return "failed";
  if (a.kind === "tool_call") return "called";
  if (a.kind === "tool_result") return "complete";
  if (a.kind === "thinking") return "thinking";
  return a.tone === "warning" ? "stopped" : "progress";
}

/** The event's activity, or null when it should stay visible as it is. */
export function activityOf(event: { kind: string; text: string; actorId?: string; activity?: { tool?: WebToolNote; meta?: MetaNote } }): Activity | null {
  const k = norm(event.kind);
  const text = event.text ?? "";
  const note = event.activity?.tool;
  if (note && !event.actorId && (k === "error" || k === "tool_call" || k === "tool_result")) {
    const web = { note: k === "error" && !note.failure ? { ...note, failure: "failed" as const } : note,
      offlineOwner: note.failure === "offline" ? webOfflineOwner(text) : null };
    const summary = webToolLabel(note.name);
    if (k === "error" || note.failure) return { kind: "tool_result", summary, tone: "danger", details: "", mono: false, web };
    if (k === "tool_result") return { kind: "tool_result", summary, tone: "runner", details: webResultCount(text), mono: false, web };
    if (k === "tool_call") return { kind: "tool_call", summary, tone: "runner",
      details: text.startsWith(`${note.name} `) ? text.slice(note.name.length + 1) : "", mono: false, web };
  }
  const meta = event.activity?.meta;
  if (meta && !event.actorId && (k === "error" || k === "tool_call" || k === "tool_result")) {
    const label = metaToolLabel(meta.tool);
    const details = k === "tool_call" ? "" : text.trim().replace(new RegExp(`^${meta.tool} refused: `), "");
    return {
      kind: k === "tool_call" ? "tool_call" : "tool_result", summary: label,
      tone: k === "error" ? "danger" : "runner", details, mono: false, meta,
    };
  }
  // Errors and failures are never hidden, whatever else their kind says.
  if (/(error|fail|denied|reject)/.test(k)) return null;

  if (TOOL_CALL_KINDS.has(k)) {
    // Server format: "<tool> <arguments JSON>".
    const m = /^(\S+)(?:\s+([\s\S]*))?$/.exec(text.trim());
    const name = m ? toolName(m[1]!) : "Tool";
    return { kind: "tool_call", summary: `Tool · ${m ? name : "call"} · called`, tone: "runner", details: text.trim(), mono: true };
  }

  if (TOOL_RESULT_KINDS.has(k)) {
    // Server format: "<tool> ok|failed\n<output>". Anything else is shown as plain output.
    const nl = text.indexOf("\n");
    const head = (nl < 0 ? text : text.slice(0, nl)).trim();
    const m = /^(\S+) (ok|failed)$/.exec(head);
    if (m) {
      const failed = m[2] === "failed";
      const output = nl < 0 ? "" : text.slice(nl + 1).replace(/\s+$/, "");
      return {
        kind: "tool_result",
        summary: `Tool · ${toolName(m[1]!)} · ${failed ? "failed" : "complete"}`,
        tone: failed ? "danger" : "runner",
        details: output,
        mono: true,
      };
    }
    return { kind: "tool_result", summary: "Tool · output", tone: "runner", details: text.replace(/\s+$/, ""), mono: true };
  }

  if (THINKING_KINDS.has(k)) {
    return { kind: "thinking", summary: "Thinking · summary shared by the model provider", tone: "agent", details: text.trim(), mono: false };
  }

  if (PROGRESS_KINDS.has(k)) {
    return { kind: "progress", summary: "Agent · working", tone: "agent", details: text.trim(), mono: false };
  }

  return null;
}
