// The conversation's controls, headless against the in-memory fake server:
// the message toolbar (hover, keyboard), the reaction popover's keys, copy,
// the composer's Send / Ask agent and focus-only hint, mention suggestions
// (Return picks without sending, closing on room change), the empty room's
// suggestions (they fill the draft, never send) and the recipient rule.

import { expect, open, test } from "nsh/test";
import type { App } from "nsh/test";
import { BASE, TOKENS, world } from "../../test/fakeServer";
import { headerAudience } from "./transcriptRows";
import theme from "../../theme/theme.json";
import { APPEARANCE_KEY } from "../ui";

type F = ReturnType<typeof world>;

async function inAcme(f: F, storage: Record<string, string> = {}) {
  const app = await open("../../App.tsx", { network: f.network as any, platform: "linux", width: 1200, height: 800, storage });
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

test("@ then Down then Return picks the second suggestion with the caret still in the field; nothing is sent", async () => {
  const f = world();
  const app = await inAcme(f);
  await app.type("id:composer", "ping @");
  await app.waitFor("id:pick:bob"); // agent first, then bob
  const before = messages(f).length;
  await app.keyboard.press("down");
  await app.keyboard.press("enter"); // a real Return key, as verified on the GTK host too
  await app.waitFor("id:mention-chip:bob");
  expect(app.shows("id:mention-chip:agent")).toBe(false);
  expect(app.value("id:composer")).toBe("ping @bob ");
  expect(messages(f).length).toBe(before);
  // Up wraps to the last; with the list closed the arrows are the caret's again.
  await app.type("id:composer", "@");
  await app.waitFor("id:pick:agent");
  await app.keyboard.press("up");
  await app.keyboard.press("up");
  await app.submit("id:composer");
  await app.waitFor("id:mention-chip:agent");
  await app.keyboard.press("down");
  expect(app.value("id:composer")).toBe("ping @bob @agent ");
  expect(messages(f).length).toBe(before);
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

test("recipient pills say who a message is for; only what no pill says is added", () => {
  const agent = { kind: "agent" as const };
  const bob = { kind: "user" as const, id: "bob" };
  const shown = (a: string | undefined, m?: Parameters<typeof headerAudience>[1]) => {
    const h = headerAudience(a, m);
    return [h.agentBadge, h.peopleNote];
  };
  expect(shown("agent", [agent])).toEqual([false, false]); // typed @agent
  expect(shown("agent", [{ kind: "agent", addressedBy: "classifier" }])).toEqual([false, false]); // detected
  expect(shown("agent", [{ kind: "agent", addressedBy: "schedule" }])).toEqual([false, false]); // scheduled
  expect(shown("users", [bob])).toEqual([false, false]); // no "to people"
  expect(shown("mixed", [agent, bob])).toEqual([false, false]); // two pills, no phrase
  expect(shown("agent", undefined)).toEqual([true, false]); // Ask agent: one small agent badge
  expect(shown("mixed", [bob])).toEqual([true, false]); // Ask agent with @bob
  expect(shown("users", undefined)).toEqual([false, true]); // legacy event without recipients
  expect(shown("room", undefined)).toEqual([false, false]);
  expect(headerAudience("mixed", [agent, bob]).spoken).toBe("Addressed to the agent and people");
});

test("headers drop 'to people' and repeat no agent audience; Ask agent shows one agent badge", async () => {
  const f = world();
  const toMe = f.post("acme/lobby", { kind: "message", actor: "bob", text: "@alice look", mentions: [{ kind: "user", id: "alice" }], audience: "users" });
  const both = f.post("acme/lobby", { kind: "message", actor: "bob", text: "@alice and @agent", mentions: [{ kind: "user", id: "alice" }, { kind: "agent" }], audience: "mixed" });
  const asked = f.post("acme/lobby", { kind: "message", actor: "bob", text: "asked with the button", audience: "agent" });
  const app = await inAcme(f);
  await app.waitFor("text:asked with the button");
  expect(app.shows("text:to people")).toBe(false);
  expect(app.shows("text:to the agent and people")).toBe(false);
  expect(app.shows(`id:recipient:${toMe.id}:alice`)).toBe(true);
  expect(app.shows(`id:recipient:${both.id}:alice`)).toBe(true);
  expect(app.shows(`id:recipient:${both.id}:agent`)).toBe(true);
  expect(app.count(`id:event:${both.id} >> text:to the agent`)).toBe(0);
  expect(app.shows(`id:recipient:${asked.id}:ask-agent >> text:to the agent`)).toBe(true);
  expect(app.shows("label:Addressed to the agent (Ask agent)")).toBe(true);
});

test("disabled Send and Ask agent are muted, never a dimmed brand fill, in both themes", async () => {
  const norm = (c: string | null | undefined) => (c ?? "").toLowerCase().replace(/^#([0-9a-f]{6})ff$/, "#$1");
  for (const scheme of ["dark", "light"] as const) {
    const palette = theme.cssVars[scheme] as Record<string, string>;
    const f = world();
    const app = await inAcme(f, { [APPEARANCE_KEY]: scheme });
    expect(app.enabled("id:send")).toBe(false);
    for (const id of ["id:send", "id:ask-agent"]) {
      const style = app.viewStyle(id)!;
      expect(norm(style.backgroundColor)).toBe(palette.muted!.toLowerCase());
      expect(style.effectiveOpacity).toBe(1);
    }
    await app.type("id:composer", "ready");
    expect(norm(app.viewStyle("id:send")!.backgroundColor)).toBe(palette.primary!.toLowerCase());
    expect(norm(app.viewStyle("id:ask-agent")!.backgroundColor)).toBe(palette["tone-agent"]!.toLowerCase());
    expect(f.posts().some((p) => p.path.endsWith("/messages"))).toBe(false); // the chip asks only when pressed
  }
});

test("at 720 and 860 the focused hint keeps its full wording beside Ask agent and Send", async () => {
  for (const width of [720, 860]) {
    const f = world();
    const app = await inAcme(f);
    await app.setViewport(width, 800);
    await app.type("id:composer", "x");
    await app.waitFor("id:composer-hint");
    expect(app.shows("label:Return sends · Shift+Return adds a line · @agent or Ask agent asks this Space's agent; @name only notifies them.")).toBe(true);
    expect(app.enabled("id:ask-agent")).toBe(true);
    expect(app.enabled("id:send")).toBe(true);
  }
});
