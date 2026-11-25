import { TextView } from "nsh/base";
import { expect, open, test } from "nsh/test";
import { BASE, TOKENS, world } from "../../test/fakeServer";
import { launched } from "../../test/launched";
import { MAX_MESSAGE_MARKDOWN_BYTES, messageMarkdown } from "./messageMarkdown";

const span = (source: string, literal: string, from = 0) => {
  const start = source.indexOf(literal, from);
  return { start, end: start + literal.length, literal };
};

test("message Markdown keeps formatting and caps heading and table dimensions", () => {
  const row = Array(14).fill("one").join(" | ");
  const divider = Array(14).fill("---").join(" | ");
  const source = ["###### Headline", "", "> **Bold** and *italic*", "", "- item with `code`", "", row, divider,
    ...Array(52).fill(row)].join("\n");
  const view = messageMarkdown(source);
  const blocks = view.document!.blocks;
  expect(blocks.map(b => b.type)).toEqual(["heading", "blockquote", "list", "table"]);
  expect(blocks[0]).toMatchObject({ level: 3 });
  expect(JSON.stringify(blocks[1])).toContain('"type":"strong"');
  expect(JSON.stringify(blocks[1])).toContain('"type":"emphasis"');
  expect(JSON.stringify(blocks[2])).toContain('"type":"code"');
  expect(blocks[3]?.type === "table" && blocks[3].head.length).toBe(12);
  expect(blocks[3]?.type === "table" && blocks[3].rows?.length).toBe(30);
  expect(view.limited).toBe(true);
  const tall = messageMarkdown(["one | two", "--- | ---", ...Array(52).fill("one | two")].join("\n"));
  expect(tall.document!.blocks[0]).toMatchObject({ type: "table" });
  const tallTable = tall.document!.blocks[0];
  expect(tallTable?.type === "table" && tallTable.rows?.length).toBe(50);
});

test("only selected spans become inline chips; typed and entity sentinels cannot forge a chip", () => {
  const source = "**@bob** then `@coder` and [*@ada*](https://example.invalid)\n\n```\n@zed\n```\n\n"
    + "Unbound @bob \uE0000\uE001 &#xE000;0&#57345; &\uE000#xE000;0&#57345;";
  const { document } = messageMarkdown(source, ["@bob", "@coder", "@ada", "@zed"].map(s => span(source, s)));
  const encoded = JSON.stringify(document);
  expect(encoded.match(/"name":"mention"/g)?.length).toBe(2);
  expect(encoded).toContain('"type":"strong","content":[{"type":"custom","name":"mention","attributes":{"ordinal":"0"}');
  expect(encoded).toContain('"ordinal":"2"');
  expect(encoded).toContain('"code":"@coder"');
  expect(encoded).toContain('"code":"@zed"');
  expect(encoded).toContain("Unbound @bob");
  expect(encoded).not.toContain('"type":"link"');
  expect(encoded).not.toContain("\uE000");
  expect(encoded).not.toContain("\uE001");
  const cut = "**@bo";
  expect(JSON.stringify(messageMarkdown(cut, [span("**@bob**", "@bob")]).document)).not.toContain("@bo");
});

test("HTML is inert, image URLs are dropped, and only safe HTTPS links survive", () => {
  const source = [
    "<img src='https://example.invalid/raw'> <script>not code</script>",
    "",
    "[Secure](https://example.invalid/okay) [No](http://example.invalid/clear) [No](javascript:alert)",
    " [Userinfo](https://alice@example.invalid/) ![Alt](https://example.invalid/pixel)",
    " [Hidden](https://example.invalid/@bob)",
  ].join("\n");
  const encoded = JSON.stringify(messageMarkdown(source, [span(source, "@bob")]).document);
  expect(encoded.match(/"type":"link"/g)?.length).toBe(1);
  expect(encoded).toContain('"url":"https://example.invalid/okay"');
  expect(encoded).not.toContain('"type":"image"');
  expect(encoded).not.toContain("/pixel");
  expect(encoded).not.toContain('"type":"html"');
  expect(encoded).toContain("Alt");
});

test("the UTF-8 and total-node budgets stop large, deep and dense documents", () => {
  const huge = messageMarkdown("🛰".repeat(MAX_MESSAGE_MARKDOWN_BYTES / 4 + 2));
  expect(huge.limited).toBe(true);
  expect(TextView.plainText(huge.document!)).toBe("🛰".repeat(MAX_MESSAGE_MARKDOWN_BYTES / 4));

  const dense = messageMarkdown("**x**\n\n".repeat(900));
  expect(dense.limited).toBe(true);
  expect(dense.document!.blocks.length).toBe(267);
  const deep = messageMarkdown(`${"> ".repeat(30)}secret`);
  expect(deep.limited).toBe(true);
  expect(JSON.stringify(deep.document)).not.toContain("secret");

  const line = "hey @a there";
  const almostFull = line + "\n\n" + "**x**\n\n".repeat(266);
  const split = messageMarkdown(almostFull, [span(line, "@a")]);
  expect(split.limited).toBe(true);
  expect(JSON.stringify(split.document)).not.toContain('"name":"mention"');
  expect(TextView.plainText(split.document!).startsWith(line)).toBe(true);
});

test("narrow transcript renders people and agents, confirms links and copies folded source", async () => {
  const f = world();
  const raw = "**First line**\n\n```txt\n" + "very-long-code-".repeat(32) + "\n```\n\n"
    + "seven\n\neight\n\n[Read more](https://example.invalid/read)\n\n**Last line**";
  const person = f.post("acme/lobby", { kind: "message", text: raw, actor: "bob" });
  const agent = f.post("acme/lobby", { kind: "agent", text: "_Agent prose_ and `code`", actor: "agent" });
  const app = await open("../../App.tsx", { modules: launched(), network: f.network as any, platform: "linux", width: 720, height: 850 });
  await app.clear("id:server-url");
  await app.type("id:server-url", BASE);
  await app.type("id:access-token", TOKENS.alice!);
  await app.press("id:connect");
  await app.waitFor("id:space:acme");
  await app.press("id:space:acme");
  const body = `id:message-body:${person.id}`, row = `id:event:${person.id}`;
  await app.waitFor(`${body} >> text:First line`);
  expect(app.shows(`${body} >> text:Last line`)).toBe(false);
  expect(app.shows(`id:message-body:${agent.id} >> text:/Agent prose/`)).toBe(true);
  await app.press(`id:copy:${person.id}`);
  expect(app.clipboard()).toBe(raw);
  await app.press(`${row} >> label:/^Show all/`);
  await app.waitFor(`${body} >> text:Last line`);
  await app.press(`${body} >> text:Read more`);
  await app.waitFor("text:Open in your browser?");
  expect(app.openedUrls()).toEqual([]);
  expect(app.shows("text:https://example.invalid/read")).toBe(true);
  await app.press("label:Cancel");
  expect(app.openedUrls()).toEqual([]);
  await app.press(`${body} >> text:Read more`);
  await app.press("label:Open link");
  expect(app.openedUrls()).toEqual(["https://example.invalid/read"]);
  await app.press(`${row} >> label:Show less`);
  expect(app.shows(`${body} >> text:Last line`)).toBe(false);
  await app.type("id:composer", "**Still plain input**");
  expect(app.shows("id:composer-markdown-hint")).toBe(true);
  expect(app.value("id:composer")).toBe("**Still plain input**");
});
