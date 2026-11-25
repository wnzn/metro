import {
  closesMention, isHandle, isMemberId, isRange, isTextBoundary, mentionCodeRanges, opensMention, overlapsCode,
  saneExplicitLabel, saneTypedLabel, validMentionSpan,
} from "./syntax";
import type { MentionRange } from "./syntax";

export type { MentionRange } from "./syntax";
export const MAX_MENTION_SPANS = 64;
export const MAX_MENTION_RECIPIENTS = 32;

export type MentionRecipient = { kind: "agent"; agentId?: string } | { kind: "user"; id: string };
export type MentionDirectory = {
  readonly members: readonly { id: string; displayName?: string | null }[];
  readonly agents: readonly { id: string; handle: string; name: string }[];
};

export type MentionSelection = MentionRange & { readonly literal: string };
export type MentionAnchor = MentionSelection & { readonly mention: MentionRecipient };
export type MentionDraft = { readonly text: string; readonly anchors: readonly MentionAnchor[] };
export type MentionMatch =
  | (MentionAnchor & { kind: "resolved" })
  | (MentionSelection & { kind: "ambiguous"; choices: readonly MentionRecipient[] })
  | (MentionSelection & { kind: "unresolved" });

type WireRecipient = { kind: "agent"; agent_id?: string } | { kind: "user"; id: string };
export type WireMentionSpan = { start: number; end: number; index: number };
type MentionBody = { text: string; mentions?: WireRecipient[]; spans?: WireMentionSpan[] };
export type MentionSend =
  | { ok: true; body: MentionBody }
  | { ok: false; reason: "ambiguous"; match: Extract<MentionMatch, { kind: "ambiguous" }> }
  | { ok: false; reason: "invalid-text" | "invalid-anchor" | "stale-recipient" | "too-many-spans" | "too-many-recipients" };

const generic: MentionRecipient = { kind: "agent" };
const isGenericLabel = (label: string) => label.toLowerCase() === "agent";

function key(mention: MentionRecipient): string {
  return mention.kind === "user" ? `user:${mention.id}`
    : mention.agentId === undefined ? "generic" : `agent:${mention.agentId}`;
}

function hasRecipient(directory: MentionDirectory, mention: MentionRecipient): boolean {
  return mention.kind === "user" ? directory.members.some(m => m.id === mention.id)
    : mention.agentId === undefined || directory.agents.some(a => a.id === mention.agentId);
}

type Alias = { match: RegExp; priority: number; mention: MentionRecipient };

function aliases(directory: MentionDirectory): Alias[] {
  const out: Alias[] = [];
  function add(label: string, priority: number, mention: MentionRecipient, handle = false) {
    if (handle ? !isHandle(label) : !saneTypedLabel(label)) return;
    const literal = label.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
    out.push({ match: new RegExp(`^${literal}`, handle ? "i" : "iu"), priority, mention });
  }
  add("agent", 0, generic, true);
  for (const a of directory.agents) {
    const mention = { kind: "agent", agentId: a.id } as const;
    if (!isGenericLabel(a.handle)) add(a.handle, 1, mention, true);
    add(a.name, 3, mention);
  }
  for (const m of directory.members) {
    const mention = { kind: "user", id: m.id } as const;
    if (isMemberId(m.id)) add(m.id, 2, mention, isHandle(m.id));
    if (m.displayName) add(m.displayName, 3, mention);
  }
  return out;
}

function anchorSyntax(draft: MentionDraft) {
  const { text } = draft;
  let end = 0;
  let anchors = [...draft.anchors].sort((a, b) => a.start - b.start).filter(a => {
    if (a.start < end || !validMentionSpan(text, a) || text.slice(a.start, a.end) !== a.literal) return false;
    end = a.end;
    return true;
  });
  // Delimiters inside selected labels do not affect other tokens. If an
  // edit wrapped some anchors in real code, drop them and unmask their text
  // before checking the remaining anchors.
  for (;;) {
    const code = mentionCodeRanges(text, anchors);
    const outside = anchors.filter(a => !overlapsCode(a, code));
    if (outside.length === anchors.length) return { anchors, code };
    anchors = outside;
  }
}

