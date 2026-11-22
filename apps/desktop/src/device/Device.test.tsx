// In-app device setup against the fake server (test/fakeServer.ts) and a fake
// desktop bridge (./test/fakeBridge.ts). The app gets the launcher's two
// variables through nsh/test's `modules` stand-in for `process`, exactly as
// the packaged UI reads them. Fake tokens and keys only.

import { expect, open, test } from "nsh/test";
import type { App } from "nsh/test";
import { BASE, fakeServer, header, TOKENS, world } from "../../test/fakeServer";
import { BRIDGE, BRIDGE_DEVICE_SECRET, BRIDGE_TOKEN, bridgeEnv, fakeBridge, LOCAL_MODELS } from "./test/fakeBridge";

type F = ReturnType<typeof fakeServer>;
type B = ReturnType<typeof fakeBridge>;

const VENDOR_KEY = "sk-vendor-fixture-not-real-123456";

async function openPackaged(f: F, b: B | null, env: object | null = bridgeEnv()) {
  const network = { ...(f.network as any), ...(b ? (b.network as any) : {}) };
  return open("../../App.tsx", { network, platform: "linux", width: 1280, height: 900, ...(env ? { modules: env as any } : {}) });
}

async function signIn(app: App, user: string) {
  await app.clear("id:server-url");
  await app.type("id:server-url", BASE);
  await app.type("id:access-token", TOKENS[user]!);
  await app.press("id:connect");
  await app.waitFor("id:nav-settings");
}

async function openProviders(app: App) {
  if (!app.shows("id:nav-settings:providers")) await app.press("id:nav-settings");
  await app.press("id:nav-settings:providers");
  await app.waitFor("id:providers-settings");
}

const VENDOR: Record<string, string> = { "device-openai-compatible": "custom", "device-openai-oauth": "openai", "device-claude-code": "anthropic" };

/** Add a connection: the provider from the drop-down, then the on-device way to connect. */
async function createDevice(app: App, providerId: string, name: string) {
  const vendor = VENDOR[providerId]!;
  await app.press("id:provider-picker");
  await app.press(`id:provider-picker:${vendor}`);
  await app.waitFor(`id:provider-ways:${vendor}`);
  await app.press(`id:provider:${providerId}`);
  await app.waitFor("id:device-form");
  await app.clear("id:provider-name");
  await app.type("id:provider-name", name);
  await app.press("id:provider-create-device");
}

const text = (app: App) => app.texts().join(" | ");
const secrets = [BRIDGE_TOKEN, BRIDGE_DEVICE_SECRET, VENDOR_KEY];
function expectNoSecrets(app: App) {
  const shown = text(app);
  const stored = JSON.stringify(app.storage());
  for (const s of secrets) {
    expect(shown).not.toContain(s);
    expect(stored).not.toContain(s);
  }
  expect(app.clipboard() ?? "").not.toContain(BRIDGE_TOKEN);
}

test("packaged: sign-in binds the bridge with the session; it's the only place the bridge bearer goes", async () => {
  const f = world();
  const b = fakeBridge(f);
  const app = await openPackaged(f, b);
  // Before any sign-in, the app only asks the helper about itself.
  await app.waitUntil(() => b.calls("/v1/status", "GET").length >= 1);
  await app.wait(300);
  expect(b.state.log.every((l) => l.method === "GET" && l.path === "/v1/status")).toBe(true);
  await signIn(app, "bob");
  await app.waitUntil(() => b.calls("/v1/session", "POST").length === 1);
  const bind = b.calls("/v1/session", "POST")[0]!;
  expect(bind.body).toEqual({ server_url: BASE, session_token: TOKENS.bob });
  expect(header(bind as any, "authorization")).toBe(`Bearer ${BRIDGE_TOKEN}`);
  expect(header(bind as any, "origin")).toBe(undefined);
  // The bridge bearer never goes to the Metro server.
  for (const r of app.requests()) {
    if (!r.url.startsWith(BRIDGE)) expect(JSON.stringify(r)).not.toContain(BRIDGE_TOKEN);
  }
  // Signing out stops this computer's workers.
  await app.press("id:account-menu"); // Sign out is in the account menu
  await app.press("id:disconnect");
  await app.waitUntil(() => b.calls("/v1/session", "DELETE").length === 1);
  expectNoSecrets(app);
});

