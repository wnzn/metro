import { expect, open, test } from "nsh/test";
import type { App } from "nsh/test";
import { BASE, TOKENS, world } from "../../test/fakeServer";
import { composerAddress } from "./composerAddress";
import { attachmentWorld } from "./attachments/test/fixtures";
import { launched } from "../../test/launched";

function setup(systemOne = false) {
  const f = world();
  f.addConnection("alice", "openai", "Default chat", "legacy");
  const target = f.addConnection("alice", "openai", "Work", "other");
  if (systemOne) f.addConnection("alice", "systemone", "Classifier", "jev-latest");
  const key = `GET ${BASE}/v1/*`;
  const get = f.network[key]!;
  f.network[key] = async req => {
    const answer = await get(req) as { status: number; body: { models?: { connection_id: string; chat_models?: unknown }[] } };
    for (const m of answer.body?.models ?? []) if (m.connection_id === target.id)
      m.chat_models = [{ id: "review", reasoning_efforts: ["low", "high"] }];
    return answer;
  };
  return { f, target, posts: () => f.posts().filter(p => p.path.endsWith("/messages")) };
}

async function chat(f: Pick<ReturnType<typeof world>, "network">) {
  const app = await open("../../App.tsx", { modules: launched(), network: f.network as any, platform: "linux", width: 1200, height: 800 });
  await app.clear("id:server-url");
  await app.type("id:server-url", BASE);
  await app.type("id:access-token", TOKENS.alice!);
  await app.press("id:connect");
  await app.waitFor("id:composer-model");
  return app;
}

async function pin(app: App, target: { id: string }) {
  await app.press("id:composer-model");
  await app.waitFor("id:composer-model-options");
  await app.press("id:composer-model-options");
  await app.press(`id:composer-model-options:${JSON.stringify([target.id, "review"])}`);
  await app.press("id:composer-thinking-options:high");
  await app.press("text:Done");
}

test("Chat Return sends directly with the picked model, even with @text; Save note omits it and Space addressing is unchanged", async () => {
  const { f, posts, target } = setup();
  const app = await chat(f);
  await pin(app, target);
  const chosen = { provider_connection_id: target.id, model_id: "review", reasoning_effort: "high" };
  await app.type("id:composer", "Please review");
  await app.waitFor("id:send");
  expect(app.shows("id:ask-agent")).toBe(false);
  await app.submit("id:composer");
  await app.waitUntil(() => posts().length === 1);
  expect(posts()[0]!.body).toEqual({ text: "Please review", invoke_agent: true, ...chosen });
  await app.type("id:composer", "@agent remember tomorrow");
  expect(app.enabled("id:save-note")).toBe(true);
  await app.press("text:Save note");
  await app.waitUntil(() => posts().length === 2);
  expect(posts()[1]!.body).toEqual({ text: "@agent remember tomorrow", save_note: true });
  expect(f.state.events.get("chat:alice")!.at(-1)).toMatchObject({ kind: "message", audience: "room" });
  await app.type("id:composer", "@bob please review");
  await app.submit("id:composer");
  await app.waitUntil(() => posts().length === 3);
  expect(posts()[2]!.body).toEqual({ text: "@bob please review", invoke_agent: true, ...chosen });
  await app.press("id:space:acme");
  await app.waitFor("text:hello from bob in acme");
  await app.type("id:composer", "@agent please review");
  await app.submit("id:composer");
  await app.waitUntil(() => posts().length === 4);
  expect(posts()[3]!.body.provider_connection_id).toBeUndefined();
  expect(posts()[3]!.body.invoke_agent).toBe(false);
  await pin(app, target);
  await app.type("id:composer", "demo ` @agent ` bob@agent here");
  await app.submit("id:composer");
  await app.waitUntil(() => posts().length === 5);
  expect(posts()[4]!.body.provider_connection_id).toBeUndefined();
  expect(posts()[4]!.body.invoke_agent).toBe(false);
  await app.type("id:composer", "@bob @AGENT, now");
  await app.press("id:send");
  await app.waitUntil(() => posts().length === 6);
  expect(posts()[5]!.body).toMatchObject({ invoke_agent: false, ...chosen });
  await app.type("id:composer", "@bob please review together");
  await app.press("id:ask-agent");
  await app.waitUntil(() => posts().length === 7);
  expect(posts()[6]!.body).toMatchObject({ invoke_agent: true, ...chosen });
});

