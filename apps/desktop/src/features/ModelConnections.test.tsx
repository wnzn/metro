// Real Settings state and NSH controls against Metro's scripted provider API.
import { expect, open, test } from "nsh/test";
import type { App } from "nsh/test";
import { BASE, TOKENS, world } from "../../test/fakeServer";
import { launched } from "../../test/launched";

const OLD = "http://127.0.0.1:11434/v1";
const NEW = "http://127.0.0.1:11001/v1";
const OTHER = "http://127.0.0.1:11002/v1";
type F = ReturnType<typeof world>;

function fixture() {
  const f = world();
  f.state.allowedBaseUrls.push(NEW);
  const custom = f.addConnection("alice", "custom", "Local models", "old-fixture", { visibility: "organization", space_ids: [] });
  custom.base_url = OLD;
  custom.models[0]!.source = "listed";
  f.state.listings.set("custom", [{ id: "new-fixture", name: "New fixture" }]);
  return { f, custom };
}

async function signIn(f: F, user: "alice" | "bob", existing?: App, width = 1200): Promise<App> {
  const app = existing ?? await open("../../App.tsx", { modules: launched(), network: f.network as any, width, height: 900, platform: "linux" });
  await app.clear("id:server-url");
  await app.type("id:server-url", BASE);
  await app.type("id:access-token", TOKENS[user]!);
  await app.press("id:connect");
  await app.waitFor("id:nav-settings");
  await app.press("id:nav-settings");
  await app.press("id:nav-settings:providers");
  return app;
}

async function edit(app: App, id: string) {
  await app.waitFor(`id:connection:${id}:open`);
  await app.press(`id:connection:${id}:open`);
  await app.waitFor(`id:endpoint-section:${id}`);
  await app.press(`id:endpoint-change:${id}`);
  await app.waitFor(`id:endpoint-input:${id}`);
}

const patches = (f: F, id: string) => f.posts().filter((p) => p.method === "PATCH" && p.path === `/v1/providers/${id}`);

test("an owner tests and saves an approved shared endpoint with one authenticated PATCH, then refreshes models", async () => {
  const { f, custom } = fixture();
  const app = await signIn(f, "alice", undefined, 760);
  await edit(app, custom.id);
  await app.waitFor(`id:endpoint-shared:${custom.id}`);
  await app.press(`id:endpoint-section:${custom.id} >> label:${NEW}`);
  expect(app.value(`id:endpoint-input:${custom.id}`)).toBe(NEW);
  expect(custom.base_url).toBe(OLD);
  await app.press(`id:endpoint-save:${custom.id}`);
  await app.waitFor(`id:endpoint-current:${custom.id} >> text:${NEW}`);
  await app.waitFor("text:Endpoint changed. Models listed again: +1 new, 1 no longer listed.");
  expect(app.shows(`id:endpoint-input:${custom.id}`)).toBe(false);
  expect(patches(f, custom.id)).toEqual([{ method: "PATCH", path: `/v1/providers/${custom.id}`, body: { base_url: NEW, expected_endpoint_revision: "0" }, user: "alice" }]);
  expect(f.state.endpointProbes).toEqual([NEW]);
  expect(f.posts().filter((p) => p.path.endsWith("/model-list/refresh"))).toHaveLength(1);
  expect(custom.base_url).toBe(NEW);
  expect(custom.model).toBe("old-fixture"); // refreshing never silently changes the saved default
});

test("the editor refuses unknown URLs and the server refuses approval removed after the editor opened", async () => {
  const { f, custom } = fixture();
  const app = await signIn(f, "alice");
  await edit(app, custom.id);
  await app.clear(`id:endpoint-input:${custom.id}`);
  await app.type(`id:endpoint-input:${custom.id}`, OTHER);
  await app.press(`id:endpoint-save:${custom.id}`);
  await app.waitFor(`id:endpoint-error:${custom.id}`);
  expect(patches(f, custom.id)).toHaveLength(0);
  await app.press(`id:endpoint-section:${custom.id} >> label:${NEW}`);
  f.state.allowedBaseUrls.pop(); // the server's list was changed after the form opened
  await app.press(`id:endpoint-save:${custom.id}`);
  await app.waitUntil(() => patches(f, custom.id).length === 1);
  await app.waitFor(`id:endpoint-error:${custom.id}`);
  expect(custom.base_url).toBe(OLD);
  expect(f.state.endpointProbes).toHaveLength(0);
  expect(app.shows(`id:endpoint-current:${custom.id} >> text:${OLD}`)).toBe(true);
});