function inspect(
  draft: MentionDraft, directory: MentionDirectory, final: boolean, code: readonly MentionRange[],
): MentionMatch[] {
  const { text } = draft;
  const saved = new Map(draft.anchors.map(a => [a.start, a]));
  const lookup = aliases(directory);
  const matches: MentionMatch[] = [];
  let coveredUntil = 0;
  for (let start = text.indexOf("@"); start !== -1; start = text.indexOf("@", start + 1)) {
    if (start < coveredUntil) continue;
    const anchor = saved.get(start);
    if (anchor) {
      matches.push({ ...anchor, kind: "resolved" });
      coveredUntil = anchor.end;
      continue;
    }
    if (!opensMention(text, start) || overlapsCode({ start, end: start + 1 }, code)) continue;
    const rest = text.slice(start + 1);
    let longest = 0;
    let candidates: Alias[] = [];
    for (const alias of lookup) {
      const label = alias.match.exec(rest)?.[0];
      if (!label || label.length < longest || !closesMention(rest.slice(label.length))
        || overlapsCode({ start, end: start + 1 + label.length }, code)) continue;
      if (label.length > longest) {
        longest = label.length;
        candidates = [];
      }
      candidates.push(alias);
    }
    if (longest) {
      const end = start + 1 + longest;
      if (!final && end === text.length) continue;
      const literal = text.slice(start, end);
      // Use priority to order choices, never to break a collision between
      // distinct identities. The exact reserved token is the only exception.
      const choices = new Map<string, MentionRecipient>();
      for (const { mention } of isGenericLabel(literal.slice(1)) ? [{ mention: generic }]
        : candidates.sort((a, b) => a.priority - b.priority)) {
        choices.set(key(mention), mention);
      }
      const distinct = [...choices.values()];
      matches.push(distinct.length === 1
        ? { kind: "resolved", start, end, literal, mention: distinct[0] }
        : { kind: "ambiguous", start, end, literal, choices: distinct });
      continue;
    }
    // Unknown text is only a hint. Do not treat email-like tokens or code as
    // either a mention or a typo the user needs to fix.
    const label = rest.match(/^[^\p{White_Space}]+/u)?.[0].replace(/[.,;:!?)\]}>"'*]+$/u, "") ?? "";
    const end = start + 1 + label.length;
    if (saneTypedLabel(label) && (final || end < text.length)
      && !overlapsCode({ start, end }, code)) {
      matches.push({ kind: "unresolved", start, end, literal: text.slice(start, end) });
    }
  }
  return matches;
}

/** Preview without freezing anchors. Opt in to EOF as a boundary, e.g. after blur. */
export function scanMentionDraft(
  draft: MentionDraft, directory: MentionDirectory, { finalize = false }: { finalize?: boolean } = {},
): MentionMatch[] {
  const { anchors, code } = anchorSyntax(draft);
  return inspect({ text: draft.text, anchors }, directory, finalize, code);
}

type Edit = MentionRange & { readonly replacement: string };

function shiftAnchors(
  draft: MentionDraft, text: string, firstStart: number, lastEnd: number, picked?: MentionAnchor,
): MentionDraft {
  const delta = text.length - draft.text.length;
  const anchors: MentionAnchor[] = [];
  for (const anchor of draft.anchors) {
    // Insertions at an edge are outside the anchor. Adjacency changes still
    // need validation (e.g. adding '\' before @ or a letter after the name).
    const next = anchor.end <= firstStart ? anchor : anchor.start >= lastEnd
      ? { ...anchor, start: anchor.start + delta, end: anchor.end + delta } : null;
    if (next) anchors.push(next);
  }
  if (picked) anchors.push(picked);
  return { text, anchors: anchorSyntax({ text, anchors }).anchors };
}

/** Use an exact native edit if available. Replacing inside an anchor drops it even for identical text. */
export function editMentionDraft(draft: MentionDraft, edit: Edit): MentionDraft | null {
  if (!isRange(draft.text, edit)) return null;
  const text = draft.text.slice(0, edit.start) + edit.replacement + draft.text.slice(edit.end);
  return shiftAnchors(draft, text, edit.start, edit.end);
}

/**
 * For a value-only onChange, preserve anchors outside every minimal single
 * replacement. Repeated text can hide which occurrence was edited: never
 * transfer a chosen identity to a different occurrence by guessing.
 */
export function updateMentionDraft(draft: MentionDraft, text: string): MentionDraft {
  const before = draft.text;
  if (before === text) return draft;
  const limit = Math.min(before.length, text.length);
  let prefix = 0;
  let suffix = 0;
  while (prefix < limit && before[prefix] === text[prefix]) prefix++;
  while (suffix < limit && before[before.length - suffix - 1] === text[text.length - suffix - 1]) suffix++;
  while (!isTextBoundary(before, prefix) || !isTextBoundary(text, prefix)) prefix--;
  while (!isTextBoundary(before, before.length - suffix) || !isTextBoundary(text, text.length - suffix)) suffix--;
  return shiftAnchors(draft, text, Math.min(prefix, limit - suffix),
    before.length - Math.min(suffix, limit - prefix));
}

