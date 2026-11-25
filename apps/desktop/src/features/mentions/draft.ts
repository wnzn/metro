import {
  closesMention, isAgentHandle, isLegacyId, isMemberId, isRange, isTextBoundary, mentionCodeRanges,
  opensMention, overlapsRanges, quotedEmailRanges,
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
type MentionDecision =
  | (MentionSelection & { kind: "ambiguous"; choices: readonly MentionRecipient[] })
  | (MentionSelection & { kind: "unresolved" });
export type MentionDraft = {
  readonly text: string;
  readonly anchors: readonly MentionAnchor[];
  /** Completed unbound tokens stay plain or require a pick even after roster refresh. */
  readonly decisions?: readonly MentionDecision[];
};
export type MentionMatch = (MentionAnchor & { kind: "resolved" }) | MentionDecision;

type WireRecipient = { kind: "agent"; agent_id?: string } | { kind: "user"; id: string };
export type WireMentionSpan = { start: number; end: number; index: number };
type MentionBody = {
  text: string; mentions?: WireRecipient[]; spans?: WireMentionSpan[];
  invoke_agent?: true; agent_id?: string;
};
export type MentionSend =
  | { ok: true; body: MentionBody }
  | { ok: false; reason: "ambiguous"; match: Extract<MentionMatch, { kind: "ambiguous" }> }
  | { ok: false; reason: "invalid-text" | "invalid-anchor" | "stale-recipient" | "too-many-spans" | "too-many-recipients" };

const generic: MentionRecipient = { kind: "agent" };
const isGenericLabel = (label: string) => label.toLowerCase() === "agent";

/** Stable across roster ordering, re-labeling and colliding names; use for chooser focus. */
export function mentionIdentityKey(mention: MentionRecipient): string {
  return mention.kind === "user" ? `user:${mention.id}`
    : mention.agentId === undefined ? "generic" : `agent:${mention.agentId}`;
}

function hasRecipient(directory: MentionDirectory, mention: MentionRecipient): boolean {
  return mention.kind === "user" ? directory.members.some(m => m.id === mention.id)
    : mention.agentId === undefined || directory.agents.some(a => a.id === mention.agentId);
}

type Alias = { label: string; flags: "i" | "iu"; match: RegExp; priority: number; mention: MentionRecipient };
const quotePattern = (literal: string) => literal.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");

function aliases(directory: MentionDirectory): Alias[] {
  const out: Alias[] = [];
  function add(label: string, priority: number, mention: MentionRecipient, ascii = false) {
    if (!saneTypedLabel(label)) return;
    const flags = ascii ? "i" : "iu";
    out.push({ label, flags, match: new RegExp(`^${quotePattern(label)}`, flags), priority, mention });
  }
  add("agent", 0, generic, true);
  for (const a of directory.agents) {
    const mention = { kind: "agent", agentId: a.id } as const;
    if (isAgentHandle(a.handle) && !isGenericLabel(a.handle)) add(a.handle, 1, mention, true);
    if (isMemberId(a.id)) add(a.id, 1, mention, isLegacyId(a.id));
    add(a.name, 3, mention);
  }
  for (const m of directory.members) {
    const mention = { kind: "user", id: m.id } as const;
    if (isMemberId(m.id)) add(m.id, 2, mention, isLegacyId(m.id));
    if (m.displayName) add(m.displayName, 3, mention);
  }
  return out;
}

function anchorSyntax(draft: MentionDraft) {
  const { text } = draft;
  const valid = (s: MentionSelection) => validMentionSpan(text, s) && text.slice(s.start, s.end) === s.literal;
  const email = quotedEmailRanges(text);
  let end = 0;
  let anchors = [...draft.anchors].sort((a, b) => a.start - b.start).filter(a => {
    if (a.start < end || !valid(a)) return false;
    end = a.end;
    return true;
  });
  // Delimiters inside selected labels do not affect other tokens. If an
  // edit wrapped some anchors in real code, drop them and unmask their text
  // before checking the remaining anchors.
  for (;;) {
    const ignored = [...email, ...mentionCodeRanges(text, anchors)];
    const outside = anchors.filter(a => !overlapsRanges(a, ignored));
    if (outside.length === anchors.length) {
      end = 0;
      const decisions = [...(draft.decisions ?? [])].sort((a, b) => a.start - b.start).filter(d => {
        if (d.start < end || !valid(d) || overlapsRanges(d, ignored) || overlapsRanges(d, anchors)) return false;
        end = d.end;
        return true;
      });
      return { anchors, decisions, ignored };
    }
    anchors = outside;
  }
}

type CaptureOptions = { finalize?: boolean; caret?: number };

function canExtend(rest: string, lookup: readonly Alias[]): boolean {
  return lookup.some(({ label, flags }) => label.length > rest.length && isTextBoundary(label, rest.length)
    && new RegExp(`^${quotePattern(label.slice(0, rest.length))}$`, flags).test(rest));
}

function compareChoices(a: Alias, b: Alias): number {
  return a.priority - b.priority || compareIdentities(a.mention, b.mention);
}

function compareIdentities(a: MentionRecipient, b: MentionRecipient): number {
  const x = mentionIdentityKey(a), y = mentionIdentityKey(b);
  return x < y ? -1 : x > y ? 1 : 0;
}

function inspect(
  draft: MentionDraft, directory: MentionDirectory, ignored: readonly MentionRange[],
  { finalize = false, caret = draft.text.length }: CaptureOptions = {},
): MentionMatch[] {
  const { text } = draft;
  const saved = new Map<number, MentionMatch>(draft.anchors.map(a => [a.start, { ...a, kind: "resolved" }]));
  for (const decision of draft.decisions ?? []) saved.set(decision.start, decision);
  const lookup = aliases(directory);
  const matches: MentionMatch[] = [];
  let coveredUntil = 0;
  for (let start = text.indexOf("@"); start !== -1; start = text.indexOf("@", start + 1)) {
    if (start < coveredUntil) continue;
    const previous = saved.get(start);
    if (previous) {
      matches.push(previous);
      coveredUntil = previous.end;
      continue;
    }
    if (!opensMention(text, start) || overlapsRanges({ start, end: start + 1 }, ignored)) continue;
    const rest = text.slice(start + 1);
    let longest = 0;
    let candidates: Alias[] = [];
    for (const alias of lookup) {
      const label = alias.match.exec(rest)?.[0];
      if (!label || label.length < longest || !closesMention(rest.slice(label.length))
        || overlapsRanges({ start, end: start + 1 + label.length }, ignored)) continue;
      if (label.length > longest) {
        longest = label.length;
        candidates = [];
      }
      candidates.push(alias);
    }
    // A space (or punctuation) can end a short alias but also start a
    // continuation. Defer that occurrence only at the active typing edge.
    if (!finalize && caret > start + 1 && caret >= start + 1 + longest && isTextBoundary(text, caret)
      && canExtend(text.slice(start + 1, caret), lookup)) continue;
    if (longest) {
      const end = start + 1 + longest;
      if (!finalize && end === text.length) continue;
      const literal = text.slice(start, end);
      // Use priority to order choices, never to break a collision between
      // distinct identities. The exact reserved token is the only exception.
      const choices = new Map<string, MentionRecipient>();
      for (const { mention } of isGenericLabel(literal.slice(1)) ? [{ mention: generic }]
        : candidates.sort(compareChoices)) {
        choices.set(mentionIdentityKey(mention), mention);
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
    if (saneTypedLabel(label) && (finalize || end < text.length)
      && !overlapsRanges({ start, end }, ignored)) {
      matches.push({ kind: "unresolved", start, end, literal: text.slice(start, end) });
    }
  }
  return matches;
}

function bindings(draft: MentionDraft, anchors: readonly MentionAnchor[], decisions: readonly MentionDecision[]): MentionDraft {
  return decisions.length ? { text: draft.text, anchors, decisions } : { text: draft.text, anchors };
}

/** Read-only preview. Supply the UTF-16 caret if it is not at the end of the draft. */
export function scanMentionDraft(
  draft: MentionDraft, directory: MentionDirectory, options?: CaptureOptions,
): MentionMatch[] {
  const { anchors, decisions, ignored } = anchorSyntax(draft);
  return inspect(bindings(draft, anchors, decisions), directory, ignored, options);
}

/**
 * After `editMentionDraft` or `updateMentionDraft`, store this result per
 * room/thread using its loaded directory. Do not recapture on roster refresh.
 * Before a send attempt, store one capture with `finalize` to commit the
 * open token too, then run preflight against the current directory.
 * Existing decisions, including ambiguity and plain text, are not revised
 * by roster data; edit the token or explicitly pick a current identity.
 */
export function captureTypedMentions(
  draft: MentionDraft, directory: MentionDirectory, options?: CaptureOptions,
): MentionDraft {
  const { anchors, decisions, ignored } = anchorSyntax(draft);
  if (anchors.length !== draft.anchors.length || decisions.length !== (draft.decisions?.length ?? 0)) return draft;
  const known = new Set([...anchors, ...decisions].map(a => a.start));
  const nextAnchors = [...anchors];
  const nextDecisions = [...decisions];
  for (const match of inspect(bindings(draft, anchors, decisions), directory, ignored, options)) {
    if (known.has(match.start)) continue;
    if (match.kind === "resolved") {
      const { start, end, literal, mention } = match;
      nextAnchors.push({ start, end, literal, mention });
    } else nextDecisions.push(match);
  }
  if (nextAnchors.length === anchors.length && nextDecisions.length === decisions.length) return draft;
  return bindings(draft, nextAnchors.sort((a, b) => a.start - b.start), nextDecisions.sort((a, b) => a.start - b.start));
}

type Edit = MentionRange & { readonly replacement: string };

function shiftAnchors(
  draft: MentionDraft, text: string, firstStart: number, lastEnd: number, picked?: MentionAnchor,
): MentionDraft {
  const delta = text.length - draft.text.length;
  const move = <T extends MentionSelection>(items: readonly T[]): T[] => items.flatMap(item => {
    // Insertions at an edge are outside the anchor. Adjacency changes still
    // need validation (e.g. adding '\' before @ or a letter after the name).
    if (item.end <= firstStart) return [item];
    if (item.start >= lastEnd) return [{ ...item, start: item.start + delta, end: item.end + delta }];
    return [];
  });
  const anchors = move(draft.anchors);
  if (picked) anchors.push(picked);
  const candidate = { text, anchors, decisions: move(draft.decisions ?? []) };
  const valid = anchorSyntax(candidate);
  return bindings(candidate, valid.anchors, valid.decisions);
}

/** Use an exact native edit if available. Replacing inside an anchor drops it even for identical text. */
export function editMentionDraft(draft: MentionDraft, edit: Edit): MentionDraft | null {
  if (!isRange(draft.text, edit)) return null;
  const text = draft.text.slice(0, edit.start) + edit.replacement + draft.text.slice(edit.end);
  return shiftAnchors(draft, text, edit.start, edit.end);
}

function retainUncertainDecisions(
  before: MentionDraft, after: MentionDraft, firstStart: number, lastEnd: number,
): MentionDraft {
  const uncertain = new Map<string, Map<string, MentionRecipient>>();
  for (const item of [...before.anchors, ...(before.decisions ?? [])]) {
    if (item.end <= firstStart || item.start >= lastEnd) continue;
    const choices = uncertain.get(item.literal) ?? new Map<string, MentionRecipient>();
    for (const m of "mention" in item ? [item.mention] : item.kind === "ambiguous" ? item.choices : []) {
      choices.set(mentionIdentityKey(m), m);
    }
    uncertain.set(item.literal, choices);
  }
  if (!uncertain.size) return after;
  const { text, anchors } = after;
  const decisions = [...(after.decisions ?? [])];
  const ignored = [...quotedEmailRanges(text), ...mentionCodeRanges(text, anchors)];
  const changedEnd = lastEnd + text.length - before.text.length;
  // An identical spelling may have survived a broad snapshot diff or may
  // have been pasted anew. Never guess its identity from the new roster.
  // Prefer the longest affected spelling so a partial token cannot take it.
  const literals = [...uncertain].sort(([a], [b]) => b.length - a.length);
  for (const [literal, ids] of literals) {
    const choices = [...ids.values()].sort(compareIdentities);
    for (let start = text.indexOf(literal, Math.max(0, firstStart - literal.length + 1));
      start !== -1 && start < changedEnd; start = text.indexOf(literal, start + literal.length)) {
      const end = start + literal.length;
      const range = { start, end, literal };
      if (!validMentionSpan(text, range) || overlapsRanges(range, ignored)
        || overlapsRanges(range, anchors) || overlapsRanges(range, decisions)) continue;
      decisions.push(choices.length ? { ...range, kind: "ambiguous", choices } : { ...range, kind: "unresolved" });
    }
  }
  return bindings(after, anchors, decisions.sort((a, b) => a.start - b.start));
}

/**
 * For a value-only onChange, preserve anchors outside every minimal single
 * replacement. Repeated text can hide which occurrence was edited: never
 * transfer a chosen identity or resolve against a new roster by guessing.
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
  const firstStart = Math.min(prefix, limit - suffix);
  const lastEnd = before.length - Math.min(suffix, limit - prefix);
  const shifted = shiftAnchors(draft, text, firstStart, lastEnd);
  return retainUncertainDecisions(draft, shifted, firstStart, lastEnd);
}

function displayLabels(directory: MentionDirectory, mention: MentionRecipient): string[] {
  if (mention.kind === "user") {
    return directory.members.filter(m => m.id === mention.id)
      .flatMap(m => [m.displayName ?? "", m.id]);
  }
  if (mention.agentId === undefined) return ["agent"];
  return directory.agents.filter(a => a.id === mention.agentId)
    .flatMap(a => [a.name, ...(isAgentHandle(a.handle) && !isGenericLabel(a.handle) ? [a.handle] : []), a.id]);
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
    || overlapsRanges(selection, anchorSyntax(draft).ignored))) return null;
  if (draft.anchors.some(a => append ? a.start < start && start < a.end
    : start < a.end && a.start < end && !(start === a.start && end === a.end))) return null;
  const wouldCloseQuote = append && /["']/.test(draft.text[start - 1])
    && quotedEmailRanges(`${draft.text.slice(0, start)}@`).some(r => r.end === start + 1);
  const prefix = append && (!opensMention(draft.text, start) || wouldCloseQuote) ? " " : "";
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
 * Use the persisted draft, with EOF as a boundary. Send `body.text` verbatim:
 * spans index UTF-8 bytes before server trim. The API caller must add
 * `infer_text_mentions: false` on every Space send, even without mentions
 * or spans. Legacy inference differs (e.g. quoted emails, changed aliases
 * and leading hyphen/underscore handles); never replay a rejected request
 * without the authoritative opt-in or without spans.
 */
export function prepareMentionSend(
  draft: MentionDraft, directory: MentionDirectory,
  { askAgent }: { askAgent?: { agentId?: string } } = {},
): MentionSend {
  const { text } = draft;
  if (/\p{Cs}/u.test(text)) return { ok: false, reason: "invalid-text" };
  const { anchors, decisions, ignored } = anchorSyntax(draft);
  if (anchors.length !== draft.anchors.length || decisions.length !== (draft.decisions?.length ?? 0)) {
    return { ok: false, reason: "invalid-anchor" };
  }
  for (const anchor of anchors) {
    if (!hasRecipient(directory, anchor.mention)) return { ok: false, reason: "stale-recipient" };
  }
  const ask: MentionRecipient | undefined = askAgent && { kind: "agent", agentId: askAgent.agentId };
  if (ask && !hasRecipient(directory, ask)) return { ok: false, reason: "stale-recipient" };
  const matches = inspect(bindings(draft, anchors, decisions), directory, ignored, { finalize: true });
  const ambiguous = matches.find(m => m.kind === "ambiguous");
  if (ambiguous) return { ok: false, reason: "ambiguous", match: ambiguous };
  const resolved = matches.filter(m => m.kind === "resolved");
  if (resolved.length > MAX_MENTION_SPANS) return { ok: false, reason: "too-many-spans" };
  const body: MentionBody = { text };
  if (askAgent) {
    body.invoke_agent = true;
    if (askAgent.agentId !== undefined) body.agent_id = askAgent.agentId;
  }
  if (!resolved.length) return { ok: true, body };
  const mentions: WireRecipient[] = [];
  const spans: WireMentionSpan[] = [];
  const indexes = new Map<string, number>();
  const encoder = new TextEncoder();
  let offset = 0;
  let previousEnd = 0;
  for (const { start, end, mention } of resolved) {
    const id = mentionIdentityKey(mention);
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
  if (ask && !indexes.has(mentionIdentityKey(ask)) && mentions.length === MAX_MENTION_RECIPIENTS) {
    return { ok: false, reason: "too-many-recipients" };
  }
  return { ok: true, body: { ...body, mentions, spans } };
}