test("a failed endpoint test leaves the old URL and doesn't refresh models; the footer can retry", async () => {
  const { f, custom } = fixture();
  f.state.endpointRefusals.set(NEW, { status: 502, error: "the new endpoint failed its model listing test" });
  const app = await signIn(f, "alice");
  await edit(app, custom.id);
  await app.press(`id:endpoint-section:${custom.id} >> label:${NEW}`);
  await app.press(`id:endpoint-save:${custom.id}`);
  await app.waitFor("id:endpoint-feedback >> text:/The connection still uses the old one/");
  expect(app.shows(`id:endpoint-current:${custom.id} >> text:${OLD}`)).toBe(true);
  expect(app.shows(`id:endpoint-input:${custom.id}`)).toBe(true);
  expect(f.posts().filter((p) => p.path.endsWith("/model-list/refresh"))).toHaveLength(0);
  expect(custom.base_url).toBe(OLD);
  f.state.endpointRefusals.delete(NEW);
  await app.press("id:endpoint-feedback >> label:Try again");
  await app.waitFor("id:endpoint-feedback >> text:/Endpoint changed/");
  expect(custom.base_url).toBe(NEW);
});

test("Metro's full probe queue refuses the write without testing and permits direct retry", async () => {
  const { f, custom } = fixture();
  f.state.endpointRefusals.set(NEW, { status: 503, code: "endpoint_probe_busy", error: "Please try again later." });
  const app = await signIn(f, "alice");
  await edit(app, custom.id);
  await app.press(`id:endpoint-section:${custom.id} >> label:${NEW}`);
  await app.press(`id:endpoint-save:${custom.id}`);
  await app.waitFor("id:endpoint-feedback >> text:/No change was saved/");
  expect(app.shows("id:endpoint-feedback >> label:Reload connection")).toBe(false);
  expect(custom.base_url).toBe(OLD);
  expect(f.state.endpointProbes).toHaveLength(0);
  expect(patches(f, custom.id)).toHaveLength(1);

  f.state.endpointRefusals.delete(NEW);
  await app.press("id:endpoint-feedback >> label:Try again");
  await app.waitFor("id:endpoint-feedback >> text:/Endpoint changed/");
  expect(f.state.endpointProbes).toEqual([NEW]);
  expect(patches(f, custom.id)).toHaveLength(2);
  expect(custom.base_url).toBe(NEW);
});

for (const [label, refusal] of [
  ["an uncoded 503", { status: 503, error: "too many endpoint tests are running; try again shortly" }],
  ["a malformed legacy probe response", { status: 502, error: "the new endpoint failed its " }],
] as const) {
  test(`${label} is not proof the endpoint write was refused`, async () => {
    const { f, custom } = fixture();
    f.state.endpointRefusals.set(NEW, refusal);
    const app = await signIn(f, "alice");
    await edit(app, custom.id);
    await app.press(`id:endpoint-section:${custom.id} >> label:${NEW}`);
    await app.press(`id:endpoint-save:${custom.id}`);
    await app.waitFor("id:endpoint-feedback >> label:Reload connection");
    expect(app.shows("id:endpoint-feedback >> label:Try again")).toBe(false);
    await app.press("id:endpoint-feedback >> label:Reload connection");
    await app.waitFor("id:endpoint-feedback >> text:/first test may still be running/");
    expect(app.enabled(`id:endpoint-save:${custom.id}`)).toBe(false);
    expect(patches(f, custom.id)).toHaveLength(1);
  });
}