function displayLabels(directory: MentionDirectory, mention: MentionRecipient): string[] {
  if (mention.kind === "user") {
    return directory.members.filter(m => m.id === mention.id)
      .flatMap(m => [m.displayName ?? "", m.id]);
  }
  if (mention.agentId === undefined) return ["agent"];
  return directory.agents.filter(a => a.id === mention.agentId)
    .flatMap(a => [a.name, ...(isHandle(a.handle) && !isGenericLabel(a.handle) ? [a.handle] : [])]);
}

/**
 * Select a suggestion for the current literal (including bare "@"), or insert
 * at an empty selection from the @ button. An explicit pick may disambiguate
 * even identical labels; its identity never comes from the inserted text.
 */
export function pickMention(
  draft: MentionDraft, selection: MentionSelection, mention: MentionRecipient, directory: MentionDirectory,
): MentionDraft | null {
  const { start, end, literal } = selection;
  if (!isRange(draft.text, selection) || draft.text.slice(start, end) !== literal) return null;
  const append = start === end;
  if (!append && (!literal.startsWith("@") || !opensMention(draft.text, start)
    || !closesMention(draft.text.slice(end)) || (literal !== "@" && !saneExplicitLabel(literal.slice(1)))
    || overlapsCode(selection, anchorSyntax(draft).code))) return null;
  if (draft.anchors.some(a => append ? a.start < start && start < a.end
    : start < a.end && a.start < end && !(start === a.start && end === a.end))) return null;
  const prefix = append && !opensMention(draft.text, start) ? " " : "";
  const suffix = end === draft.text.length || !closesMention(draft.text.slice(end)) ? " " : "";
  for (const label of displayLabels(directory, mention)) {
    if (!saneExplicitLabel(label)) continue;
    const picked = `@${label}`;
    const text = draft.text.slice(0, start) + prefix + picked + suffix + draft.text.slice(end);
    const anchor = { start: start + prefix.length, end: start + prefix.length + picked.length, literal: picked, mention };
    const next = shiftAnchors(draft, text, start, end, anchor);
    if (next.anchors.includes(anchor)) return next;
  }
  return null;
}

/**
 * Finalize with EOF as a boundary. Send `body.text` verbatim: spans index the
 * decoded UTF-8 text before server trim. On failure, do not send or retry
 * without spans (older servers reject the field and would reparse the text).
 */
export function prepareMentionSend(draft: MentionDraft, directory: MentionDirectory): MentionSend {
  const { text } = draft;
  if (/\p{Cs}/u.test(text)) return { ok: false, reason: "invalid-text" };
  const { anchors, code } = anchorSyntax(draft);
  if (anchors.length !== draft.anchors.length) return { ok: false, reason: "invalid-anchor" };
  for (const anchor of anchors) {
    if (!hasRecipient(directory, anchor.mention)) return { ok: false, reason: "stale-recipient" };
  }
  const matches = inspect({ text, anchors }, directory, true, code);
  const ambiguous = matches.find(m => m.kind === "ambiguous");
  if (ambiguous) return { ok: false, reason: "ambiguous", match: ambiguous };
  const resolved = matches.filter(m => m.kind === "resolved");
  if (resolved.length > MAX_MENTION_SPANS) return { ok: false, reason: "too-many-spans" };
  if (!resolved.length) return { ok: true, body: { text } };
  const mentions: WireRecipient[] = [];
  const spans: WireMentionSpan[] = [];
  const indexes = new Map<string, number>();
  const encoder = new TextEncoder();
  let offset = 0;
  let previousEnd = 0;
  for (const { start, end, mention } of resolved) {
    const id = key(mention);
    let index = indexes.get(id);
    if (index === undefined) {
      if (mentions.length === MAX_MENTION_RECIPIENTS) return { ok: false, reason: "too-many-recipients" };
      index = mentions.length;
      indexes.set(id, index);
      mentions.push(mention.kind === "user" ? { kind: "user", id: mention.id }
        : mention.agentId === undefined ? { kind: "agent" } : { kind: "agent", agent_id: mention.agentId });
    }
    const byteStart = offset + encoder.encode(text.slice(previousEnd, start)).length;
    offset = byteStart + encoder.encode(text.slice(start, end)).length;
    spans.push({ start: byteStart, end: offset, index });
    previousEnd = end;
  }
  return { ok: true, body: { text, mentions, spans } };
}
