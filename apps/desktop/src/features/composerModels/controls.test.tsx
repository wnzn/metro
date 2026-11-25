import { expect, open, test } from "nsh/test";
import { BASE, world } from "../../../test/fakeServer";
import { launched } from "../../../test/launched";

const DEFAULT_LABEL = "label:Model: Default model, your normal routing · Thinking: Provider default";

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
  const app = await open("./test/Harness.tsx", { modules: launched(), network: f.network as any });
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
  await app.waitFor(DEFAULT_LABEL);
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
  const app = await open("./test/Harness.tsx", { modules: launched(), network: f.network as any });
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
  const app = await open("./test/Harness.tsx", { modules: launched(), network: f.network as any });
  await app.waitFor(DEFAULT_LABEL);
  await app.press("id:composer-model");
  await app.waitFor("id:composer-model-options");
  await app.press("id:composer-model-options");
  await app.waitFor("id:composer-model-options-group:Anthropic");
  expect(app.shows("id:composer-model-options-group:Custom & self-hosted")).toBe(true);
  expect(app.shows("id:mark-letters:{}")).toBe(true);
  expect(app.shows("text:Thinking: low · medium · high")).toBe(true);
  await app.press("id:composer-model-options-group-toggle:OpenAI");
  await app.waitFor("label:OpenAI, 2 models, collapsed");
  expect(app.shows('id:composer-model-options:["plan","gpt-x"]')).toBe(false);
  await app.type("id:composer-model-options-search", "OpenAI");
  await app.waitFor('id:composer-model-options:["claude","opus"]', { state: "gone" });
  expect(app.shows('id:composer-model-options:["plan","gpt-x"]')).toBe(true);
  expect(app.shows('id:composer-model-options:["api","gpt-x"]')).toBe(true);
  expect(app.shows("id:composer-model-options:")).toBe(true);
  // Same model id on two connections of one provider: each keeps its own connection and levels.
  await app.press('id:composer-model-options:["api","gpt-x"]');
  expect(app.shows("id:composer-thinking-options")).toBe(false);
  await app.press("label:Done");
  await app.waitFor("label:Model: gpt-x · Work key · OpenAI · Thinking: Provider default");
  await app.press("id:composer-model");
  await app.waitFor("id:composer-model-options");
  await app.press("id:composer-model-options");
  expect(app.shows("id:composer-model-options-group-toggle:OpenAI")).toBe(false); // selection keeps it open
  await app.press('id:composer-model-options:["plan","gpt-x"]');
  await app.press("id:composer-thinking-options:medium");
  await app.press("label:Done");
  await app.waitFor("label:Model: gpt-x · My plan · OpenAI · Thinking: medium");
  await app.press("id:composer-model");
  await app.waitFor("id:composer-model-options");
  await app.press("id:composer-model-options");
  await app.press("id:composer-model-options:");
  await app.press("label:Done");
  await app.waitFor(DEFAULT_LABEL);
  await app.press("id:composer-model");
  await app.press("id:composer-model-options");
  await app.waitFor("label:OpenAI, 2 models, collapsed"); // selection never overwrites disclosure memory
});

test("names that fail or arrive late never block choosing; they rename groups in place and keep the pin and effort", async () => {
  const f = world();
  const get = `GET ${BASE}/v1/*`;
  const base = f.network[get]!;
  const order: string[] = [];
  let namesUp = false;
  f.network[get] = async req => {
    const url = new URL(req.url);
    if (url.pathname === "/v1/provider-catalog") {
      // Naming fails until the test lets it answer, then answers only well after the rows.
      if (!namesUp) return { status: 503, body: { error: "unavailable" } };
      await new Promise(r => setTimeout(r, 1500));
      order.push("names");
      return { status: 200, body: { providers: [{ id: "deepseek", name: "DeepSeek", requires_key: true, allow_custom_url: false,
        execution: "server", capability: "chat", lists_models: true }], allowed_base_urls: [] } };
    }
    if (url.pathname !== "/v1/models/available") return base(req);
    order.push("rows");
    return { status: 200, body: { models: [{ connection_id: "ds", name: "DS key", provider_id: "deepseek", capability: "chat",
      owned: true, execution: "server", runs_on: "your_connection", chat_models: [{ id: "r1", reasoning_efforts: ["low", "high"] }] }] } };
  };
  const app = await open("./test/Harness.tsx", { modules: launched(), network: f.network as any });
  // The unpinned trigger says Default model and its default thinking, naming no routed model.
  await app.waitFor(DEFAULT_LABEL);
  await app.waitFor("text:Default model");
  await app.waitFor("text:· Provider default");
  await app.press("id:composer-model");
  await app.waitFor("id:composer-model-options");
  await app.press("id:composer-model-options");
  // Names failed: the group falls back to the provider id and every row is still a choice.
  await app.waitFor("id:composer-model-options-group:deepseek");
  expect(app.shows("text:Thinking: Provider default")).toBe(true);
  await app.press('id:composer-model-options:["ds","r1"]');
  await app.press("id:composer-thinking-options:high");
  await app.press("label:Done");
  await app.waitFor("label:Model: r1 · DS key · deepseek · Thinking: high");
  // Reopening refreshes: the rows land first and the names well after, renaming the
  // open picker's group while the choice and level stay.
  namesUp = true;
  const mark = order.length;
  await app.press("id:composer-model");
  await app.waitFor("text:DeepSeek", { timeout: 6000 });
  const tail = order.slice(mark);
  expect(tail.indexOf("rows")).toBeGreaterThan(-1);
  expect(tail.indexOf("rows")).toBeLessThan(tail.indexOf("names"));
  expect(app.shows("id:composer-thinking-options:high")).toBe(true);
  await app.press("label:Done");
  await app.waitFor("label:Model: r1 · DS key · DeepSeek · Thinking: high");
});