for (const live of [OLD, NEW]) {
  test(`a concurrent edit requires reload and a fresh choice even when the current URL is ${live === OLD ? "unchanged" : "the one proposed"}`, async () => {
    const { f, custom } = fixture();
    f.state.endpointConcurrentEdits.set(NEW, { name: "Another editor renamed it", baseUrl: live });
    const app = await signIn(f, "alice");
    await edit(app, custom.id);
    await app.press(`id:endpoint-section:${custom.id} >> label:${NEW}`);
    await app.press(`id:endpoint-save:${custom.id}`);
    await app.waitFor("id:endpoint-feedback >> text:/Your change was not saved/");
    expect(app.shows("id:endpoint-feedback >> label:Try again")).toBe(false);
    expect(app.enabled(`id:endpoint-save:${custom.id}`)).toBe(false);
    expect(app.shows("text:Another editor renamed it")).toBe(false);
    const failedBeforeReload = f.state.log.filter((r) => r.method === "GET" && r.path === "/v1/providers").length;
    await app.press("id:endpoint-feedback >> label:Reload connection");
    await app.waitFor("id:endpoint-feedback >> text:/Connection reloaded.*Review the current endpoint/");
    expect(f.state.log.filter((r) => r.method === "GET" && r.path === "/v1/providers")).toHaveLength(failedBeforeReload + 1);
    await app.waitFor(`id:endpoint-current:${custom.id} >> text:${live}`);
    expect(app.shows("text:Another editor renamed it")).toBe(true);
    expect(app.shows(`id:endpoint-input:${custom.id}`)).toBe(false);
    expect(patches(f, custom.id)).toHaveLength(1);

    await app.press(`id:endpoint-change:${custom.id}`);
    expect(app.value(`id:endpoint-input:${custom.id}`)).toBe(live);
    expect(app.enabled(`id:endpoint-save:${custom.id}`)).toBe(false);
    const next = live === NEW ? OLD : NEW;
    await app.press(`id:endpoint-section:${custom.id} >> label:${next}`);
    await app.press(`id:endpoint-save:${custom.id}`);
    await app.waitFor("id:endpoint-feedback >> text:/Endpoint changed/");
    expect(patches(f, custom.id).map((p) => p.body.base_url)).toEqual([NEW, next]);
  });
}

test("an endpoint that doesn't list models saves without claiming a refresh", async () => {
  const { f } = fixture();
  const embeddings = f.addConnection("alice", "custom-embeddings", "Embeddings", "embed-fixture");
  embeddings.base_url = OLD;
  const app = await signIn(f, "alice");
  await edit(app, embeddings.id);
  await app.press(`id:endpoint-section:${embeddings.id} >> label:${NEW}`);
  await app.press(`id:endpoint-save:${embeddings.id}`);
  await app.waitFor("id:endpoint-feedback >> text:Endpoint changed.");
  expect(embeddings.base_url).toBe(NEW);
  expect(f.state.endpointProbes).toEqual([NEW]);
  expect(f.posts().filter((p) => p.path.endsWith("/model-list/refresh"))).toHaveLength(0);
});

test("a failed refresh is reported separately after a successful endpoint change", async () => {
  const { f, custom } = fixture();
  f.state.listings.delete("custom");
  const app = await signIn(f, "alice");
  await edit(app, custom.id);
  await app.press(`id:endpoint-section:${custom.id} >> label:${NEW}`);
  await app.press(`id:endpoint-save:${custom.id}`);
  await app.waitFor("id:endpoint-feedback >> text:/Endpoint changed. Metro couldn't refresh the model list/");
  expect(custom.base_url).toBe(NEW);
  expect(app.shows(`id:endpoint-current:${custom.id} >> text:${NEW}`)).toBe(true);
});

for (const ack of ["lost", "malformed"] as const) {
  test(`a ${ack} endpoint acknowledgement requires an authoritative reload; a committed change isn't sent again`, async () => {
    const { f, custom } = fixture();
    f.state.endpointAck.set(NEW, ack);
    const app = await open("../../App.tsx", { modules: launched(), network: f.network as any, width: 1200, height: 900, platform: "linux", clock: {} });
    await signIn(f, "alice", app);
    await edit(app, custom.id);
    await app.press(`id:endpoint-section:${custom.id} >> label:${NEW}`);
    await app.press(`id:endpoint-save:${custom.id}`);
    await app.waitFor("id:endpoint-feedback >> label:Reload connection");
    expect(custom.base_url).toBe(NEW);
    expect(app.shows(`id:endpoint-current:${custom.id} >> text:${OLD}`)).toBe(true);
    expect(app.shows("text:Last known URL")).toBe(true);
    expect(app.enabled(`id:endpoint-save:${custom.id}`)).toBe(false);
    expect(app.shows("id:endpoint-feedback >> label:Try again")).toBe(false);

    const late = f.answerLate("/v1/providers", TOKENS.alice!, 2000);
    await app.press("id:endpoint-feedback >> label:Reload connection");
    await app.waitUntil(() => late.arrived() === 1);
    expect(app.enabled(`id:endpoint-save:${custom.id}`)).toBe(false);
    expect(app.shows(`id:endpoint-current:${custom.id} >> text:${OLD}`)).toBe(true);
    await app.clock!.runFor(2000);
    await app.waitFor(`id:endpoint-current:${custom.id} >> text:${NEW}`);
    await app.waitFor("id:endpoint-feedback >> text:/confirmed the new endpoint was saved/");
    expect(app.shows(`id:endpoint-input:${custom.id}`)).toBe(false);
    expect(patches(f, custom.id)).toHaveLength(1);
  });
}

