import type { Mention, RoomEvent } from "../../api/client";
import { MAX_MENTION_SPANS } from "./draft";
import type { WireMentionSpan } from "./draft";
import {
  mentionSyntaxRanges, overlapsRanges, saneTypedLabel, validMentionSpan,
} from "./syntax";

function byteBoundaries(text: string): Map<number, number> {
  const offsets = new Map([[0, 0]]);
  let bytes = 0, units = 0;
  for (const ch of text) {
    const cp = ch.codePointAt(0)!;
    bytes += cp <= 0x7f ? 1 : cp <= 0x7ff ? 2 : cp <= 0xffff ? 3 : 4;
    units += ch.length;
    offsets.set(bytes, units);
  }
  return offsets;
}

/** Invalid wire spans never fall back to guessing a textual identity. */
export function readMentionSpans(raw: unknown, text: string, indexes: ReadonlyMap<number, number>): WireMentionSpan[] {
  if (!Array.isArray(raw) || !raw.length || raw.length > MAX_MENTION_SPANS) return [];
  const offsets = byteBoundaries(text);
  const spans: WireMentionSpan[] = [];
  let lastEnd = 0;
  for (const item of raw) {
    if (typeof item !== "object" || item === null) return [];
    const { start, end, index } = item;
    const from = offsets.get(start), to = offsets.get(end), recipient = indexes.get(index);
    if (from === undefined || to === undefined || recipient === undefined
      || start < lastEnd || !validMentionSpan(text, { start: from, end: to })) return [];
    spans.push({ start, end, index: recipient });
    lastEnd = end;
  }
  return spans;
}

export type MentionPiece = {
  start: number; end: number; literal: string; index: number; mention: Mention;
};

/** Position is authoritative if supplied. Text fallback only sees stored recipients, never the directory. */
export function sentMentions(event: RoomEvent, handleOf: (agentId: string) => string | undefined): MentionPiece[] {
  const { mentions, spans, text } = event;
  if (event.kind !== "message" || !mentions?.length) return [];
  if (spans !== undefined) {
    if (!spans.length) return [];
    const offsets = byteBoundaries(text);
    return spans.flatMap(({ start, end, index }) => {
      const from = offsets.get(start), to = offsets.get(end), mention = mentions[index];
      return from === undefined || to === undefined || !mention
        ? [] : [{ start: from, end: to, literal: text.slice(from, to), index, mention }];
    });
  }
  const aliases = mentions.flatMap((mention, index) => {
    const label = mention.kind === "user" ? mention.id
      : mention.addressedBy ? undefined : mention.agentId ? handleOf(mention.agentId) : "agent";
    // An old named agent with the reserved handle must not claim generic text.
    return label && saneTypedLabel(label) && !/\p{White_Space}/u.test(label)
      && (label.toLowerCase() !== "agent" || (mention.kind === "agent" && !mention.agentId))
      ? [{ label: label.toLowerCase(), index, mention }] : [];
  });
  if (!aliases.length) return [];
  const { ignored } = mentionSyntaxRanges(text, []);
  const out: MentionPiece[] = [];
  for (let start = text.indexOf("@"); start >= 0 && out.length < MAX_MENTION_SPANS; start = text.indexOf("@", start + 1)) {
    if (out.length && start < out[out.length - 1].end) continue;
    let length = 0;
    let best: typeof aliases = [];
    for (const alias of aliases) {
      const n = alias.label.length;
      const end = start + 1 + n;
      if (n < length || text.slice(start + 1, end).toLowerCase() !== alias.label
        || !validMentionSpan(text, { start, end }) || overlapsRanges({ start, end }, ignored)) continue;
      if (n > length) { length = n; best = []; }
      best.push(alias);
    }
    if (best.length === 1) {
      const end = start + 1 + length, { index, mention } = best[0];
      out.push({ start, end, literal: text.slice(start, end), index, mention });
    }
  }
  return out;
}

export function copyMentionText(text: string, pieces: readonly MentionPiece[], label: (piece: MentionPiece) => string): string {
  let end = 0, out = "";
  for (const piece of pieces) {
    out += text.slice(end, piece.start) + label(piece);
    end = piece.end;
  }
  return out + text.slice(end);
}
