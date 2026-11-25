/** Half-open ranges in the editor's UTF-16 code units, not wire bytes. */
export type MentionRange = { readonly start: number; readonly end: number };

export const MAX_MENTION_LABEL = 64;

// Match the server's token punctuation and Rust char::is_whitespace. In
// particular, JS's \s omits U+0085 and includes U+FEFF.
const space = /\p{White_Space}/u;
const sharedOpen = /[\p{White_Space}([{<,;:*]/u;
const mentionOnlyOpen = /["'!?]/;
const trailing = /[.,;:!?)\]}>"'*]/u;
const badLabel = /[\p{Cc}\p{Cs}\u2028\u2029]/u;

export function opensMention(text: string, start: number): boolean {
  return start === 0 || sharedOpen.test(text[start - 1]) || mentionOnlyOpen.test(text[start - 1]);
}

function escaped(text: string, i: number, opaque: readonly MentionRange[]): boolean {
  let before = i - 1;
  while (before >= 0 && text[before] === "\\" && !overlapsRanges({ start: before, end: before + 1 }, opaque)) before--;
  return (i - before) % 2 === 0;
}

function visibleQuote(text: string, i: number, opaque: readonly MentionRange[]): boolean {
  return !overlapsRanges({ start: i, end: i + 1 }, opaque) && !escaped(text, i, opaque);
}

type QuoteLookback = number | MentionRange | null;
const lineBreak = /[\r\n\u0085\u2028\u2029]/u;

function visibleQuoteBefore(
  text: string, before: number, floor: number, quote: string, opaque: readonly MentionRange[],
): number | "line-break" | null {
  for (let i = before - 1; i >= floor; i--) {
    if (lineBreak.test(text[i])) return "line-break";
    if (text[i] === quote && visibleQuote(text, i, opaque)) return i;
  }
  return null;
}

function opaqueBefore(before: number, opaque: readonly MentionRange[]): MentionRange | null {
  let boundary: MentionRange | null = null;
  for (const range of opaque) {
    if (range.start >= before) continue;
    const end = Math.min(range.end, before);
    if (!boundary || end > boundary.end || (end === boundary.end && range.start < boundary.start)) {
      boundary = { start: range.start, end };
    }
  }
  return boundary;
}

function previousQuote(text: string, before: number, opaque: readonly MentionRange[]): QuoteLookback {
  const boundary = opaqueBefore(before, opaque);
  // Code and selected labels split the line into quote-pairing regions.
  // A hidden delimiter must not join visible quotes on opposite sides.
  const prior = visibleQuoteBefore(text, before, boundary?.end ?? 0, text[before], opaque);
  if (prior !== null) return prior === "line-break" ? null : prior;
  return boundary && !lineBreak.test(text[boundary.end - 1]) ? boundary : null;
}

function startsEmailToken(text: string, start: number, opaque: readonly MentionRange[]): boolean {
  // The right edge of opaque text can start a new token without a space.
  if (start === 0 || sharedOpen.test(text[start - 1]) || opaque.some(range => range.end === start)) return true;
  if (text[start - 1] !== ">") return false;
  // A no-space `>` is an email boundary only at a Markdown line prefix.
  // Do not admit a prose closing angle bracket or widen wire mention spans.
  let prefix = start - 1;
  while (prefix > 0 && /[> \t]/.test(text[prefix - 1])) prefix--;
  return (prefix === 0 || /[\r\n]/.test(text[prefix - 1]))
    && /^ {0,3}>(?:(?: {0,4}|\t {0,3})>)*$/.test(text.slice(prefix, start));
}

function openingUnderscores(text: string, quote: number, opaque: readonly MentionRange[]): number {
  // Underscore emphasis can wrap mail, but is not wire mention punctuation:
  // underscores are legal in handles and the server treats them as part of one.
  let start = quote;
  while (start > 0 && text[start - 1] === "_") start--;
  return start < quote && startsEmailToken(text, start, opaque) ? quote - start : 0;
}

function startsQuotedToken(text: string, start: number, opaque: readonly MentionRange[]): boolean {
  // Narrower than a mention boundary: the quote in `hello!"`, for example,
  // closes prose rather than starting a new local-part token.
  return startsEmailToken(text, start, opaque) || openingUnderscores(text, start, opaque) > 0;
}

function endsDelimitedToken(text: string, end: number): boolean {
  return end === text.length || space.test(text[end]) || trailing.test(text[end]);
}

function quotedLocalBoundary(text: string, close: number, opaque: readonly MentionRange[]): QuoteLookback | undefined {
  if (close < 0 || close >= text.length) return undefined;
  const quote = text[close];
  if ((quote !== '"' && quote !== "'") || !visibleQuote(text, close, opaque)) return undefined;
  const start = previousQuote(text, close, opaque);
  // An unescaped inner quote makes this local part incomplete; do not
  // skip it to grab an earlier quote with a better boundary.
  return typeof start === "number" && !startsQuotedToken(text, start, opaque) ? null : start;
}

/** Find a complete local part ending at this quote, without pairing unrelated prose quotes. */
export function quotedLocalStart(text: string, close: number, opaque: readonly MentionRange[]): number | null {
  const start = quotedLocalBoundary(text, close, opaque);
  return typeof start === "number" ? start : null;
}

function proseCloserShape(text: string, start: number): boolean {
  const before = text[start - 1];
  return /[,;:]/.test(before) || (space.test(before) && space.test(text[start + 1]));
}

function couldCloseProse(text: string, start: number, opaque: readonly MentionRange[]): boolean {
  if (!proseCloserShape(text, start)) return false;
  const earlier = previousQuote(text, start, opaque);
  // The preceding quote may already close a local part, as in
  // `"first"@host," second"@host`; it cannot open prose at the separator.
  return typeof earlier === "number" && startsQuotedToken(text, earlier, opaque) && text[earlier + 1] !== "@";
}

function interruptedCandidateStart(
  text: string, close: number, opaque: readonly MentionRange[], confirmed: readonly MentionRange[],
): number | null {
  const boundary = opaqueBefore(close, opaque);
  if (!boundary) return null;
  // Don't guess which of several same-line quotes belongs to prose. If a
  // local part could cross an opaque region, ask before addressing any
  // agent from the earliest plausible opener to the putative closing @.
  // This is never a confirmed email and does not hide a person's mention.
  let start: number | null = null;
  for (let i = close - 1; i >= 0 && !lineBreak.test(text[i]); i--) {
    if (i < boundary.start && text[i] === text[close] && visibleQuote(text, i, opaque) && startsQuotedToken(text, i, opaque)
      && !overlapsRanges({ start: i, end: i + 1 }, confirmed)) start = i;
  }
  return start;
}

function quotedEmailSyntax(text: string, opaque: readonly MentionRange[]) {
  const quoted: MentionRange[] = [];
  const uncertain: MentionRange[] = [];
  // A domain can be a single label or a bracketed literal. Stop at its
  // immediate separator; a following address may begin with another quote.
  const domain = /[\p{L}\p{N}][\p{L}\p{N}\p{M}.-]*|\[[\p{L}\p{N}:][\p{L}\p{N}.:%_-]*\]/uy;
  const domainLabel = /^[\p{L}\p{N}](?:[\p{L}\p{N}\p{M}-]*[\p{L}\p{N}\p{M}])?$/u;
  for (const match of text.matchAll(/["']@/gu)) {
    const close = match.index!;
    const local = quotedLocalBoundary(text, close, opaque);
    if (local === undefined) continue;
    domain.lastIndex = close + 2;
    const host = domain.exec(text)?.[0];
    // A leading underscore or hyphen can start an agent handle, not a host.
    if (!host && !/[_-]/.test(text[close + 2])) continue;
    let interrupted: number | null | undefined;
    if (host && typeof local === "number") {
      const end = domain.lastIndex;
      const completeHost = host.startsWith("[") || host.replace(/\.$/, "").split(".").every(label => domainLabel.test(label));
      const underscores = text[end] === "_" ? openingUnderscores(text, local, opaque) : 0;
      const closesEmphasis = underscores > 0 && text.startsWith("_".repeat(underscores), end)
        && endsDelimitedToken(text, end + underscores);
      const boundary = endsDelimitedToken(text, end) || closesEmphasis;
      if (completeHost && boundary && !couldCloseProse(text, local, opaque)
        && !(text[end] === text[close] && visibleQuote(text, end, opaque))) {
        if (proseCloserShape(text, local)) interrupted = interruptedCandidateStart(text, close, opaque, quoted);
        if (interrupted == null) {
          // Use the original @ even when it starts an anchor: an adjoining
          // edit can turn a formerly standalone mention into an email domain.
          quoted.push({ start: local, end: close + 2 });
          continue;
        }
      }
    }
    interrupted ??= interruptedCandidateStart(text, close, opaque, quoted);
    if (local !== null || interrupted !== null) {
      uncertain.push({ start: Math.min(typeof local === "number" ? local : close, interrupted ?? close), end: close + 2 });
    }
  }
  return { quoted, uncertain };
}

export function closesMention(rest: string): boolean {
  let end = 0;
  while (end < rest.length && trailing.test(rest[end])) end++;
  return end === rest.length || space.test(rest[end]);
}

export function trimTrailingMentionPunctuation(token: string): string {
  let end = token.length;
  while (end > 0 && trailing.test(token[end - 1])) end--;
  return token.slice(0, end);
}

export function saneExplicitLabel(label: string): boolean {
  if (!label.length || space.test(label[0]) || space.test(label[label.length - 1])
    || badLabel.test(label) || [...label].length > MAX_MENTION_LABEL || label.endsWith("@")) return false;
  // B-G allows only interior @ marks that cannot start a separate token.
  for (let i = label.indexOf("@"); i !== -1; i = label.indexOf("@", i + 1)) {
    if (opensMention(label, i)) return false;
  }
  return true;
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
  const { quoted, uncertain } = quotedEmailSyntax(text, quoteOpaque);
  return { quoteOpaque, ignored: [...code, ...quoted], uncertain };
}

export function overlapsRanges(range: MentionRange, ranges: readonly MentionRange[]): boolean {
  return ranges.some(({ start, end }) => start < range.end && range.start < end);
}

export function validMentionSpan(text: string, range: MentionRange): boolean {
  const { start, end } = range;
  return isRange(text, range) && text[start] === "@" && opensMention(text, start)
    && saneExplicitLabel(text.slice(start + 1, end)) && closesMention(text.slice(end));
}