test("an unchanged reload after a proxy 504 cannot requeue a held probe, even after reopening", async () => {
  const { f, custom } = fixture();
  // An intermediary's generic 504 is not proof the server refused its probe.
  f.state.heldEndpointProbes.add(NEW);
  const app = await signIn(f, "alice");
  await edit(app, custom.id);
  await app.press(`id:endpoint-section:${custom.id} >> label:${NEW}`);
  await app.press(`id:endpoint-save:${custom.id}`);
  await app.waitFor("id:endpoint-feedback >> label:Reload connection");
  expect(custom.base_url).toBe(OLD);
  expect(f.state.finishEndpointProbes.has(NEW)).toBe(true);
  await app.press("id:connection-back");
  await app.press(`id:connection:${custom.id}:open`);
  await app.waitFor("id:endpoint-feedback >> label:Reload connection");
  expect(app.enabled(`id:endpoint-save:${custom.id}`)).toBe(false);
  f.state.providersReadStatus = 503;
  await app.press("id:endpoint-feedback >> label:Reload connection");
  await app.waitFor("id:endpoint-feedback >> text:/reload didn't confirm/");
  expect(app.enabled(`id:endpoint-save:${custom.id}`)).toBe(false);
  expect(patches(f, custom.id)).toHaveLength(1);
  f.state.providersReadStatus = 200;
  await app.press("id:endpoint-feedback >> label:Reload connection");
  await app.waitFor("id:endpoint-feedback >> text:/first test may still be running/");
  expect(app.enabled(`id:endpoint-save:${custom.id}`)).toBe(false);
  expect(app.shows("id:endpoint-feedback >> label:Try again")).toBe(false);
  expect(patches(f, custom.id)).toHaveLength(1);
  await app.press("id:settings-close");
  await app.press("id:nav-settings");
  await app.press(`id:connection:${custom.id}:open`);
  await app.waitFor("id:endpoint-feedback >> label:Reload connection");
  expect(app.enabled(`id:endpoint-save:${custom.id}`)).toBe(false);
  expect(f.state.endpointProbes).toEqual([NEW]);

  // Finish the original server probe only after both reads observed OLD.
  f.state.finishEndpointProbes.get(NEW)!();
  await app.press("id:endpoint-feedback >> label:Reload connection");
  await app.waitFor("id:endpoint-feedback >> text:/confirmed the new endpoint was saved/");
  await app.waitFor(`id:endpoint-current:${custom.id} >> text:${NEW}`);
  expect(app.shows(`id:endpoint-input:${custom.id}`)).toBe(false);
  expect(patches(f, custom.id)).toHaveLength(1);
});

test("only owners see endpoint details; built-in and device connections don't show a server endpoint editor", async () => {
  const { f, custom } = fixture();
  const builtIn = f.addConnection("bob", "openai", "Bob model", "gpt-fixture-1");
  const device = f.addConnection("bob", "device-openai-compatible", "Bob computer", "qwen");
  const app = await signIn(f, "bob");
  await app.waitFor(`id:connection:${builtIn.id}`);
  expect(app.shows(`id:connection:${custom.id}`)).toBe(false);
  for (const id of [builtIn.id, device.id]) {
    await app.press(`id:connection:${id}:open`);
    await app.waitFor(`id:connection-details:${id}`);
    expect(app.shows(`id:endpoint-section:${id}`)).toBe(false);
    await app.press("id:connection-back");
  }
  expect(patches(f, custom.id)).toHaveLength(0);
});

