import { expect, open, test } from "nsh/test";
import { BASE, world } from "../../../test/fakeServer";

test("composer refreshes exact-scope metadata on open and drops a shared model on returning to personal Chat", async () => {
  const f = world();
  const get = `GET ${BASE}/v1/*`;
  const base = f.network[get]!;
  const requests: string[] = [];
  f.network[get] = req => {
    const url = new URL(req.url);
    if (url.pathname !== "/v1/models/available") return base(req);
    requests.push(url.search);
    return { status: 200, body: { models: url.search === "?space_id=acme" ? [{
      connection_id: "shared", name: "Team", provider_id: "openai", model: "a", capability: "chat",
      execution: "server", visibility: "spaces", owned: false, owner_id: "bob",
      runs_on: "owner_connection", billing: "owner",
      chat_models: [{ id: "b", reasoning_efforts: ["high"] }],
    }] : [] } };
  };
  const app = await open("./test/Harness.tsx", { network: f.network as any });
  await app.waitFor("id:composer-model");
  await app.waitUntil(() => requests.includes(""));
  await app.press("label:Acme");
  await app.waitUntil(() => requests.includes("?space_id=acme"));
  const count = requests.length;
  await app.press("id:composer-model");
  await app.waitUntil(() => requests.length > count);
  expect(requests.at(-1)).toBe("?space_id=acme");
  await app.press("id:composer-model-options");
  // Grouped under the provider's canonical name and bundled mark; levels readable on the row.
  await app.waitFor("id:composer-model-options-group:OpenAI");
  expect(app.shows("id:mark-logo:openai")).toBe(true);
  expect(app.shows("text:Thinking: high")).toBe(true);
  await app.press('id:composer-model-options:["shared","b"]');
  await app.waitFor("text:bob can see these requests and may pay for them.");
  expect(app.shows("id:composer-fetch-models")).toBe(false);
  // Every advertised level is on screen at once, with Provider default chosen.
  expect(app.shows("id:composer-thinking-options:")).toBe(true);
  await app.press("label:Done");
  await app.waitFor("label:Model: b · Team · OpenAI · Thinking: Provider default");
  await app.waitFor("text:· Provider default");
  await app.press("id:composer-model");
  await app.press("id:composer-thinking-options:high");
  await app.press("id:composer-model-options");
  await app.waitFor("text:Thinking: high");
  await app.press('id:composer-model-options:["shared","b"]');
  await app.press("label:Done");
  await app.waitFor("label:Model: b · Team · OpenAI · Thinking: high");
  await app.waitFor("text:· high");
  expect(app.shows("text:b")).toBe(true);
  await app.press("label:Personal");
  await app.waitFor("label:Model: Default");
  expect(requests.at(-1)).toBe("");
  expect(requests.includes("?scope=any")).toBe(false);
});

test("listing an owned profile is explicit; only refreshed catalog rows become choices", async () => {
  const f = world();
  const calls: string[] = [];
  let listed = false, fail = true;
  const get = `GET ${BASE}/v1/*`, post = `POST ${BASE}/v1/*`;
  const baseGet = f.network[get]!, basePost = f.network[post]!;
  f.network[get] = req => {
    const url = new URL(req.url);
    if (url.pathname !== "/v1/models/available") return baseGet(req);
    calls.push(`catalog${url.search}`);
    const ids = listed ? ["base", "approved"] : ["base"];
    return { status: 200, body: { models: [{ connection_id: "own", name: "Work", provider_id: "openai",
      capability: "chat", owned: true, execution: "server", runs_on: "your_connection",
      chat_models: ids.map(id => ({ id, reasoning_efforts: [] })) }] } };
  };
  f.network[post] = req => {
    if (new URL(req.url).pathname !== "/v1/providers/own/models") return basePost(req);
    calls.push("list");
    if (fail) return { status: 400, body: { error: "unsupported" } };
    listed = true;
    return { status: 200, body: { models: ["unverified"] } };
  };
  const app = await open("./test/Harness.tsx", { network: f.network as any });
  await app.waitFor("id:composer-model");
  await app.press("label:Acme");
  await app.waitUntil(() => calls.includes("catalog?space_id=acme"));
  await app.press("id:composer-model");
  expect(app.shows("id:composer-fetch-models")).toBe(false);
  await app.press("id:composer-model-options");
  await app.press('id:composer-model-options:["own","base"]');
  await app.waitFor("label:Fetch models from Work");
  expect(calls.includes("list")).toBe(false);
  await app.press("id:composer-fetch-models");
  await app.waitFor("text:/Could not fetch models from Work/");
  expect(calls.at(-1)).toBe("list");
  fail = false;
  await app.press("id:composer-fetch-models");
  await app.waitFor("label:Fetch models from Work");
  expect(calls.slice(-2)).toEqual(["list", "catalog?space_id=acme"]);
  await app.press("id:composer-model-options");
  await app.waitFor('id:composer-model-options:["own","approved"]');
  expect(app.shows('id:composer-model-options:["own","unverified"]')).toBe(false);
});