test("packaged: an endpoint connection is set up in-app with consent; the UI never asks the server for a device token", async () => {
  const f = world();
  const b = fakeBridge(f);
  const app = await openPackaged(f, b);
  await signIn(app, "bob");
  await openProviders(app);
  await createDevice(app, "device-openai-compatible", "Home GPU");
  await app.waitFor("id:device-consent");
  const c = [...f.state.connections.values()].find((x) => x.name === "Home GPU")!;
  // Nothing was issued: no token request from the UI, no token panel, no terminal steps.
  expect(f.posts().map((p) => p.path)).not.toContain(`/v1/providers/${c.id}/device-token`);
  expect(app.shows("id:runner-token-value")).toBe(false);
  expect(text(app)).not.toContain("metro-connector setup");
  expect(text(app)).not.toContain("cargo");
  expect(text(app)).toContain("Set up “Home GPU” on this computer");

  // Pairing needs the endpoint and explicit agreement.
  expect(app.enabled("id:device-pair")).toBe(false);
  await app.type("id:device-endpoint-url", "http://127.0.0.1:11434/v1");
  expect(app.enabled("id:device-pair")).toBe(false);
  expect(b.calls(`/v1/connections/${c.id}/pair`)).toHaveLength(0);
  await app.press("id:device-consent-agree");
  await app.press("id:device-pair");
  await app.waitFor("id:device-credentials");
  expect(b.calls(`/v1/connections/${c.id}/pair`)[0]!.body).toEqual({ kind: "device-openai-compatible", base_url: "http://127.0.0.1:11434/v1" });
  expect(f.state.connections.get(c.id)!.deviceToken).toBe(BRIDGE_DEVICE_SECRET);

  // The vendor key: masked, sent to the bridge only, then dropped.
  await app.type("id:device-api-key", VENDOR_KEY);
  await app.press("id:device-save-key");
  await app.waitFor("id:device-models");
  expect(b.state.keys.get(c.id)).toBe(VENDOR_KEY);
  for (const r of app.requests()) if (!r.url.startsWith(BRIDGE)) expect(JSON.stringify(r)).not.toContain(VENDOR_KEY);

  // Models listed from this computer; approve one.
  await app.waitFor(`id:device-model:${LOCAL_MODELS[1]}`);
  expect(app.enabled("id:device-models-save")).toBe(false);
  await app.press(`id:device-model:${LOCAL_MODELS[1]}`);
  await app.press("id:device-models-save");
  await app.waitFor("id:device-ready");
  expect(b.calls(`/v1/connections/${c.id}/models`, "PUT")[0]!.body).toEqual({ models: [LOCAL_MODELS[1]] });
  await app.waitFor(`id:device-local-state:${c.id} >> text:Serving from here`);

  // The server sees it online; a test goes through.
  await app.press("id:device-test");
  await app.waitFor(`id:listing-ok:${c.id}`);
  expectNoSecrets(app);

  // Done: back to the list, where its row says this computer serves it.
  await app.press("id:device-setup-done");
  await app.waitFor(`id:connection:${c.id}`);
  await app.waitUntil(() => app.texts().some((s) => s.startsWith("This computer serves it")));
  expectNoSecrets(app);
});

test("packaged: ChatGPT sign-in happens in the browser and the page follows it", async () => {
  const f = world();
  const b = fakeBridge(f);
  const app = await openPackaged(f, b);
  await signIn(app, "bob");
  await openProviders(app);
  await createDevice(app, "device-openai-oauth", "Work ChatGPT");
  await app.waitFor("id:device-consent");
  const c = [...f.state.connections.values()].find((x) => x.name === "Work ChatGPT")!;
  expect(app.shows("id:device-endpoint-url")).toBe(false);
  await app.press("id:device-consent-agree");
  await app.press("id:device-pair");
  await app.waitFor("id:device-sign-in");
  expect(app.shows("id:device-api-key")).toBe(false); // no code or token is ever pasted
  await app.press("id:device-sign-in");
  await app.waitFor("id:device-signing-in");
  expect(b.calls(`/v1/connections/${c.id}/credentials`, "POST")[0]!.body).toEqual({ open_browser: true });
  b.finishLogin(c.id);
  await app.waitFor("id:device-models"); // polled, no press needed
  await app.waitFor(`id:device-model:${LOCAL_MODELS[0]}`);
  await app.press(`id:device-model:${LOCAL_MODELS[0]}`);
  await app.press("id:device-models-save");
  await app.waitFor("id:device-ready");
  expectNoSecrets(app);
});