test("an expired server token during endpoint save signs out without committing or refreshing", async () => {
  const { f, custom } = fixture();
  f.state.endpointRefusals.set(NEW, { status: 401, error: "invalid token" });
  const app = await signIn(f, "alice");
  await edit(app, custom.id);
  await app.press(`id:endpoint-section:${custom.id} >> label:${NEW}`);
  await app.press(`id:endpoint-save:${custom.id}`);
  await app.waitFor("id:connect-screen");
  expect(custom.base_url).toBe(OLD);
  expect(f.posts().filter((p) => p.path.endsWith("/model-list/refresh"))).toHaveLength(0);
});

test("an old session's delayed endpoint response cannot refresh models or appear in another session", async () => {
  const { f, custom } = fixture();
  const b = f.addConnection("bob", "custom", "Bob endpoint", "bob-fixture");
  b.base_url = OLD;
  const late = f.answerLate(`/v1/providers/${custom.id}`, TOKENS.alice!, 3000);
  const app = await open("../../App.tsx", { modules: launched(), network: f.network as any, width: 1200, height: 900, platform: "linux", clock: {} });
  await signIn(f, "alice", app);
  await edit(app, custom.id);
  await app.press(`id:endpoint-section:${custom.id} >> label:${NEW}`);
  await app.press(`id:endpoint-save:${custom.id}`);
  await app.waitUntil(() => late.arrived() === 1);
  expect(app.shows(`id:endpoint-current:${custom.id} >> text:${OLD}`)).toBe(true);
  await app.press("id:settings-close");
  await app.press("id:account-menu");
  await app.press("id:disconnect");
  await app.waitFor("id:connect-screen");
  await signIn(f, "bob", app);
  await app.waitFor(`id:connection:${b.id}:open`);
  await app.press(`id:connection:${b.id}:open`);
  await app.waitFor(`id:endpoint-current:${b.id} >> text:${OLD}`);
  await app.clock!.runFor(3000);
  expect(app.shows("id:endpoint-feedback")).toBe(false);
  expect(f.posts().filter((p) => p.path.endsWith("/model-list/refresh"))).toHaveLength(0);
});

test("a PATCH held before Metro reads the row cannot overwrite a new same-account session's edit", async () => {
  const { f, custom } = fixture();
  f.state.allowedBaseUrls.push(OTHER);
  f.state.heldEndpointRequests.add(NEW);
  const app = await signIn(f, "alice");
  await edit(app, custom.id);
  await app.press(`id:endpoint-section:${custom.id} >> label:${NEW}`);
  await app.press(`id:endpoint-save:${custom.id}`);
  await app.waitFor("id:endpoint-feedback >> label:Reload connection");
  expect(patches(f, custom.id)).toHaveLength(0);
  await app.press("id:settings-close");
  await app.press("id:account-menu");
  await app.press("id:disconnect");
  await app.waitFor("id:connect-screen");
  await signIn(f, "alice", app);
  await edit(app, custom.id);
  await app.press(`id:endpoint-section:${custom.id} >> label:${OTHER}`);
  await app.press(`id:endpoint-save:${custom.id}`);
  await app.waitFor(`id:endpoint-current:${custom.id} >> text:${OTHER}`);
  expect(f.state.forwardEndpointRequests.get(NEW)!()).toMatchObject({ status: 409, body: { code: "endpoint_conflict" } });
  expect(patches(f, custom.id).map((p) => p.body)).toEqual([
    { base_url: OTHER, expected_endpoint_revision: "0" },
    { base_url: NEW, expected_endpoint_revision: "0" },
  ]);
  expect(f.state.endpointProbes).toEqual([OTHER]);
  expect(custom.base_url).toBe(OTHER);
});

test("a server that does not advertise endpoint revisions cannot be edited unguarded", async () => {
  const { f, custom } = fixture();
  f.state.exposesEndpointRevision = false;
  const app = await signIn(f, "alice");
  await app.press(`id:connection:${custom.id}:open`);
  await app.waitFor("text:This server doesn't support guarded endpoint changes.");
  expect(app.enabled(`id:endpoint-change:${custom.id}`)).toBe(false);
  expect(patches(f, custom.id)).toHaveLength(0);
});