test("search finds models by provider name across connections; Default stays apart from provider groups", async () => {
  const f = world();
  const get = `GET ${BASE}/v1/*`;
  const base = f.network[get]!;
  const row = (id: string, name: string, provider: string, models: { id: string; reasoning_efforts: string[] }[]) => ({
    connection_id: id, name, provider_id: provider, capability: "chat", owned: true, execution: provider.startsWith("device-") ? "device" : "server",
    runs_on: "your_connection", chat_models: models,
  });
  f.network[get] = req => {
    const url = new URL(req.url);
    if (url.pathname !== "/v1/models/available") return base(req);
    return { status: 200, body: { models: [
      row("plan", "My plan", "device-openai-oauth", [{ id: "gpt-x", reasoning_efforts: ["low", "medium", "high"] }]),
      row("api", "Work key", "openai", [{ id: "gpt-x", reasoning_efforts: [] }]),
      row("claude", "Claude", "anthropic", [{ id: "opus", reasoning_efforts: [] }]),
      row("lab", "Lab box", "custom", [{ id: "qwen", reasoning_efforts: [] }]),
    ] } };
  };
  const app = await open("./test/Harness.tsx", { network: f.network as any });
  await app.waitFor("label:Model: Default");
  await app.press("id:composer-model");
  await app.press("id:composer-model-options");
  await app.waitFor("id:composer-model-options-group:Anthropic");
  expect(app.shows("id:composer-model-options-group:Custom & self-hosted")).toBe(true);
  expect(app.shows("id:mark-letters:{}")).toBe(true);
  expect(app.shows("text:Thinking: low · medium · high")).toBe(true);
  await app.type("id:composer-model-options-search", "OpenAI");
  await app.waitFor('id:composer-model-options:["claude","opus"]', { state: "gone" });
  expect(app.shows('id:composer-model-options:["plan","gpt-x"]')).toBe(true);
  expect(app.shows('id:composer-model-options:["api","gpt-x"]')).toBe(true);
  // Same model id on two connections of one provider: each keeps its own connection and levels.
  await app.press('id:composer-model-options:["api","gpt-x"]');
  expect(app.shows("id:composer-thinking-options")).toBe(false);
  await app.press("label:Done");
  await app.waitFor("label:Model: gpt-x · Work key · OpenAI · Thinking: Provider default");
  await app.press("id:composer-model");
  await app.press("id:composer-model-options");
  await app.press('id:composer-model-options:["plan","gpt-x"]');
  await app.press("id:composer-thinking-options:medium");
  await app.press("label:Done");
  await app.waitFor("label:Model: gpt-x · My plan · OpenAI · Thinking: medium");
  await app.press("id:composer-model");
  await app.press("id:composer-model-options");
  await app.press("id:composer-model-options:");
  await app.press("label:Done");
  await app.waitFor("label:Model: Default");
});