test("packaged: re-pair, sign out and forget ask first; forget revokes", async () => {
  const f = world();
  const b = fakeBridge(f);
  const c = f.addConnection("bob", "device-openai-compatible", "Lab box", null);
  const app = await openPackaged(f, b);
  await signIn(app, "bob");
  await openProviders(app);
  await app.waitFor(`id:connection:${c.id}`);
  // The row's token action opens in-app setup in the packaged app; it issues nothing.
  await app.press(`id:connection-manage:${c.id}`);
  await app.press(`id:connection-token:${c.id}`);
  await app.waitFor("id:device-consent");
  expect(f.posts().map((p) => p.path)).not.toContain(`/v1/providers/${c.id}/device-token`);
  await app.type("id:device-endpoint-url", "http://127.0.0.1:8000/v1");
  await app.press("id:device-consent-agree");
  await app.press("id:device-pair");
  await app.waitFor("id:device-no-key");
  await app.press("id:device-no-key");
  await app.press("id:device-save-key");
  await app.waitFor(`id:device-model:${LOCAL_MODELS[0]}`);
  await app.press(`id:device-model:${LOCAL_MODELS[0]}`);
  await app.press("id:device-models-save");
  await app.waitFor("id:device-ready");
  expect(b.state.keys.get(c.id)).toBe(null);

  await app.press("id:device-rotate");
  await app.waitFor("id:device-confirm-rotate");
  expect(b.calls(`/v1/connections/${c.id}/rotate`)).toHaveLength(0);
  await app.press("id:device-confirm-rotate-now");
  await app.waitFor("id:device-setup-notice");
  expect(f.state.connections.get(c.id)!.deviceToken).toBe(`${BRIDGE_DEVICE_SECRET}_rotated`);

  await app.press("id:device-signout");
  await app.press("id:device-confirm-signout-now");
  await app.waitFor("id:device-credentials");
  expect(b.calls(`/v1/connections/${c.id}/credentials`, "DELETE")).toHaveLength(1);

  // Forget is offered once set up again; simulate it by finishing setup.
  await app.press("id:device-no-key");
  await app.press("id:device-save-key");
  await app.waitFor("id:device-ready");
  await app.press("id:device-forget");
  await app.waitFor("id:device-confirm-forget");
  await app.press("id:device-confirm-forget-now");
  await app.waitFor("id:device-consent");
  expect(b.calls(`/v1/connections/${c.id}/forget`)[0]!.body).toEqual({ revoke: true });
  expect(f.state.connections.get(c.id)!.deviceToken).toBe(null);
  expectNoSecrets(app);
});

test("packaged: a second Metro window says so instead of pairing", async () => {
  const f = world();
  const b = fakeBridge(f, { instance: "secondary" });
  const app = await openPackaged(f, b);
  await signIn(app, "bob");
  await openProviders(app);
  await createDevice(app, "device-openai-compatible", "Second");
  await app.waitFor("id:device-helper-status");
  expect(text(app)).toContain("Another Metro window on this computer already runs your device connections");
  expect(app.shows("id:device-pair")).toBe(false);
});

test("packaged: a bridge that is down never falls back to manual steps or a token", async () => {
  const f = world();
  const app = await openPackaged(f, null); // the variables are set, nothing answers
  await signIn(app, "bob");
  await openProviders(app);
  await createDevice(app, "device-openai-compatible", "Down");
  await app.waitFor("id:device-helper-status");
  expect(text(app)).toContain("didn't answer");
  expect(text(app)).not.toContain("metro-connector setup");
  expect(app.shows("id:runner-token-value")).toBe(false);
});

test("unpackaged or malformed variables: no bridge requests, the manual path for headless computers", async () => {
  for (const env of [null, bridgeEnv("http://192.168.1.5:47123"), bridgeEnv(BRIDGE, "short"), bridgeEnv("http://localhost:47123")]) {
    const f = world();
    const b = fakeBridge(f);
    const app = await openPackaged(f, b, env);
    await signIn(app, "bob");
    await openProviders(app);
    await createDevice(app, "device-openai-compatible", "Manual");
    await app.waitFor("id:token-reveal");
    expect(app.shows("id:device-consent")).toBe(false);
    expect(text(app)).toContain("metro-connector setup --profile manual");
    expect(text(app)).not.toContain("cargo install");
    // Explicitly the operator/headless path, never a default install.
    expect(text(app)).toContain("On a headless computer (operators and development builds)");
    expect(text(app)).toContain("assume an operator already installed metro-connector");
    expect(b.state.log).toHaveLength(0);
    for (const r of app.requests()) expect(r.url.startsWith(BASE)).toBe(true);
  }
});

