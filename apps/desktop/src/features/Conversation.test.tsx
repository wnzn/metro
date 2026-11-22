// The conversation's controls, headless against the in-memory fake server:
// the message toolbar (hover, keyboard), the reaction popover's keys, copy,
// the composer's Send / Ask agent and focus-only hint, mention suggestions
// (Return picks without sending, closing on room change), the empty room's
// suggestions (they fill the draft, never send) and the recipient rule.

import { expect, open, test } from "nsh/test";
import type { App } from "nsh/test";
import { BASE, TOKENS, world } from "../../test/fakeServer";
import { headerAudience } from "./transcriptRows";

type F = ReturnType<typeof world>;

async function inAcme(f: F) {
  const app = await open("../../App.tsx", { network: f.network as any, platform: "linux", width: 1200, height: 800 });
  await app.clear("id:server-url");
  await app.type("id:server-url", BASE);
  await app.type("id:access-token", TOKENS.alice!);
  await app.press("id:connect");
  await app.waitFor("id:space:acme");
  await app.press("id:space:acme");
  await app.waitFor("text:hello from bob in acme");
  return app;
}

const messages = (f: F) => f.posts().filter((p) => p.path.endsWith("/messages"));
const opacity = (app: App, selector: string) => app.viewStyle(selector)?.effectiveOpacity ?? -1;

test("the message toolbar is quiet until hover, then reacts and copies", async () => {
  const f = world();
  const app = await inAcme(f);
  const ev = f.state.events.get("acme/lobby")![0]!;
  // Present for the keyboard and screen readers, drawn only on hover or focus.
  expect(app.shows(`id:react:${ev.id}`)).toBe(true);
  expect(opacity(app, `id:react:${ev.id}`)).toBe(0);
  await app.hover(`id:event:${ev.id}`);
  expect(opacity(app, `id:react:${ev.id}`)).toBe(1);

  await app.press(`id:copy:${ev.id}`);
  expect(app.clipboard()).toBe("hello from bob in acme");
  expect(app.shows("label:Copied")).toBe(true);
});

test("the reaction popover: focus on the first emoji, arrows move, Return picks, Escape closes", async () => {
  const f = world();
  const app = await inAcme(f);
  const ev = f.state.events.get("acme/lobby")![0]!;

  await app.press(`id:react:${ev.id}`);
  await app.waitFor(`id:palette:${ev.id}`);
  await app.keyboard.press("escape");
  await app.waitFor(`id:palette:${ev.id}`, { state: "gone" });
  expect(f.posts().some((p) => p.path.includes("/reactions"))).toBe(false);

  await app.press(`id:react:${ev.id}`);
  await app.waitFor(`id:palette:${ev.id}`);
  await app.keyboard.press("right"); // 👍 → 👀
  await app.keyboard.press("enter");
  await app.waitFor(`id:reaction:${ev.id}:👀`);
  expect(f.posts().at(-1)).toMatchObject({ method: "PUT", body: { emoji: "👀" } });
  expect(app.shows(`id:palette:${ev.id}`)).toBe(false);
});

test("Send posts a plain message; Ask agent explicitly asks; the hint shows only while typing", async () => {
  const f = world();
  const app = await inAcme(f);
  expect(app.shows("id:composer-hint")).toBe(false);
  await app.type("id:composer", "just people");
  expect(app.shows("id:composer-hint")).toBe(true);
  await app.press("id:send");
  await app.waitFor("text:just people");
  expect(messages(f).at(-1)!.body).toMatchObject({ text: "just people", invoke_agent: false });

  await app.type("id:composer", "what's in the repo?");
  await app.press("id:ask-agent");
  await app.waitFor("text:what's in the repo?");
  expect(messages(f).at(-1)!.body).toMatchObject({ text: "what's in the repo?", invoke_agent: true });
});

