import type { Expr, PageBlock, PageData, PageSpec } from "../../../api/pages";

export const stamp = "2026-11-25T00:00:00.000Z";
export const lit = (v: unknown): Expr => ({ k: "lit", v });
export const bind = (...path: string[]): Expr => ({ k: "bind", path });
const content = (type: string, key: string, value: Expr, props?: PageBlock["props"]): PageBlock => ({ type, key, content: [value], props });
export const sample: PageSpec = {
  format: 1, title: "Pages fixture", refresh: "30s",
  data: ["runs", "answer", "messages", "schedules", "files", "private"].map(name => ({ name, source: name })),
  blocks: [
    { type: "Section", key: "overview", props: { title: lit("Overview") }, children: [
      content("Heading", "heading", lit("Release dashboard"), { level: lit(2) }),
      { type: "Row", key: "stats", children: [
        { type: "Stat", key: "stat", props: { label: lit("Active runs"), value: bind("runs", "active"), format: lit("number") } },
        content("Badge", "badge", lit("Passing"), { tone: lit("success") }),
      ] },
      { type: "Grid", key: "grid", props: { columns: lit(2) }, children: [
        { type: "Progress", key: "progress", props: { label: lit("Coverage"), value: lit(7), max: lit(10) } },
        content("Text", "intro", lit("Compiled natively")),
      ] },
      { type: "Divider", key: "divider" },
      { type: "Table", key: "runs", props: { rows: bind("runs", "items") }, children: [
        { type: "Column", props: { field: lit("agent"), label: lit("Agent") } },
        { type: "Column", props: { field: lit("status"), label: lit("Status"), format: lit("status") } },
      ] },
      { type: "List", key: "list", props: { items: bind("runs", "items"), as: lit("row") }, children: [content("Text", "list-text", bind("row", "task"))] },
      { type: "Each", key: "each", props: { items: bind("runs", "items"), as: lit("run") }, children: [
        { type: "Show", key: "show", props: { when: { k: "cmp", op: "eq", a: bind("run", "status"), b: "running" } }, children: [
          content("Text", "running", lit("Work in progress")),
        ] },
      ] },
      content("Empty", "empty", lit("Nothing else to show")),
    ] },
    { type: "Chart", key: "chart", props: { kind: lit("bar"), label: lit("Durations"), data: bind("runs", "items"), x: lit("agent"), y: lit("duration") } },
    { type: "AgentAnswer", key: "answer", props: { from: bind("answer") } },
    { type: "MessageFeed", key: "messages", props: { from: bind("messages") } },
    { type: "RunList", key: "run-list", props: { from: bind("runs") } },
    { type: "ScheduleCard", key: "schedules", props: { from: bind("schedules") } },
    { type: "FileList", key: "files", props: { from: bind("files") } },
    content("Markdown", "markdown", lit("**Notes** and [release](https://example.invalid/release).")),
    { type: "Stat", key: "restricted", props: { label: lit("Restricted"), value: bind("private", "secret") } },
    { type: "Button", key: "ask", props: { label: lit("Ask for a brief") }, action: { kind: "runAgent" } },
    { type: "Button", key: "run", props: { label: lit("Run schedule") }, action: { kind: "runSchedule" } },
    { type: "Link", key: "room", props: { label: lit("Open missing room") }, action: { kind: "openRoom" } },
    { type: "Link", key: "other", props: { label: lit("Open another page") }, action: { kind: "openPage" } },
    { type: "Button", key: "refresh", props: { label: lit("Refresh this page") }, action: { kind: "refresh" } },
  ],
};

export const sampleData: PageData = {
  version: 1, resolved_at: stamp,
  data: {
    runs: { ok: true, value: { active: 2, items: [{ agent: "Release bot", task: "Build release", duration: 5, status: "running" }] } },
    answer: { ok: true, value: { text: "Everything looks good.\n\n![Diagram alt](https://example.invalid/remote.png)\n\n[Not a live link](http://example.invalid/)", agent: "Release bot", status: "done" } },
    messages: { ok: true, value: { items: [{ author: "Alice", text: "Ready to ship." }] } },
    schedules: { ok: true, value: { items: [{ name: "Release reminder", enabled: false }] } },
    files: { ok: true, value: { items: [{ name: "changelog.md", kind: "document", size: 1024, uploaded: stamp }] } },
    private: { ok: false, error: "no_access", message: "Not a member" },
  },
  actions: {
    ask: { enabled: true, confirm: true }, run: { enabled: true, confirm: true, schedule_revision: 3 },
    room: { enabled: true, space_id: "acme", room_id: "missing" }, other: { enabled: true, page_id: "pg_personal" },
    refresh: { enabled: true },
  },
};

const repeatedLabel = "🛰".repeat(2048);
const repeatedCell = "日".repeat(4096);
export const plainBudgetSpec: PageSpec = { format: 1, title: "Plain text fixture", refresh: "off", data: [{ name: "rows", source: "runs" }], blocks: [
  { type: "Each", key: "rows", props: { items: bind("rows"), as: lit("row") }, children: [
    { type: "Stat", key: "repeated", props: { label: lit(repeatedLabel), value: lit(1) } },
  ] },
  { type: "Table", key: "cells", props: { rows: bind("rows") }, children: [
    { type: "Column", props: { field: lit("cell"), label: lit("Cells") } },
  ] },
  content("Text", "after-budget", lit("Must not mount after the plain text quota")),
] };
export const plainBudgetData: PageData = { ...sampleData, data: {
  rows: { ok: true, value: Array.from({ length: 10 }, () => ({ cell: repeatedCell })) },
} };
