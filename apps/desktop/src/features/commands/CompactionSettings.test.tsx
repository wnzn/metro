import { expect, open, test } from "nsh/test";
import type { App } from "nsh/test";
import { BASE, TOKENS, world } from "../../../test/fakeServer";
import { launched } from "../../../test/launched";

async function settings(f: ReturnType<typeof world>, page: "routing" | "providers", user = "alice") {
  const app = await open("../../../App.tsx", { modules: launched(), network: f.network as any, platform: "linux", width: 900, height: 840 });
  await app.clear("id:server-url");
  await app.type("id:server-url", BASE);
  await app.type("id:access-token", TOKENS[user]!);
  await app.press("id:connect");
  await app.press("id:nav-settings");
  await app.press(`id:nav-settings:${page}`);
  return app;
}

async function replace(app: App, id: string, text: string) {
  await app.clear(id);
  if (text) await app.type(id, text);
}

test("summary preferences default on at 70%; only whole percentages from 40 to 90 save", async () => {
  const f = world();
  let prefs = { auto: true, at_percent: 70, min_percent: 40, max_percent: 90, default_context_tokens: 32_000 };
  const writes: unknown[] = [];
  const get = `GET ${BASE}/v1/*`, put = `PUT ${BASE}/v1/*`;
  const fallbackGet = f.network[get]!, fallbackPut = f.network[put]!;
  f.network[get] = req => new URL(req.url).pathname === "/v1/compaction" ? { status: 200, body: prefs } : fallbackGet(req);
  f.network[put] = req => {
    if (new URL(req.url).pathname !== "/v1/compaction") return fallbackPut(req);
    const patch = JSON.parse(req.body!);
    writes.push(patch);
    prefs = { ...prefs, ...patch };
    return { status: 200, body: prefs };
  };
  const app = await settings(f, "routing");
  const field = "id:compaction-percent";
  await app.waitFor(field);
  expect(app.value(field)).toBe("70");
  expect(app.enabled("id:compaction-save")).toBe(false);
  await app.press("id:compaction-auto");
  for (const bad of ["39", "91", "80.5", ""]) {
    await replace(app, field, bad);
    expect(app.enabled("id:compaction-save")).toBe(false);
    expect(app.shows("text:Enter a whole number from 40 to 90.")).toBe(true);
  }
  expect(writes).toHaveLength(0);
  for (const edge of ["40", "90"]) {
    await replace(app, field, edge);
    expect(app.enabled("id:compaction-save")).toBe(true);
    await app.press("id:compaction-save");
    await app.waitFor("text:Summary settings saved.");
  }
  expect(writes).toEqual([{ auto: false, at_percent: 40 }, { auto: false, at_percent: 90 }]);
  const reopened = await settings(f, "routing");
  await reopened.waitFor(field);
  expect(reopened.value(field)).toBe("90");
});

test("only the owner can edit or clear a model's context size, or set it when adding a model", async () => {
  const f = world();
  const c = f.addConnection("alice", "openai", "Alice's shared connection", "existing", { visibility: "spaces", space_ids: ["acme"] });
  c.models[0]!.context_tokens = 12_345;
  const app = await settings(f, "providers");
  await app.press(`id:connection:${c.id}:open`);
  const key = `${c.id}:existing`;
  const input = `id:model-context-input:${key}`;
  const save = `id:model-context-save:${key}`;
  await app.press(`id:model-context:${key}`);
  expect(app.value(input)).toBe("12345");
  for (const bad of ["1023", "10000001", "1600.5", "2e3"]) {
    await replace(app, input, bad);
    expect(app.enabled(save)).toBe(false);
    expect(app.shows(`${input}:error`)).toBe(true);
  }
  await replace(app, input, "1024");
  await app.press(save);
  await app.waitFor(input, { state: "gone" });
  await app.press(`id:model-context:${key}`);
  expect(app.value(input)).toBe("1024");
  await replace(app, input, "");
  await app.press(save);
  await app.waitFor(input, { state: "gone" });
  expect(f.posts().filter(p => p.method === "PATCH" && p.path.endsWith("/model-list")).map(p => p.body)).toEqual([
    { model_id: "existing", context_tokens: 1024 }, { model_id: "existing", context_tokens: null },
  ]);
  await app.press(`id:model-add-open:${c.id}`);
  await app.type(`id:model-add:${c.id}-id`, "new-model");
  await app.type(`id:model-add:${c.id}-context`, "10000000");
  await app.press(`id:model-add:${c.id}-save`);
  await app.waitFor(`id:model-row:${c.id}:new-model`);
  expect(f.posts().find(p => p.method === "POST" && p.path.endsWith("/model-list"))?.body).toEqual({
    model_id: "new-model", context_tokens: 10_000_000,
  });
  const bob = await settings(f, "providers", "bob");
  await bob.waitFor("id:providers-settings");
  expect(bob.shows(`id:connection:${c.id}:open`)).toBe(false);
  expect(bob.shows(`id:model-context:${key}`)).toBe(false);
});
