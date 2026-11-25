/** Half-open ranges in the editor's UTF-16 code units, not wire bytes. */
export type MentionRange = { readonly start: number; readonly end: number };

export const MAX_MENTION_LABEL = 64;

// Match the server's token punctuation and Rust char::is_whitespace. In
// particular, JS's \s omits U+0085 and includes U+FEFF.
const space = /\p{White_Space}/u;
const opens = /[\p{White_Space}([{<"',;:!?*]/u;
const trailing = /^[.,;:!?)\]}>"'*]*/u;
const badLabel = /[\p{Cc}\p{Cs}\u2028\u2029]/u;

export function opensMention(text: string, start: number): boolean {
  return start === 0 || opens.test(text[start - 1]);
}

function escaped(text: string, i: number, opaque: readonly MentionRange[]): boolean {
  let before = i - 1;
  while (before >= 0 && text[before] === "\\" && !overlapsRanges({ start: before, end: before + 1 }, opaque)) before--;
  return (i - before) % 2 === 0;
}

function visibleQuote(text: string, i: number, opaque: readonly MentionRange[]): boolean {
  return !overlapsRanges({ start: i, end: i + 1 }, opaque) && !escaped(text, i, opaque);
}

/** Find a complete local part ending at this quote, without pairing unrelated prose quotes. */
export function quotedLocalStart(text: string, close: number, opaque: readonly MentionRange[]): number | null {
  if (close < 1 || close >= text.length) return null;
  const quote = text[close];
  if ((quote !== '"' && quote !== "'") || !visibleQuote(text, close, opaque)) return null;
  for (let i = close - 1; i >= 0; i--) {
    if (/[\r\n\u0085\u2028\u2029]/u.test(text[i])) break;
    if (text[i] === quote && visibleQuote(text, i, opaque)) {
      // An unescaped inner quote makes this local part incomplete; do
      // not skip it to grab an earlier quote with a better boundary.
      return opensMention(text, i) ? i : null;
    }
  }
  return null;
}

function quotedEmailRanges(text: string, opaque: readonly MentionRange[]): MentionRange[] {
  const quoted: MentionRange[] = [];
  // A domain can be a single label or a bracketed literal. This is only
  // a plausibility check, not full email validation.
  const domain = /[\p{L}\p{N}][\p{L}\p{N}\p{M}.-]*|\[[\p{L}\p{N}:][\p{L}\p{N}.:%_-]*\]/uy;
  for (const match of text.matchAll(/["']@/gu)) {
    const close = match.index!;
    domain.lastIndex = close + 2;
    if (!domain.test(text)) continue;
    // Give an ordinary quoted mention its own closing quote. Otherwise
    // `"Hello!" "@coder"` could look like a local part starting at `!"`.
    const end = domain.lastIndex;
    const punctuation = /^[.,;:!?]*/.exec(text.slice(end))![0].length;
    const after = end + punctuation;
    if (text[after] === text[close] && visibleQuote(text, after, opaque)) continue;
    const start = quotedLocalStart(text, close, opaque);
    // Use the original @ even when it starts an anchor: an adjoining
    // edit can turn a formerly standalone mention into an email domain.
    if (start !== null) quoted.push({ start, end: close + 2 });
  }
  return quoted;
}

export function closesMention(rest: string): boolean {
  const end = rest.match(trailing)![0].length;
  return end === rest.length || space.test(rest[end]);
}

export function saneExplicitLabel(label: string): boolean {
  return label.length > 0 && !space.test(label[0]) && !space.test(label[label.length - 1])
    && !badLabel.test(label) && [...label].length <= MAX_MENTION_LABEL
    // B-G allows only interior @ marks that cannot start a separate token.
    && !label.startsWith("@") && !label.endsWith("@") && !/[\p{White_Space}([{<"',;:!?*]@/u.test(label);
}

export function saneTypedLabel(label: string): boolean {
  return saneExplicitLabel(label) && !/[@`]/.test(label);
}

export function isLegacyId(label: string): boolean {
  return /^[a-z0-9][a-z0-9._-]{0,63}$/i.test(label) && !badLabel.test(label);
}

export function isAgentHandle(label: string): boolean {
  // Space agent creation accepts leading underscores and hyphens even
  // though the legacy server text scanner does not recognize them.
  return /^[a-z0-9_-]{1,32}$/i.test(label) && !badLabel.test(label);
}

export function isMemberId(label: string): boolean {
  return saneTypedLabel(label) && !space.test(label);
}

export function isTextBoundary(text: string, i: number): boolean {
  return Number.isInteger(i) && i >= 0 && i <= text.length
    && !(text.charCodeAt(i) >= 0xdc00 && text.charCodeAt(i) <= 0xdfff
      && text.charCodeAt(i - 1) >= 0xd800 && text.charCodeAt(i - 1) <= 0xdbff);
}

export function isRange(text: string, range: MentionRange): boolean {
  return range.start <= range.end && isTextBoundary(text, range.start) && isTextBoundary(text, range.end);
}

function codeRanges(text: string, offset: number): MentionRange[] {
  // Fenced blocks use the same rules as the server's legacy scan, including
  // leaving an unclosed fence in effect until the end.
  const fenced: MentionRange[] = [];
  let fence: { ch: string; length: number; start: number } | undefined;
  let pos = offset;
  for (const line of text.split("\n")) {
    const start = pos;
    pos += line.length + 1;
    const match = /^ {0,3}(`{3,}|~{3,})/.exec(line);
    if (!match) continue;
    const run = match[1];
    if (!fence) fence = { ch: run[0], length: run.length, start };
    else if (run[0] === fence.ch && run.length >= fence.length && /^\p{White_Space}*$/u.test(line.slice(match[0].length))) {
      fenced.push({ start: fence.start, end: Math.min(pos, offset + text.length) });
      fence = undefined;
    }
  }
  if (fence) fenced.push({ start: fence.start, end: offset + text.length });

  // Pair runs of the same length. Like the legacy scan, a run inside a
  // fence cannot open an inline span; an unmatched run is literal.
  const runs = [...text.matchAll(/`+/g)];
  const nextEqual = new Map<number, number>();
  const closes: (number | undefined)[] = [];
  for (let i = runs.length - 1; i >= 0; i--) {
    const length = runs[i][0].length;
    closes[i] = nextEqual.get(length);
    nextEqual.set(length, i);
  }
  const ranges = fenced.slice();
  let block = 0;
  for (let i = 0; i < runs.length; i++) {
    const start = offset + runs[i].index!;
    while (block < fenced.length && fenced[block].end <= start) block++;
    if (block < fenced.length && fenced[block].start <= start) continue;
    const close = closes[i];
    if (close !== undefined) {
      ranges.push({ start, end: offset + runs[close].index! + runs[close][0].length });
      i = close;
    }
  }
  return ranges;
}

function mentionCodeRanges(text: string, opaque: readonly MentionRange[]): readonly MentionRange[] {
  // Explicit labels are not Markdown. Use inert non-whitespace placeholders
  // (not indentation) without moving any editor offsets.
  let previousEnd = 0;
  let masked = "";
  for (const { start, end } of opaque) {
    masked += text.slice(previousEnd, start) + "x".repeat(end - start);
    previousEnd = end;
  }
  const source = masked + text.slice(previousEnd);
  // The server parses uncovered text after trim. Leading trim can expose a
  // fence on the first line; use that same view without moving editor offsets.
  const leading = source.match(/^\p{White_Space}+/u)?.[0].length ?? 0;
  return codeRanges(source.slice(leading), leading);
}

export function mentionSyntaxRanges(text: string, anchors: readonly MentionRange[]) {
  const code = mentionCodeRanges(text, anchors);
  // Neither code nor a selected label supplies quotes to the prose parser.
  // Scan the original text around them so UTF-16 ranges and escapes stay intact.
  const quoteOpaque = [...code, ...anchors];
  return { quoteOpaque, ignored: [...code, ...quotedEmailRanges(text, quoteOpaque)] };
}

export function overlapsRanges(range: MentionRange, ranges: readonly MentionRange[]): boolean {
  return ranges.some(({ start, end }) => start < range.end && range.start < end);
}

export function validMentionSpan(text: string, range: MentionRange): boolean {
  const { start, end } = range;
  return isRange(text, range) && text[start] === "@" && opensMention(text, start)
    && saneExplicitLabel(text.slice(start + 1, end)) && closesMention(text.slice(end));
}
