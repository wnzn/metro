import type { TextViewBlock, TextViewInline } from "nsh/base";
import { pageRichText, utf8Prefix } from "./pageMarkdown";

export const MAX_MESSAGE_MARKDOWN_BYTES = 32 * 1024;
export const MAX_MESSAGE_MARKDOWN_NODES = 800;

type Span = { start: number; end: number; literal: string };
const marker = /\uE000(\d+)\uE001/g;
// Neither typed markers nor Markdown character references may create a chip.
const clean = (value: string) => value.replace(/[\uE000\uE001]/g, "").replace(/&#(?:0*5734[45]|x0*e00[01]);/gi, "\uFFFD");

export function messageMarkdown(text: string, spans: readonly Span[] = []) {
  const limit = MAX_MESSAGE_MARKDOWN_BYTES;
  const end = utf8Prefix(text, limit).end;
  let limited = end < text.length;
  let cursor = 0, bytes = 0;
  const parts: string[] = [];
  const literals = new Map<string, string>();
  const append = (part: string, atomic = false) => {
    const taken = utf8Prefix(part, limit - bytes);
    if (taken.end < part.length) {
      limited = true;
      if (atomic) return false;
    }
    parts.push(part.slice(0, taken.end));
    bytes += taken.bytes;
    return taken.end === part.length;
  };
  for (const [ordinal, span] of spans.entries()) {
    if (span.start >= end) break;
    if (!append(clean(text.slice(cursor, span.start)))) { cursor = end; break; }
    cursor = end;
    // Never show half a chip at a fold or a byte boundary.
    if (span.end > end || !append(`\uE000${ordinal}\uE001`, true)) break;
    literals.set(String(ordinal), clean(span.literal));
    cursor = span.end;
  }
  append(clean(text.slice(cursor, end)));

  const rich = pageRichText(limit, MAX_MESSAGE_MARKDOWN_NODES);
  const document = rich.document(parts);
  const usage = rich.usage();
  limited ||= usage.limited;
  let remaining = MAX_MESSAGE_MARKDOWN_NODES - usage.nodes;
  const literal = (value: string) => value.replace(marker, (_, ordinal) => literals.get(ordinal) ?? "");
  const mentions = (value: string): TextViewInline[] => {
    if (!value.includes("\uE000")) return [{ type: "text", text: value }];
    const out: TextViewInline[] = [];
    let start = 0;
    for (const match of value.matchAll(marker)) {
      if (match.index > start) out.push({ type: "text", text: value.slice(start, match.index) });
      const text = literals.get(match[1]);
      if (text !== undefined) out.push({ type: "custom", name: "mention", attributes: { ordinal: match[1] }, text });
      start = match.index + match[0].length;
    }
    if (start < value.length) out.push({ type: "text", text: value.slice(start) });
    if (out.length - 1 > remaining) {
      limited = true;
      return [{ type: "text", text: literal(value) }];
    }
    remaining -= Math.max(0, out.length - 1);
    return out;
  };
  // Pages supplies the allowlist, depth, and table limits. Charge each added
  // inline against its remaining node budget, and never reintroduce a URL.
  const inlines = (values: TextViewInline[]): TextViewInline[] => values.flatMap(n => {
    switch (n.type) {
      case "text": return mentions(n.text);
      case "code": return [{ ...n, code: literal(n.code) }];
      case "strong": case "emphasis": case "strikethrough":
        return [{ ...n, content: inlines(n.content) }];
      case "link": {
        // A marker parsed as part of the destination must not become a link.
        if (/%ee%80%8[01]|[\uE000\uE001]/i.test(n.url)) return inlines(n.content);
        // An identity chip is itself a link. Don't nest interactive controls.
        if (hasMention(n.content)) return inlines(n.content);
        return [{ ...n, content: inlines(n.content) }];
      }
      default: return [n];
    }
  });
  const hasMention = (values: TextViewInline[]): boolean => values.some(n =>
    n.type === "text" ? n.text.includes("\uE000") : "content" in n && !!n.content && hasMention(n.content));
  const blocks = (values: TextViewBlock[]): TextViewBlock[] => values.map(n => {
    switch (n.type) {
      case "heading": return { ...n, level: Math.min(n.level, 3) as 1 | 2 | 3, content: inlines(n.content) };
      case "paragraph": return { ...n, content: inlines(n.content) };
      case "blockquote": return { ...n, children: blocks(n.children) };
      case "list": return { ...n, items: n.items.map(item => ({ children: blocks(item.children) })) };
      case "table": {
        const row = (cells: typeof n.head) => cells.map(cell => ({ content: inlines(cell.content) }));
        return { ...n, head: row(n.head), rows: n.rows?.map(row) };
      }
      case "codeBlock": return { ...n, code: literal(n.code), wrap: true };
      default: return n;
    }
  });
  const projected = document && { blocks: blocks(document.blocks) };
  return { document: projected, limited };
}
