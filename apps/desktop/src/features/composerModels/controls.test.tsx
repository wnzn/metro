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
  await app.press("id:composer-thinking-options");
  await app.press("text:high");
  await app.press("label:Done");
  await app.waitFor("label:Model: b · Team · openai · high");
  await app.press("label:Personal");
  await app.waitFor("label:Model: Default");
  expect(requests.at(-1)).toBe("");
  expect(requests.includes("?scope=any")).toBe(false);
});
