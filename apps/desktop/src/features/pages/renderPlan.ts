import type { ActionKind, Capability, PageValues, SourceResult } from "../../api/pages";
import type { TextViewDocument } from "nsh/base";
import { pageRichText, utf8Prefix } from "../../ui/pageMarkdown";

// Match pages/spec.rs and pages/tsx.rs, even for a malformed response.
export const MAX_RENDERED = 5_000;
export const MAX_DEPTH = 12;
export const MAX_PAGE_TEXT_BYTES = 128 * 1024;
const MAX_ITEMS = 50;
const MAX_COLUMNS = 12;
const RESERVED = new Set(Object.getOwnPropertyNames(Object.prototype).concat(["prototype"]));
const BLOCKS = new Set([
  "Section", "Row", "Grid", "Divider", "Heading", "Text", "Markdown", "Badge", "Empty", "Stat",
  "Table", "List", "Each", "Show", "Progress", "Chart", "AgentAnswer", "RunList", "MessageFeed", "ScheduleCard",
  "FileList", "Button", "Link",
]);
const ACTIONS = new Set(["runAgent", "runSchedule", "openRoom", "openPage", "refresh"]);
export const safeName = (s: unknown): s is string => typeof s === "string" && /^[A-Za-z0-9_-]{1,128}$/.test(s) && !RESERVED.has(s);
export function own(value: unknown, key: unknown): unknown {
  if (!safeName(key) || !value || typeof value !== "object" || Array.isArray(value)) return undefined;
  // Only JSON own value properties, never an inherited member or accessor.
  return Object.getOwnPropertyDescriptor(value, key)?.value;
}
export const str = (v: unknown) => typeof v === "string" ? v.slice(0, 65_536) : "";
const displayString = (v: unknown) => typeof v === "string" ? v : "";
export const number = (v: unknown) => typeof v === "number" && Number.isFinite(v) ? v : undefined;
const items = (v: unknown, max = MAX_ITEMS): unknown[] => Array.isArray(v) ? v.slice(0, max) : [];
type Local = { name: string; value: unknown };
type Read = { value?: unknown; issue?: string };
type Lookup = (name: string) => Read;

function evaluate(expr: unknown, get: Lookup, budget: { left: number }, depth = 0): Read {
  if (!expr || depth > 8 || --budget.left < 0) return {};
  const inner = (key: string) => evaluate(own(expr, key), get, budget, depth + 1);
  switch (own(expr, "k")) {
    case "lit": {
      const value = own(expr, "v");
      return value === null || ["string", "number", "boolean"].includes(typeof value) ? { value } : {};
    }
    case "bind": {
      const path = own(expr, "path");
      if (!Array.isArray(path) || !path.length || path.length > 32 || !path.every(safeName)) return {};
      const read = get(path[0]);
      if (read.issue) return read;
      return { value: path.slice(1).reduce((v, k) => own(v, k), read.value) };
    }
    case "not": {
      const a = inner("a");
      return a.issue ? a : { value: !a.value };
    }
    case "coalesce": {
      const a = inner("a");
      return a.issue || a.value != null ? a : inner("b");
    }
    case "cmp": {
      const a = inner("a");
      const b = own(expr, "b");
      const op = own(expr, "op");
      if (a.issue) return a;
      if (op === "eq" || op === "ne") return { value: op === "eq" ? a.value === b : a.value !== b };
      const x = a.value;
      const cmp = (typeof x === "number" && typeof b === "number") || (typeof x === "string" && typeof b === "string")
        ? (x < b ? -1 : x > b ? 1 : 0) : undefined;
      return { value: cmp !== undefined && (op === "lt" ? cmp < 0 : op === "gt" ? cmp > 0 : op === "le" ? cmp <= 0 : op === "ge" && cmp >= 0) };
    }
    default: return {};
  }
}

export type Cell = { label: string; tone?: "success" | "warning" | "danger" | "system" };
export function cell(value: unknown, format?: string): Cell {
  const raw = typeof value === "number" && Number.isFinite(value) || typeof value === "boolean" ? String(value) : displayString(value);
  if (!raw) return { label: "—" };
  const n = number(value);
  if (format === "number" && n !== undefined) return { label: n.toLocaleString() };
  if (format === "percent" && n !== undefined) return { label: `${(n * 100).toLocaleString(undefined, { maximumFractionDigits: 1 })}%` };
  if (format === "bytes" && n !== undefined && n >= 0) {
    const unit = Math.min(4, Math.floor(Math.log(Math.max(n, 1)) / Math.log(1024)));
    return { label: `${(n / 1024 ** unit).toLocaleString(undefined, { maximumFractionDigits: 1 })} ${["B", "KiB", "MiB", "GiB", "TiB"][unit]}` };
  }
  if ((format === "relative" || format === "datetime") && typeof value === "string" && value.length <= 128) {
    const ms = Date.parse(raw);
    if (Number.isFinite(ms)) {
      if (format === "datetime") return { label: new Date(ms).toLocaleString() };
      const diff = Math.round((Date.now() - ms) / 1000);
      const s = Math.abs(diff);
      if (s < 60) return { label: "Just now" };
      const [amount, unit] = s < 3600 ? [Math.floor(s / 60), "min"] : s < 86400 ? [Math.floor(s / 3600), "h"] : [Math.floor(s / 86400), "d"];
      return { label: diff < 0 ? `In ${amount} ${unit}` : `${amount} ${unit} ago` };
    }
  }
  if (format === "status") {
    const label = raw.replace(/_/g, " ");
    const tone = /^(failed|error|interrupted)$/.test(raw) ? "danger" : /^(starting|running|queued|pending)$/.test(raw) ? "system"
      : /^(done|completed|success|succeeded)$/.test(raw) ? "success" : /^(cancelled|unknown|skipped)$/.test(raw) ? "warning" : undefined;
    return { label: label.charAt(0).toUpperCase() + label.slice(1), tone };
  }
  return { label: raw };
}

