// How event kinds and actors are presented: domain mapping only, no styling.
// Unknown kinds fall back to "neutral" so new server event types still render.

import type { Tone } from "../ui";

export function toneForEvent(kind: string, actor: string): Tone {
  const k = kind.toLowerCase();
  const a = actor.toLowerCase();
  if (/(error|fail|denied|reject)/.test(k)) return "danger";
  if (/(runner|tool|workspace|demo|check|mcp)/.test(k) || a === "runner" || a.startsWith("runner:")) return "runner";
  if (/(agent|assistant|model)/.test(k) || a === "agent" || a === "assistant") return "agent";
  if (/(system|join|leave|presence|room|info|status)/.test(k) || a === "system") return "system";
  if (/(ok|done|success|complete)/.test(k)) return "success";
  return "neutral";
}

/** A short, readable label for a kind like "runner.result" or "agent_message". */
export function kindLabel(kind: string): string {
  const label = kind.trim().replace(/[_]+/g, " ");
  return label || "event";
}

export type MentionLike = { kind: "agent"; addressedBy?: "classifier" | "schedule" | "reply" | "metro" } | { kind: "user"; id: string };

/** "@agent", "@you" for the signed-in user, else "@<user id>". */
export function mentionText(m: MentionLike, myId: string | null): string {
  // Not typed by the sender, so never shown as an "@agent" they wrote.
  if (m.kind === "agent" && m.addressedBy === "classifier") return "agent · detected by System One";
  if (m.kind === "agent" && m.addressedBy === "schedule") return "agent · scheduled prompt";
  if (m.kind === "agent" && m.addressedBy === "reply") return "agent · reply";
  if (m.kind === "agent" && m.addressedBy === "metro") return "agent · via Metro";
  if (m.kind === "agent") return "@agent";
  return m.id === myId ? "@you" : `@${m.id}`;
}

/** A short phrase for a message's audience; null when it addresses the whole room. */
export function audienceText(audience: string | undefined): string | null {
  switch (audience) {
    case "agent":
      return "to the agent";
    case "users":
      return "to people";
    case "mixed":
      return "to the agent and people";
    default:
      return null;
  }
}

export function mentionsMe(mentions: readonly MentionLike[] | undefined, myId: string | null): boolean {
  return !!myId && !!mentions?.some((m) => m.kind === "user" && m.id === myId);
}

/**
 * Who reacted with one emoji: reactor keys are "user:<id>" or "agent:<id>".
 * `count` counts people (the agent is shown separately), `description` is for
 * screen readers.
 */
export function reactionSummary(reactors: readonly string[], myId: string | null) {
  const people = reactors.filter((r) => r.startsWith("user:")).map((r) => r.slice(5));
  const agent = reactors.some((r) => r.startsWith("agent:"));
  const mine = !!myId && people.includes(myId);
  const others = people.filter((p) => p !== myId);
  const names = [...(mine ? ["you"] : []), ...others, ...(agent ? ["the agent"] : [])];
  const description = names.length === 0 ? "no reactions" : `reacted by ${names.length > 1 ? `${names.slice(0, -1).join(", ")} and ${names[names.length - 1]}` : names[0]}`;
  return { count: people.length, mine, agent, description };
}
