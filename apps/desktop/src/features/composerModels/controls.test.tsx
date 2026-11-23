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
  await app.press('id:composer-model-options:["shared","b"]');
  await app.waitFor("text:bob can see these requests and may pay for them.");
  expect(app.shows("id:composer-fetch-models")).toBe(false);
  await app.press("id:composer-thinking-options");
  await app.press("text:high");
  await app.press("label:Done");
  await app.waitFor("label:Model: b · Team · openai · high");
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
  await app.press("text:base · Work · openai");
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
  await app.waitFor("text:approved · Work · openai");
  expect(app.shows("text:unverified · Work · openai")).toBe(false);
});