export function capabilityOf(data: PageValues | null, key: string): Capability | undefined {
  const c = own(data?.actions, key);
  return typeof own(c, "enabled") === "boolean" ? c as Capability : undefined;
}

export type PlanBlock = {
  type: string; instance: string; key?: string; issue?: string; text: string; p: Record<string, unknown>;
  children: PlanBlock[]; headings?: string[]; rows?: Cell[][]; entries?: DisplayEntry[];
  points?: { label: string; value: number }[]; answer?: DisplayEntry & { error: string }; action?: ActionKind;
  stat?: Cell; disabledReason?: string;
  document?: TextViewDocument; messages?: (TextViewDocument | undefined)[];
};
type DisplayEntry = { title: string; status: Cell; metadata: string; fileId?: string };
const PROPS = ["title", "collapsed", "columns", "level", "tone", "muted", "label", "value", "format", "hint", "rows",
  "empty", "items", "as", "when", "max", "kind", "data", "x", "y", "from"];

export function planPage(spec: unknown, data: PageValues | null, pending: boolean, limit = MAX_RENDERED) {
  const supported = own(spec, "format") === 1;
  const rich = pageRichText();
  const document = (value: unknown) => typeof value === "string" ? rich.document([value]) : undefined;
  let textBytes = 0, textLimited = false;
  const prefix = (text: string) => {
    const { bytes, end } = utf8Prefix(text, MAX_PAGE_TEXT_BYTES - textBytes);
    if (end < text.length) textLimited = true;
    return { text: text.slice(0, end), bytes };
  };
  const plain = (text: string) => {
    const part = prefix(text);
    textBytes += part.bytes;
    return part.text;
  };
  const formatted = (value: unknown, format?: string): Cell => {
    // Bound work before formatting too (e.g. replacing underscores in
    // status). Never interpret a clipped date as an actual timestamp.
    if (typeof value === "string" && value && format !== "status" && format !== "relative" && format !== "datetime") {
      return { label: plain(value) };
    }
    const input = typeof value === "string" ? prefix(value).text : value;
    if (value && input === "") return { label: "" };
    const c = cell(input, input !== value && (format === "relative" || format === "datetime") ? undefined : format);
    return { ...c, label: plain(c.label) };
  };
  const relative = (value: unknown) => cell(value, "relative").label;
  const byteLabel = (value: unknown) => typeof value === "string" ? value : cell(value, "bytes").label;
  const metadata = (labels: string[]) => {
    let result = "";
    for (const label of labels) {
      if (!label || label === "—") continue;
      const part = plain(label);
      if (!part) break;
      // Separators are fixed UI text, not content from the page.
      result += result ? ` · ${part}` : part;
    }
    return result;
  };
  let left = Math.min(MAX_RENDERED, Math.max(0, limit)), truncated = false;
  const initial = left;
  const spend = (count = 1) => {
    if (count > left) { truncated = true; return false; }
    left -= count;
    return true;
  };
  const sources = new Set(items(own(spec, "data"), 16).map(d => own(d, "name")).filter(safeName));
  function walk(blocks: unknown, parent: string, depth: number, local?: Local): PlanBlock[] {
    const out: PlanBlock[] = [];
    if (!Array.isArray(blocks) || !blocks.length) return out;
    if (depth > MAX_DEPTH) { truncated = true; return out; }
    for (let i = 0; i < blocks.length; i++) {
      if (!spend()) break; // also bounds scanning of unknown nodes
      const b: unknown = blocks[i];
      const type = str(own(b, "type"));
      if (!BLOCKS.has(type) || (local && ["Table", "List", "Each"].includes(type))) continue;
      const k = own(b, "key");
      const key = safeName(k) ? k : undefined;
      const instance = `${parent}/${i}:${type}:${key ?? ""}`;
      const get = (name: string): Read => {
        if (local?.name === name) return { value: local.value };
        if (!sources.has(name)) return {};
        const src = own(data?.data, name) as SourceResult | undefined;
        if (own(src, "ok") === true) return { value: own(src, "value") };
        if (own(src, "ok") === false) return { issue: own(src, "error") === "no_access" ? "You don’t have access to this source." : displayString(own(src, "message")) || "This source isn’t available." };
        return { issue: pending ? "Loading data…" : "This source isn’t available." };
      };
      const p: Record<string, unknown> = Object.create(null);
      const node: PlanBlock = { type, instance, key, text: "", p, children: [] };
      out.push(node);
      const read = (expr: unknown) => {
        const r = evaluate(expr, get, { left: 32 });
        node.issue ??= r.issue;
        return r.value;
      };
      const props = own(b, "props");
      for (const prop of PROPS) p[prop] = read(own(props, prop));
      const parts = items(own(b, "content")).map(v => {
        const scalar = read(v);
        return typeof scalar === "string" ? scalar : scalar == null ? "" : cell(scalar).label;
      });
      if (node.issue) { node.issue = plain(node.issue); continue; }
      // Project only display strings. Keep format selectors and action/file
      // identifiers intact; clipping an identifier could change its meaning.
      if (type === "Section") p.title = plain(displayString(p.title));
      if (["Stat", "Progress", "Chart", "Button", "Link"].includes(type)) p.label = plain(displayString(p.label));
      if (["Empty", "Progress", "Chart", "Table", "List", "AgentAnswer", "RunList", "MessageFeed", "ScheduleCard", "FileList"].includes(type)) p.empty = plain(displayString(p.empty));
      if (type === "Markdown") node.document = rich.document(parts);
      else if (["Heading", "Text", "Empty", "Badge"].includes(type)) for (const part of parts) node.text += plain(part);
      const children = own(b, "children");
      switch (type) {
        case "Section": case "Row": case "Grid":
          node.children = walk(children, instance, depth + 1, local); break;
        case "Show":
          if (p.when) node.children = walk(children, instance, depth + 1, local);
          break;
        case "Each": case "List":
          if (safeName(p.as)) for (const [index, value] of items(p.items).entries()) {
            if (!left) { truncated = true; break; }
            node.children.push(...walk(children, `${instance}#${index}`, depth + 1, { name: p.as, value }));
          }
          break;
        case "Table": {
          if (depth === MAX_DEPTH) { truncated = true; break; }
          const columns = items(children, MAX_COLUMNS).filter(c => own(c, "type") === "Column").flatMap(c => {
            if (!spend()) return [];
            const props = own(c, "props");
            const field = read(own(props, "field"));
            return safeName(field) ? [{ field, label: displayString(read(own(props, "label"))) || field, format: str(read(own(props, "format"))) }] : [];
          });
          node.headings = columns.map(c => plain(c.label));
          node.rows = [];
          if (columns.length) for (const row of items(p.rows)) {
            if (!spend(columns.length)) break;
            node.rows.push(columns.map(c => formatted(own(row, c.field), c.format)));
          }
          break;
        }
        case "Chart":
          node.points = items(p.data).flatMap(r => {
            const y = number(own(r, p.y));
            return y !== undefined && spend() ? [{ label: formatted(own(r, p.x)).label, value: y }] : [];
          });
          break;
        case "AgentAnswer":
          if (p.from) node.answer = {
            title: "", status: formatted(own(p.from, "status"), "status"),
            metadata: metadata([displayString(own(p.from, "agent")), displayString(own(p.from, "room")), relative(own(p.from, "at"))]),
            error: plain(displayString(own(p.from, "error"))),
          };
          node.document = document(own(p.from, "text"));
          break;
        case "RunList": case "MessageFeed": case "ScheduleCard": case "FileList": {
          const rows = items(own(p.from, "items")).filter(() => spend());
          node.entries = rows.map(row => {
            const field = (name: string) => own(row, name);
            return {
              title: plain(displayString(field(type === "RunList" ? "task" : type === "MessageFeed" ? "author" : "name")) || displayString(field("agent"))),
              status: formatted(field(type === "ScheduleCard" ? "last_status" : "status"), "status"),
              metadata: type === "MessageFeed" ? "" : metadata([
                displayString(field("room")), displayString(field(type === "RunList" ? "agent" : "kind")),
                relative(field(type === "FileList" ? "uploaded" : type === "ScheduleCard" ? "next_run" : "started")),
                type === "FileList" ? byteLabel(field("size")) : type === "ScheduleCard" && field("enabled") === false ? "Paused" : "",
              ]),
              fileId: type === "FileList" ? str(field("id")) : undefined,
            };
          });
          if (type === "MessageFeed") node.messages = rows.map(row => document(own(row, "text")));
          break;
        }
        case "Stat":
          node.stat = formatted(p.value, str(p.format));
          p.hint = plain(displayString(p.hint));
          break;
        case "Button": case "Link": {
          const kind = own(own(b, "action"), "kind");
          if (typeof kind === "string" && ACTIONS.has(kind) && (type !== "Link" || kind === "openRoom" || kind === "openPage")) node.action = kind as ActionKind;
          if (key) node.disabledReason = plain(displayString(own(capabilityOf(data, key), "reason")));
          break;
        }
      }
    }
    return out;
  }
  // The Page root is depth 1 in the compiler; its first block is depth 2.
  const blocks = supported ? walk(own(spec, "blocks"), "", 2) : [];
  return { blocks, supported, truncated: truncated || textLimited, count: initial - left, plainText: { bytes: textBytes, limited: textLimited }, richText: rich.usage() };
}