test("with System One available, Send still asks directly; a stale pin blocks Send but not Save note", async () => {
  const { f, target, posts } = setup(true);
  const app = await chat(f);
  await pin(app, target);
  await app.type("id:composer", "direct conversation");
  await app.submit("id:composer");
  await app.waitUntil(() => posts().length === 1);
  expect(posts()[0]!.body).toEqual({
    text: "direct conversation", invoke_agent: true, provider_connection_id: target.id, model_id: "review", reasoning_effort: "high",
  });
  f.state.connections.delete(target.id);
  await app.press("id:composer-model");
  await app.waitFor("id:composer-model-options");
  await app.press("text:Done");
  // Rust treats NEL as whitespace, so legacy false + this text would ask.
  const text = "keep this\u0085@agent as literal text";
  expect(composerAddress(text).agent).toBe(false);
  await app.type("id:composer", text);
  await app.press("id:send");
  await app.waitFor("id:action-error");
  expect(posts()).toHaveLength(1);
  expect(app.value("id:composer")).toBe(text);
  await app.press("id:save-note");
  await app.waitUntil(() => posts().length === 2);
  expect(posts()[1]!.body).toEqual({ text, save_note: true });
  expect(f.state.events.get("chat:alice")!.at(-1)).toMatchObject({ kind: "message", audience: "room" });
});

test("an older server rejects Save note without any retry, keeping the draft", async () => {
  const { f, posts } = setup();
  const key = `POST ${BASE}/v1/*`;
  const post = f.network[key]!;
  const attempts: unknown[] = [];
  f.network[key] = req => {
    if (new URL(req.url).pathname === "/v1/chat/messages") {
      const body = JSON.parse(req.body!);
      attempts.push(body);
      if ("save_note" in body) return { status: 422, body: { error: "unknown field `save_note`" } };
    }
    return post(req);
  };
  const app = await chat(f);
  const text = "keep this\u0085@agent as literal text";
  await app.type("id:composer", text);
  await app.press("id:save-note");
  await app.waitFor("text:/Update Metro Server to use Save note/");
  expect(app.value("id:composer")).toBe(text);
  expect(app.enabled("id:save-note")).toBe(true);
  expect(attempts).toEqual([{ text, save_note: true }]);
  expect(posts()).toHaveLength(0);
  expect(f.state.events.get("chat:alice") ?? []).toEqual([]);
});

test("a file-only Chat send starts a turn using the pinned model", async () => {
  const w = attachmentWorld();
  const notes = w.f.addConnection("alice", "openai", "Notes", "review");
  const app = await chat(w);
  await app.press("id:composer-model");
  await app.waitFor("id:composer-model-options");
  await app.press("id:composer-model-options");
  await app.press(`id:composer-model-options:${JSON.stringify([notes.id, null])}`);
  await app.press("text:Done");
  app.onPick(() => [{ name: "notes.txt", type: "text/plain", data: new TextEncoder().encode("a note") }]);
  await app.press("id:attach");
  await app.waitFor("id:staged:notes.txt");
  expect(app.enabled("id:send")).toBe(true);
  await app.submit("id:composer");
  await app.waitFor("id:attachments", { state: "gone" });
  expect(w.f.posts().find(p => p.path === "/v1/chat/messages")?.body).toEqual({
    text: "", invoke_agent: true, asset_ids: [w.l.assets[0]!.id], provider_connection_id: notes.id,
  });
});

test("an empty composer catalog offers the provider setup without contacting a model", async () => {
  const f = world();
  const app = await chat(f);
  await app.press("id:composer-model");
  await app.waitFor("id:composer-open-providers");
  expect(app.shows("id:composer-model-options")).toBe(false);
  await app.press("id:composer-open-providers");
  await app.waitFor("id:provider-picker");
  expect(f.posts().filter(p => p.path.endsWith("/models") || p.path.endsWith("/messages"))).toHaveLength(0);
});

test("unloaded and loaded catalogs give Chat Send the same direct behavior", async () => {
  const { f, posts } = setup();
  f.state.delays.set("/v1/models/available", 600);
  const app = await chat(f);
  await app.type("id:composer", "already writing");
  await app.submit("id:composer");
  await app.waitUntil(() => posts().length === 1);
  expect(posts()[0]!.body).toEqual({ text: "already writing", invoke_agent: true });
  await app.wait(700);
  await app.type("id:composer", "fresh draft");
  await app.submit("id:composer");
  await app.waitUntil(() => posts().length === 2);
  expect(posts()[1]!.body).toEqual({ text: "fresh draft", invoke_agent: true });
});

test("agent addressing matches standalone handles outside inline and fenced code", () => {
  for (const text of ["` @agent `", "`` odd ` @agent ``", "```sh\n@agent\n```", "~~~js\n@agent", "mail@agent", "\\@agent", "@agent.example", "@agent's", "@agent/foo", "@agent/@bob", "https://x/@agent"])
    expect(composerAddress(text).agent).toBe(false);
  for (const text of ["@agent", "(@AgEnT), hi", "unmatched ` @agent?", "```\n@agent\n```\n @agent!"])
    expect(composerAddress(text).agent).toBe(true);
  expect(composerAddress("@agent.", ["agent."])).toEqual({ agent: false, people: true });
  expect(composerAddress("@bob @agent now")).toEqual({ agent: true, people: true });
});
