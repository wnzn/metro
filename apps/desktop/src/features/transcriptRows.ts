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