test("packaged: a starting worker turns to serving by itself, with spaced status reads only, while the page is open", async () => {
  const f = world();
  const b = fakeBridge(f);
  b.state.slowStart = true;
  const app = await openPackaged(f, b);
  await signIn(app, "bob");
  await openProviders(app);
  await createDevice(app, "device-openai-compatible", "Slow box");
  await app.waitFor("id:device-consent");
  const c = [...f.state.connections.values()].find((x) => x.name === "Slow box")!;
  await app.type("id:device-endpoint-url", "http://127.0.0.1:8000/v1");
  await app.press("id:device-consent-agree");
  await app.press("id:device-pair");
  await app.waitFor("id:device-no-key");
  await app.press("id:device-no-key");
  await app.press("id:device-save-key");
  await app.waitFor(`id:device-model:${LOCAL_MODELS[0]}`);
  await app.press(`id:device-model:${LOCAL_MODELS[0]}`);
  await app.press("id:device-models-save");
  await app.waitFor(`id:device-local-state:${c.id} >> text:Starting…`);
  const listed = b.calls(`/v1/connections/${c.id}/available-models`).length;

  // The worker reaches the server; nothing is pressed or reopened.
  b.serve(c.id);
  await app.waitFor(`id:device-local-state:${c.id} >> text:Serving from here`, { timeout: 5000 });
  // Only status reads followed it: no vendor or model call, and never two at once.
  expect(b.calls(`/v1/connections/${c.id}/available-models`)).toHaveLength(listed);
  // The server's view followed too: the row says this computer serves it.
  await app.press("id:device-setup-done");
  await app.waitUntil(() => app.texts().some((s) => s.startsWith("This computer serves it")));
  expectNoSecrets(app);
});

test("packaged: following a starting worker stops when the page closes or the user signs out", async () => {
  const f = world();
  const b = fakeBridge(f);
  b.state.slowStart = true;
  const c = f.addConnection("bob", "device-openai-compatible", "Never starts", null);
  const app = await openPackaged(f, b);
  await signIn(app, "bob");
  await openProviders(app);
  await app.press(`id:connection-manage:${c.id}`);
  await app.press(`id:connection-token:${c.id}`);
  await app.type("id:device-endpoint-url", "http://127.0.0.1:8000/v1");
  await app.press("id:device-consent-agree");
  await app.press("id:device-pair");
  await app.waitFor("id:device-no-key");
  await app.press("id:device-no-key");
  await app.press("id:device-save-key");
  await app.waitFor(`id:device-model:${LOCAL_MODELS[0]}`);
  await app.press(`id:device-model:${LOCAL_MODELS[0]}`);
  await app.press("id:device-models-save");
  await app.waitFor(`id:device-local-state:${c.id} >> text:Starting…`);
  const reads = () => b.calls("/v1/status", "GET").length;
  const start = reads();
  await app.wait(3400);
  expect(reads() - start).toBeGreaterThanOrEqual(1);
  expect(reads() - start).toBeLessThanOrEqual(3); // spaced, about one per 1.5 s

  // Leaving Model providers ends it.
  await app.back(); // Escape closes the Settings modal
  await app.waitFor("id:providers-settings", { state: "gone" });
  await app.wait(300);
  const left = reads();
  await app.wait(3200);
  expect(reads()).toBe(left);

  // Back on the page it resumes. The session then ends while the page is still
  // open and following the worker: the fixture stops accepting Bob's token, so
  // the app's next ordinary request gets 401 and the session ends. Following
  // stops for good.
  await openProviders(app);
  await app.wait(1700);
  expect(reads()).toBeGreaterThan(left);
  expect(app.shows("id:providers-settings")).toBe(true); // Settings stays open
  const token = TOKENS.bob!;
  TOKENS.bob = "tok-bob-expired";
  try {
    await app.waitFor("id:connect", { timeout: 8000 });
    expect(app.shows("text:/rejected the access token/")).toBe(true); // ended by the 401, not by a press
    await app.wait(300);
    const out = reads();
    await app.wait(3200);
    expect(reads()).toBe(out);
  } finally {
    TOKENS.bob = token;
  }
});