test("the keyboard alone searches, chooses a model and a thinking level", async () => {
  const f = world();
  const get = `GET ${BASE}/v1/*`;
  const base = f.network[get]!;
  f.network[get] = req => {
    const url = new URL(req.url);
    if (url.pathname !== "/v1/models/available") return base(req);
    return { status: 200, body: { models: [
      { connection_id: "api", name: "Work key", provider_id: "openai", capability: "chat", owned: true, execution: "server",
        runs_on: "your_connection", chat_models: [{ id: "gpt-x", reasoning_efforts: [] }] },
      { connection_id: "claude", name: "Claude", provider_id: "anthropic", capability: "chat", owned: true, execution: "server",
        runs_on: "your_connection", chat_models: [{ id: "opus", reasoning_efforts: ["low", "high"] }] },
    ] } };
  };
  const app = await open("./test/Harness.tsx", { modules: launched(), network: f.network as any, platform: "linux" });
  await app.waitFor(DEFAULT_LABEL);
  await app.press("id:composer-model");
  await app.waitFor("id:composer-model-options");
  await app.press("id:composer-model-options");
  await app.type("id:composer-model-options-search", "opus");
  await app.keyboard.press("return");
  await app.waitFor("id:composer-thinking-options");
  // Return leaves the focus on the model list's trigger; Tab passes Fetch models to the levels.
  await app.keyboard.press("tab");
  await app.keyboard.press("tab");
  await app.keyboard.press("right");
  await app.keyboard.press("space");
  await app.keyboard.press("escape");
  await app.waitFor("label:Model: opus · Claude · Anthropic · Thinking: low");
});

test("rows show the model's display name with its id beneath, custom thinking levels included; Default names the operator's model when it runs", async () => {
  const f = world();
  const get = `GET ${BASE}/v1/*`;
  const base = f.network[get]!;
  let operator: { model_id: string; label: string } | null = null;
  let rows: unknown[] = [{
    connection_id: "own", name: "Home vLLM", provider_id: "custom", model: "qwen3-32b", capability: "chat",
    execution: "server", visibility: "personal", owned: true, owner_id: "alice", runs_on: "your_connection", billing: "you",
    chat_models: [{ id: "qwen3-32b", display_name: "Qwen 3 (home)", reasoning_efforts: ["think_low", "think_high"], is_default: true, available: true }],
  }];
  f.network[get] = req => {
    const url = new URL(req.url);
    if (url.pathname !== "/v1/models/available") return base(req);
    return { status: 200, body: { models: rows, operator_model: operator } };
  };
  const app = await open("./test/Harness.tsx", { modules: launched(), network: f.network as any });
  await app.waitFor("id:composer-model");
  await app.press("id:composer-model");
  await app.press("id:composer-model-options");
  const row = 'id:composer-model-options:["own","qwen3-32b"]';
  await app.waitFor(row);
  expect(app.shows(`${row} >> text:Qwen 3 (home)`)).toBe(true);
  expect(app.shows(`${row} >> text:qwen3-32b`)).toBe(true); // the id, muted beneath
  expect(app.shows("text:Thinking: think_low · think_high")).toBe(true); // an owner's own levels are kept
  await app.press(row);
  await app.press("id:composer-thinking-options:think_high");
  await app.press("label:Done");
  await app.waitFor("id:composer-model >> text:Qwen 3 (home)"); // the closed trigger says the name
  await app.waitFor("text:· think_high");

  // No chat model of my own, but the operator's: Default says so and stays selectable.
  rows = [];
  operator = { model_id: "operator-fixture", label: "Server model (operator)" };
  await app.press("label:Personal");
  await app.press("id:composer-model");
  await app.waitFor("id:composer-operator-model");
  expect(app.shows("text:Default runs server model (operator): operator-fixture. Add your own model in Model providers.")).toBe(true);
  await app.press("id:composer-model-options");
  await app.waitFor("text:Server model (operator): operator-fixture");
});
