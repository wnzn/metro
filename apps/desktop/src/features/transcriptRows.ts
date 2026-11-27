// The transcript's rows: the grouped items (src/features/activity/group.ts)
// with a date line before each new day and consecutive messages by the same
// author drawn as one block. Pure: the same items and clock always give the
// same rows and keys. Domain mapping only, no UI.

import type { RoomEvent } from "../api/client";
import type { ActivityGroup, TranscriptItem } from "./activity/group";
import { dayLabel, stampOf } from "./chatTime";
import type { Clock, Stamp } from "./chatTime";

/** Messages this close together by the same author share one header. */
export const CONTINUE_WITHIN_MS = 5 * 60_000;

export type TranscriptRow =
  | { type: "day"; key: string; label: string }
  | { type: "event"; key: string; event: RoomEvent; stamp: Stamp | null; continued: boolean }
  | { type: "group"; key: string; group: ActivityGroup; stamp: Stamp | null };

/** Who wrote an event, for grouping: a person by stable id, else the server actor and kind. */
export function authorKey(e: RoomEvent): string | null {
  const kind = e.kind.trim().toLowerCase();
  if (kind === "message" && e.actorId) return `user:${e.actorId}`;
  if (kind === "agent") return `agent:${e.activity?.name || e.actor}`;
  return null; // errors, status, threads, Demo, unknown kinds: always their own header
}

type MentionOf = NonNullable<RoomEvent["mentions"]>[number];

/**
 * What a message header adds beside its explicit recipients. The recipient
 * pills (@person, @agent, "agent · scheduled prompt") already say who it
 * is for, so no audience phrase repeats
 * them. Only what no pill says is shown: `agentBadge` when the agent was
 * asked without an agent mention (Ask agent), `peopleNote` when people were
 * addressed but no person is listed (older events without recipients). The
 * full audience stays in `spoken` for screen readers.
 */
export function headerAudience(
  audience: string | undefined,
  mentions: readonly MentionOf[] | undefined,
): { agentBadge: boolean; peopleNote: boolean; spoken: string | null } {
  const toAgent = audience === "agent" || audience === "mixed";
  const toPeople = audience === "users" || audience === "mixed";
  const agentPill = !!mentions?.some((m) => m.kind === "agent");
  const personPill = !!mentions?.some((m) => m.kind === "user");
  const spoken = toAgent && toPeople ? "Addressed to the agent and people" : toAgent ? "Addressed to the agent" : toPeople ? "Addressed to people" : null;
  return { agentBadge: toAgent && !agentPill, peopleNote: toPeople && !personPill, spoken };
}

export function transcriptRows(items: readonly TranscriptItem[], events: readonly RoomEvent[], clock: Clock): TranscriptRow[] {
  const created = new Map<number, string | undefined>();
  for (const e of events) created.set(e.id, e.createdAt);
  const rows: TranscriptRow[] = [];
  let lastDay: string | null = null;
  let prev: { author: string | null; ms: number | null } | null = null;
  for (const item of items) {
    let stamp: Stamp | null;
    if (item.type === "event") stamp = stampOf(item.event.createdAt, clock);
    else {
      // A turn's time is its earliest step the server dated.
      stamp = null;
      for (const step of item.group.steps) {
        const s = stampOf(created.get(step.eventId), clock);
        if (s && (!stamp || s.ms < stamp.ms)) stamp = s;
      }
    }
    let newDay = false;
    if (stamp && stamp.day !== lastDay) {
      lastDay = stamp.day;
      newDay = true;
      rows.push({ type: "day", key: `day:${stamp.day}:${item.key}`, label: dayLabel(stamp.ms, clock) });
    }
    if (item.type === "group") {
      rows.push({ type: "group", key: item.key, group: item.group, stamp });
      prev = { author: null, ms: stamp?.ms ?? null };
      continue;
    }
    const author = authorKey(item.event);
    const continued =
      !newDay &&
      !!prev &&
      author !== null &&
      prev.author === author &&
      stamp !== null &&
      prev.ms !== null &&
      stamp.ms - prev.ms >= 0 &&
      stamp.ms - prev.ms <= CONTINUE_WITHIN_MS;
    rows.push({ type: "event", key: item.key, event: item.event, stamp, continued });
    prev = { author, ms: stamp?.ms ?? null };
  }
  return rows;
}

/**
 * The message's text already starts by naming every explicit recipient
 * ("@bob @agent can you…"), so a follow-up message needs no recipient line
 * of its own. Only typed mentions count: an agent the classifier picked or
 * a schedule started was never written in the text.
 */
export function textNamesRecipients(text: string, mentions: readonly MentionOf[] | undefined): boolean {
  if (!mentions?.length) return false;
  const lead = /^\s*((?:@[^\s@,:;]+[\s,:;]*)+)/.exec(text)?.[1] ?? "";
  const named = new Set((lead.match(/@[^\s@,:;]+/g) ?? []).map((t) => t.slice(1).replace(/[.!?]+$/, "").toLowerCase()));
  return mentions.every((m) => (m.kind === "agent" ? !m.addressedBy && named.has("agent") : named.has(m.id.toLowerCase())));
}