test("@ suggestions: Return picks the highlighted one and sends nothing", async () => {
  const f = world();
  const app = await inAcme(f);
  await app.type("id:composer", "ping @b");
  await app.waitFor("id:mention-picker");
  expect(app.shows("id:pick:bob")).toBe(true);
  expect(app.shows("id:pick:agent")).toBe(false); // "b" doesn't match the agent
  expect(app.shows("id:pick:alice")).toBe(false); // never yourself
  expect(app.shows("id:mention-picker >> text:@bob")).toBe(true); // the stable id beside the name
  const before = messages(f).length;
  await app.submit("id:composer");
  await app.waitFor("id:mention-chip:bob");
  expect(app.value("id:composer")).toBe("ping @bob ");
  expect(messages(f).length).toBe(before); // Return picked; nothing was sent
  expect(app.shows("id:mention-picker")).toBe(false);

  // Escape closes it; typing on opens it again.
  await app.type("id:composer", "@");
  await app.waitFor("id:mention-picker");
  await app.keyboard.press("escape");
  await app.waitFor("id:mention-picker", { state: "gone" });
  await app.type("id:composer", "a");
  await app.waitFor("id:pick:agent");

  // Return with the list open picks; the next Return sends with both recipients.
  await app.submit("id:composer");
  await app.waitFor("id:mention-chip:agent");
  await app.submit("id:composer");
  await app.waitUntil(() => messages(f).length === before + 1);
  expect(messages(f).at(-1)!.body).toEqual({ text: "ping @bob @agent", invoke_agent: false, mentions: [{ kind: "user", id: "bob" }, { kind: "agent" }] });
});

test("the @ button opens the list under itself; leaving the room closes it", async () => {
  const f = world();
  f.addRoom("acme", { id: "ops", name: "ops" });
  const app = await inAcme(f);
  await app.press("id:mention");
  await app.waitFor("id:mention-picker");
  await app.press("id:room:ops");
  await app.waitFor("id:mention-picker", { state: "gone" });

  // A half-typed mention doesn't follow the user into another room.
  await app.type("id:composer", "@");
  await app.waitFor("id:mention-picker");
  await app.press("id:room:lobby");
  await app.waitFor("id:mention-picker", { state: "gone" });
});

test("an empty room offers suggestions that fill the draft and never send", async () => {
  const f = world();
  f.addRoom("acme", { id: "quiet", name: "quiet" });
  const app = await inAcme(f);
  await app.press("id:room:quiet");
  await app.waitFor("id:transcript-empty");
  const before = f.posts().length;
  await app.press("id:suggestion:ask-agent");
  await app.waitFor("id:mention-chip:agent");
  expect(app.value("id:composer")).toBe("@agent what can you help with in this room?");
  await app.press("id:suggestion:hello");
  expect(app.value("id:composer")).toBe("Hello everyone! ");
  expect(app.shows("id:mention-chips")).toBe(false); // its @agent went with the text
  expect(f.posts().length).toBe(before);
});

test("the agent has a glyph, not initials; a message to me has the rail and explicit recipients", async () => {
  const f = world();
  const reply = f.post("acme/lobby", { kind: "agent", actor: "agent", text: "on it" });
  const toMe = f.post("acme/lobby", { kind: "message", actor: "bob", text: "@alice look", mentions: [{ kind: "user", id: "alice" }], audience: "users" });
  const app = await inAcme(f);
  await app.waitFor("text:on it");
  expect(app.shows(`id:avatar:${reply.id}`)).toBe(true);
  expect(app.shows(`id:avatar:${reply.id} >> text:/^[A-Z]{1,2}$/`)).toBe(false);
  expect(app.shows(`id:recipient:${toMe.id}:alice >> text:@you`)).toBe(true);
  expect(app.shows("label:Addressed to you")).toBe(true);
});

test("an audience phrase is left out only where a typed @agent already says it", () => {
  const typed = [{ kind: "agent" as const }];
  expect(headerAudience("agent", typed)).toBe(undefined);
  expect(headerAudience("agent", undefined)).toBe("agent"); // Ask agent, nothing typed
  expect(headerAudience("agent", [{ kind: "agent", addressedBy: "classifier" }])).toBe("agent");
  expect(headerAudience("mixed", [...typed, { kind: "user", id: "bob" }])).toBe("mixed");
  expect(headerAudience("users", [{ kind: "user", id: "bob" }])).toBe("users");
  expect(headerAudience("room", undefined)).toBe("room");
});
