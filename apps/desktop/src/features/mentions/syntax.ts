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

export function closesMention(rest: string): boolean {
  const end = rest.match(trailing)![0].length;
  return end === rest.length || space.test(rest[end]);
}

export function saneExplicitLabel(label: string): boolean {
  return label.length > 0 && !space.test(label[0]) && !space.test(label[label.length - 1])
    && !badLabel.test(label) && [...label].length <= MAX_MENTION_LABEL
    // An interior @ can be literal, but a span must not cover another
    // standalone @. The label's first character is preceded by the leading @.
    && !/[\p{White_Space}([{<"',;:!?*]@/u.test(label);
}

export function saneTypedLabel(label: string): boolean {
  return saneExplicitLabel(label) && !/[@`]/.test(label);
}

export function isHandle(label: string): boolean {
  return /^[a-z0-9][a-z0-9._-]{0,63}$/i.test(label) && !badLabel.test(label);
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

export function mentionCodeRanges(text: string, opaque: readonly MentionRange[] = []): readonly MentionRange[] {
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

export function overlapsCode(range: MentionRange, code: readonly MentionRange[]): boolean {
  return code.some(({ start, end }) => start < range.end && range.start < end);
}

export function validMentionSpan(text: string, range: MentionRange): boolean {
  const { start, end } = range;
  return isRange(text, range) && text[start] === "@" && opensMention(text, start)
    && saneExplicitLabel(text.slice(start + 1, end)) && closesMention(text.slice(end));
}
