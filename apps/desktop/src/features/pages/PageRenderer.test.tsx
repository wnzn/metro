import { expect, open, test } from "nsh/test";
import type { PageBlock } from "../../api/pages";
import { httpsLink, MAX_PAGE_MARKDOWN_BYTES, MAX_PAGE_MARKDOWN_NODES, pageRichText } from "../../ui/pageMarkdown";
import { MAX_PAGE_TEXT_BYTES, MAX_RENDERED, own, planPage } from "./renderPlan";
import { bind, lit, plainBudgetData, plainBudgetSpec, sample, sampleData } from "./test/sample";

test("PageSpec fixture draws native blocks, tables, repeated conditions and denied sources", async () => {
  const app = await open("./test/pages.fixture.tsx", { width: 1100, height: 850, platform: "linux" });
  await app.waitFor("text:Release dashboard");
  const text = () => app.texts().join(" | ");
  for (const phrase of ["Active runs", "Coverage", "Release bot", "Build release", "Work in progress", "Nothing else to show",
    "Durations", "Everything looks good.", "Ready to ship.", "Release reminder", "changelog.md", "1 KiB", "You don’t have access to this source."]) {
    expect(text()).toContain(phrase);
  }
  expect(app.shows("id:page-table")).toBe(true);
  expect(text()).toContain("Diagram alt");
  expect(text()).toContain("Not a live link");
  await app.press("id:fixture-draft");
  await app.waitFor("text:Show live");
  expect(text()).toContain("Preview only");
});

test("only own scalar data paths and supported blocks are interpreted", () => {
  let accessed = false;
  const hostile = Object.assign(Object.create({ hidden: "inherited" }), { ok: "visible" });
  Object.defineProperty(hostile, "getter", { get() { accessed = true; return "secret"; } });
  expect(own(hostile, "hidden")).toBe(undefined);
  expect(own(hostile, "constructor")).toBe(undefined);
  expect(own(hostile, "getter")).toBe(undefined);
  expect(accessed).toBe(false);
  const spec = { format: 1, data: [{ name: "record" }], blocks: [
    { type: "WebView", key: "html", props: { html: lit("<script>doSomething()</script>") } },
    { type: "Text", key: "safe", content: [bind("record", "ok")] },
    { type: "Text", key: "inherited", content: [bind("record", "hidden")] },
    { type: "Text", key: "proto", content: [bind("record", "__proto__", "hidden")] },
    { type: "Button", key: "reserved", action: { kind: "runAnywhere" } },
  ] };
  const plan = planPage(spec, { ...sampleData, data: { record: { ok: true, value: hostile } } }, false);
  expect(plan.blocks.map(b => b.text)).toEqual(["visible", "", "", ""]);
  expect(plan.blocks[3]?.action).toBe(undefined);
  expect(planPage({ ...spec, format: 2 }, null, false).blocks).toHaveLength(0);
});

test("PageSpec render budget counts expanded rows and cells and stops deep trees", () => {
  const spec = { format: 1, data: [{ name: "records" }], blocks: [
    { type: "Table", key: "table", props: { rows: bind("records") }, children: [
      { type: "Column", props: { field: lit("n") } }, { type: "Column", props: { field: lit("n") } },
    ] },
    { type: "List", key: "repeat", props: { items: bind("records"), as: lit("item") }, children: [
      { type: "Text", key: "value", content: [bind("item", "n")] },
      { type: "Each", key: "nested", props: { items: bind("records"), as: lit("again") }, children: [{ type: "Text", key: "prohibited" }] },
    ] },
  ] };
  const values = Array.from({ length: 80 }, (_, n) => ({ n }));
  const data = { ...sampleData, data: { records: { ok: true as const, value: values } } };
  const limited = planPage(spec, data, false, 8);
  expect(limited.count).toBe(8);
  expect(limited.truncated).toBe(true);
  expect(limited.blocks[0]?.rows?.length).toBe(2);
  const normal = planPage(spec, data, false);
  expect(normal.blocks[0]?.rows?.length).toBe(50);
  expect(normal.blocks[1]?.children.map(b => b.type)).toEqual(Array.from({ length: 50 }, () => "Text"));
  const huge = planPage({ format: 1, blocks: Array.from({ length: MAX_RENDERED + 100 }, () => ({ type: "Text" })) }, null, false);
  expect(huge.blocks).toHaveLength(MAX_RENDERED);
  expect(huge.truncated).toBe(true);
  let deep: PageBlock = { type: "Text", key: "hidden" };
  for (let n = 0; n < 30; n++) deep = { type: "Section", children: [deep] };
  const bounded = planPage({ format: 1, blocks: [deep] }, null, false);
  expect(bounded.count).toBe(11);
  expect(bounded.truncated).toBe(true);
});

