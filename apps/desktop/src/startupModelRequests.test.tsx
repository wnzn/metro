import { expect, open, test } from "nsh/test";
import type { App } from "nsh/test";
import { BASE, TOKENS, world } from "../test/fakeServer";
import { launched } from "../test/launched";

const models = "/v1/models/available";
const catalog = "/v1/provider-catalog";
const providers = "/v1/providers";
const acmeModels = `${models}?space_id=acme`;
const otherModels = `${models}?space_id=other`;
type Fixture = ReturnType<typeof world>;

const calls = (f: Fixture, path: string, user?: string) =>
  f.state.log.filter(r => r.method === "GET" && r.path === path && (!user || r.user === user)).length;

const launch = (f: Fixture, storage?: Record<string, string>) =>
  open("../App.tsx", { network: f.network as any, modules: launched(), clock: {}, storage,
    platform: "linux", width: 1200, height: 840 });

async function signIn(app: App, account = "alice") {
  await app.type("id:access-token", TOKENS[account]!);
  await app.press("id:connect");
  await app.waitFor("id:workspace", { timeout: 150 });
}

test("empty Chat shares pending model and provider catalog reads, and shows onboarding only after readiness", async () => {
  const f = world();
  f.answerLate(models, TOKENS.alice!, 500);
  f.answerLate(catalog, TOKENS.alice!, 800);
  const app = await launch(f);
  await signIn(app);
  await app.waitFor("id:chat-empty", { timeout: 100 });
  expect(calls(f, "/v1/chat/events?after=0")).toBe(1);
  expect(calls(f, models)).toBe(1);
  expect(calls(f, catalog)).toBe(1);
  expect(calls(f, providers)).toBe(0);
  expect(app.shows("id:chat-onboarding")).toBe(false);

  await app.waitFor("id:chat-onboarding:device-openai-oauth", { timeout: 900 });
  expect(calls(f, models)).toBe(1);
  expect(calls(f, catalog)).toBe(1);
  expect(calls(f, providers)).toBe(1);
});

test("empty Chat with the operator's model does not fetch onboarding connections", async () => {
  const f = world();
  f.state.operatorModel = "operator-fixture";
  f.answerLate(models, TOKENS.alice!, 400);
  const app = await launch(f);
  await signIn(app);
  await app.waitFor("id:chat-empty", { timeout: 100 });
  await app.clock!.runFor(500);
  expect(calls(f, models)).toBe(1);
  expect(calls(f, catalog)).toBe(1);
  expect(calls(f, providers)).toBe(0);
  expect(app.shows("id:chat-onboarding")).toBe(false);
  await app.press("id:composer-model");
  await app.waitFor("id:composer-operator-model", { timeout: 450 });
  expect(calls(f, models)).toBe(2); // a later, explicit check was not cached
  expect(app.texts().join(" ")).toContain("operator-fixture");
});

test("a server without the optional model routes still opens its remembered transcript", async () => {
  const f = world();
  const get = f.network[`GET ${BASE}/v1/*`]!;
  const unavailable: string[] = [];
  f.network[`GET ${BASE}/v1/*`] = req => {
    const { pathname, search } = new URL(req.url);
    if (pathname === models || pathname === catalog) {
      unavailable.push(pathname + search);
      return { status: 404, body: { error: "not found" }, delay: 400 };
    }
    return get(req);
  };
  const app = await launch(f, { "metro.lastSpace": JSON.stringify({ server: BASE, space: "acme" }) });
  await signIn(app);
  await app.waitFor("text:hello from bob in acme", { timeout: 150 });
  expect(unavailable.filter(p => p === acmeModels)).toHaveLength(1);
  await app.press("id:composer-model");
  await app.waitFor("id:composer-retry-catalog", { timeout: 500 });
  expect(unavailable.filter(p => p === acmeModels)).toHaveLength(2);
  expect(unavailable).not.toContain(models);
  expect(app.shows("text:hello from bob in acme")).toBe(true);
});

test("pending exact-Space availability never serves another Space or the next account", async () => {
  const f = world();
  f.addSpace("other", "Other", "alice");
  f.answerLate(acmeModels, TOKENS.alice!, 800);
  f.answerLate(otherModels, TOKENS.alice!, 800);
  f.answerLate(acmeModels, TOKENS.bob!, 800);
  const app = await launch(f, { "metro.lastSpace": JSON.stringify({ server: BASE, space: "acme" }) });
  await signIn(app);
  await app.waitUntil(() => calls(f, acmeModels, "alice") > 0, { timeout: 150 });
  expect(calls(f, acmeModels, "alice")).toBe(1);
  await app.press("id:space:other");
  await app.waitUntil(() => calls(f, otherModels, "alice") > 0, { timeout: 150 });
  expect(calls(f, otherModels, "alice")).toBe(1);
  // Return to Acme so the new account will restore the same Space and endpoint.
  await app.press("id:space:acme");
  await app.waitFor("text:hello from bob in acme", { timeout: 150 });
  expect(calls(f, acmeModels, "alice")).toBe(2); // a new visit doesn't reuse the previous visit's read
  await app.press("id:account-menu");
  await app.press("id:disconnect");
  await app.waitFor("id:connect-screen", { timeout: 100 });
  await signIn(app, "bob");
  await app.waitUntil(() => calls(f, acmeModels, "bob") > 0, { timeout: 150 });
  expect(calls(f, acmeModels, "bob")).toBe(1);
  expect(calls(f, otherModels, "bob")).toBe(0);
  expect(calls(f, models)).toBe(0);
});

test("opening the picker rechecks a share before the older startup reply arrives", async () => {
  const f = world();
  const loan = f.addConnection("bob", "openai", "Shared team model", "acme-only", { visibility: "spaces", space_ids: ["acme"] });
  f.state.operatorModel = "old-operator";
  const old = f.answerLate(acmeModels, TOKENS.alice!, 850);
  const app = await launch(f, { "metro.lastSpace": JSON.stringify({ server: BASE, space: "acme" }) });
  await signIn(app);
  await app.waitUntil(() => old.arrived() === 1, { timeout: 150 });
  loan.share = { visibility: "personal", space_ids: [] };
  f.state.operatorModel = null;
  f.answerLate(acmeModels, TOKENS.alice!, 20);
  await app.press("id:composer-model");
  await app.waitFor("text:/No chat models are ready here/", { timeout: 160 });
  await app.clock!.runFor(870);
  expect(calls(f, acmeModels)).toBe(2);
  expect(app.shows("text:/No chat models are ready here/")).toBe(true);
  expect(app.shows("id:composer-model-options")).toBe(false);
  expect(app.shows("id:composer-operator-model")).toBe(false);
});
