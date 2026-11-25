import { TextView } from "nsh/base";
import type { TextViewBlock, TextViewDocument, TextViewInline } from "nsh/base";

export const MAX_PAGE_MARKDOWN_BYTES = 128 * 1024;
export const MAX_PAGE_MARKDOWN_NODES = 2_500;
const MAX_DEPTH = 12;

export function httpsLink(url: string): string | null {
  if (!/^https:\/\/[^/\s\\]/i.test(url) || /[\s\\\p{Cc}]/u.test(url)) return null;
  try {
    const parsed = new URL(url);
    return parsed.protocol === "https:" && parsed.hostname && !parsed.username && !parsed.password ? parsed.href : null;
  } catch { return null; }
}

/** Scan only the prefix we might parse. A surrogate pair is one code point;
 * a lone surrogate takes 3 bytes (UTF-8's replacement character). */
export function utf8Prefix(text: string, available: number) {
  let bytes = 0, end = 0;
  while (end < text.length) {
    const code = text.charCodeAt(end);
    const low = text.charCodeAt(end + 1);
    const pair = code >= 0xd800 && code <= 0xdbff && low >= 0xdc00 && low <= 0xdfff;
    const width = pair ? 4 : code < 0x80 ? 1 : code < 0x800 ? 2 : 3;
    if (bytes + width > available) break;
    bytes += width;
    end += pair ? 2 : 1;
  }
  return { bytes, end };
}

/** One budget per render plan, charged in render order, including repeats.
 * The input quota is shared before any parser call. Output charges also
 * count document, list item, table row and cell wrappers, not just text. */
export function pageRichText(maxBytes = MAX_PAGE_MARKDOWN_BYTES, maxNodes = MAX_PAGE_MARKDOWN_NODES) {
  let bytes = 0, nodes = 0, limited = false;
  const spend = (depth: number) => {
    if (depth > MAX_DEPTH || nodes >= maxNodes) { limited = true; return false; }
    nodes++;
    return true;
  };
  const sanitizeInlines = (values: TextViewInline[], depth: number): TextViewInline[] => {
    const out: TextViewInline[] = [];
    for (const n of values) {
      if (!spend(depth)) break;
      switch (n.type) {
        case "text": out.push({ type: "text", text: n.text }); break;
        case "code": out.push({ type: "code", code: n.code }); break;
        case "break": out.push({ type: "break" }); break;
        case "strong": case "emphasis": case "strikethrough":
          out.push({ type: n.type, content: sanitizeInlines(n.content, depth + 1) }); break;
        case "link": {
          const content = sanitizeInlines(n.content, depth + 1);
          const url = httpsLink(n.url);
          if (url) out.push({ type: "link", url, content });
          else out.push(...content);
          break;
        }
        case "image": out.push({ type: "text", text: n.alt || "[Image]" }); break;
      }
    }
    return out;
  };
  const sanitizeBlocks = (values: TextViewBlock[], depth: number): TextViewBlock[] => {
    const out: TextViewBlock[] = [];
    for (const n of values) {
      if (!spend(depth)) break;
      switch (n.type) {
        case "paragraph": out.push({ type: "paragraph", content: sanitizeInlines(n.content, depth + 1) }); break;
        case "heading": out.push({ type: "heading", level: n.level, content: sanitizeInlines(n.content, depth + 1) }); break;
        case "blockquote": out.push({ type: "blockquote", children: sanitizeBlocks(n.children, depth + 1) }); break;
        case "list": {
          const items = [];
          for (const item of n.items) {
            if (!spend(depth + 1)) break;
            items.push({ children: sanitizeBlocks(item.children, depth + 2) });
          }
          if (items.length) out.push({ type: "list", ordered: n.ordered, start: n.start, items });
          break;
        }
        case "table": {
          const cells = (row: typeof n.head) => {
            const cols: typeof n.head = [];
            for (const cell of row) {
              if (cols.length === 12) { limited = true; break; }
              if (!spend(depth + 2)) break;
              cols.push({ content: sanitizeInlines(cell.content, depth + 3) });
            }
            return cols;
          };
          if (!spend(depth + 1)) break;
          const head = cells(n.head), rows = [];
          if (!head.length) break;
          for (const row of n.rows ?? []) {
            if (rows.length === 50) { limited = true; break; }
            if (!spend(depth + 1)) break;
            const projected = cells(row);
            if (projected.length < head.length) break;
            rows.push(projected);
          }
          out.push({ type: "table", head, rows });
          break;
        }
        case "rule": out.push({ type: "rule" }); break;
        case "codeBlock": out.push({ type: "codeBlock", code: n.code }); break;
      }
    }
    return out;
  };
  function document(parts: readonly string[]): TextViewDocument | undefined {
    if (!parts.some(Boolean)) return;
    if (bytes >= maxBytes || nodes >= maxNodes) { limited = true; return; }
    const bounded: string[] = [];
    for (const part of parts) {
      const taken = utf8Prefix(part, maxBytes - bytes);
      bytes += taken.bytes;
      if (taken.end) bounded.push(part.slice(0, taken.end));
      if (taken.end < part.length) { limited = true; break; }
    }
    if (!bounded.length || !spend(0)) return;
    // Parse only the bounded prefix, with HTML disabled. No parsed image,
    // custom node, or unsafe destination is copied into the native document.
    const parsed = TextView.parse(bounded.join(""), { html: false });
    return { blocks: sanitizeBlocks(parsed.blocks, 1) };
  }
  return { document, usage: () => ({ bytes, nodes, limited }) };
}