test("Page Markdown drops image nodes and insecure links in nested content", () => {
  const document = pageRichText().document([[
    "> [![Alt only](https://example.invalid/pixel)](https://example.invalid/good)",
    "",
    "- [Insecure](http://example.invalid/)",
    "- [Bad](javascript:alert)",
    "- [Mail](mailto:a@example.invalid)",
    "",
    "<img src=\"https://example.invalid/not-rendered\" />",
  ].join("\n")]);
  const encoded = JSON.stringify(document);
  expect(encoded).toContain("Alt only");
  expect(encoded).toContain("https://example.invalid/good");
  expect(encoded).toContain("Insecure");
  expect(encoded).not.toContain("\"type\":\"image\"");
  expect(encoded).not.toContain("\"url\":\"http:");
  expect(encoded).not.toContain("\"url\":\"javascript:");
  expect(encoded).not.toContain("\"url\":\"mailto:");
  for (const url of ["//example.invalid", "file:///secret", "javascript:alert", "mailto:a@b", "https://a@b", "https:\\\\a", "https:///a"]) {
    expect(httpsLink(url)).toBe(null);
  }
  expect(httpsLink("https://example.invalid/a")).toBe("https://example.invalid/a");
});

test("One UTF-8 quota covers literal Markdown, answers and repeated message documents", () => {
  const spec = { format: 1, data: [{ name: "answer" }, { name: "feed" }, { name: "rows" }], blocks: [
    { type: "Markdown", key: "md", content: [lit("日本語".repeat(2_000))] },
    { type: "AgentAnswer", key: "answer", props: { from: bind("answer") } },
    { type: "Each", key: "items", props: { items: bind("rows"), as: lit("one") }, children: [
      { type: "MessageFeed", key: "feed", props: { from: bind("feed") } },
    ] },
    { type: "Markdown", key: "later", content: [lit("Must not mount after the quota")] },
  ] };
  const result = planPage(spec, { ...sampleData, data: {
    answer: { ok: true, value: { text: "🛰".repeat(5_000) } },
    feed: { ok: true, value: { items: [{ text: "é".repeat(4_000) }, { text: "🛠".repeat(2_000) }] } },
    rows: { ok: true, value: Array.from({ length: 30 }, (_, i) => i) },
  } }, false);
  expect(result.richText.limited).toBe(true);
  expect(result.richText.bytes).toBe(MAX_PAGE_MARKDOWN_BYTES);
  expect(result.blocks[2]?.children.filter(b => b.messages?.some(Boolean)).length).toBeLessThan(30);
  expect(result.blocks[3]?.document).toBe(undefined);
  const scalarBound = pageRichText(5);
  const clipped = scalarBound.document(["é🛰"]);
  expect(scalarBound.usage()).toEqual({ bytes: 2, nodes: 3, limited: true });
  expect(JSON.stringify(clipped)).not.toContain("🛰");
});

test("The emitted Markdown budget charges lists and tables across small documents", () => {
  const spec = { format: 1, data: [{ name: "feed" }], blocks: Array.from({ length: 100 }, (_, i) => ({
    type: "MessageFeed", key: `feed-${i}`, props: { from: bind("feed") },
  })) };
  const text = "A | B\n---|---\nC | D\n\n- **one**\n- *two*";
  const data = { ...sampleData, data: { feed: { ok: true as const, value: { items: Array.from({ length: 8 }, () => ({ text })) } } } };
  const result = planPage(spec, data, false);
  expect(result.richText.nodes).toBe(MAX_PAGE_MARKDOWN_NODES);
  expect(result.richText.bytes).toBeLessThan(MAX_PAGE_MARKDOWN_BYTES);
  expect(result.richText.limited).toBe(true);
  expect(result.blocks[0]?.messages?.every(Boolean)).toBe(true);
  expect(result.blocks[99]?.messages?.some(Boolean)).toBe(false);
});

test("Repeated plain labels and table cells share a UTF-8 limit before mounting", async () => {
  const plan = planPage(plainBudgetSpec, plainBudgetData, false);
  expect(plan.plainText).toEqual({ limited: true, bytes: MAX_PAGE_TEXT_BYTES });
  expect(plan.richText.limited).toBe(false);
  const [repeat, table, after] = plan.blocks;
  expect(repeat?.children).toHaveLength(10);
  const cells = table?.rows?.map(row => row[0]?.label ?? "") ?? [];
  expect(cells[0]?.length).toBe(4096);
  expect(cells[3]?.length).toBeLessThan(4096);
  expect(cells[4]).toBe("");
  expect(after?.text).toBe("");

  const app = await open("./test/pages.fixture.tsx", { width: 1100, height: 850, platform: "linux" });
  await app.press("id:fixture-budget");
  await app.waitFor("id:page-render-limit");
  expect(app.texts().join(" ")).not.toContain("Must not mount after the plain text quota");
});
