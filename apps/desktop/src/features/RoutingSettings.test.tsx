// Model routing, connection capabilities and sharing, end to end in the app:
// the real NSH engine against the in-memory fake server. No network.

import { expect, open, test } from "nsh/test";
import type { App } from "nsh/test";
import { BASE, TOKENS, world } from "../../test/fakeServer";

type F = ReturnType<typeof world>;

async function signIn(f: F, user: string, existing?: App) {
  const app = existing ?? (await open("../../App.tsx", { network: f.network as any, platform: "linux", width: 1200, height: 900 }));
  await app.clear("id:server-url");
  await app.type("id:server-url", BASE);
  await app.type("id:access-token", TOKENS[user]!);
  await app.press("id:connect");
  await app.waitFor("id:nav-chat");
  return app;
}

async function settings(app: App, section: "providers" | "routing" | "runners") {
  if (!app.shows(`id:nav-settings:${section}`)) await app.press("id:nav-settings");
  await app.press(`id:nav-settings:${section}`);
  await app.waitFor(`id:${section}-settings`);
}

const text = (app: App) => app.texts().join(" | ");
const puts = (f: F, path: string) => f.posts().filter((p) => p.method === "PUT" && p.path === path);

/** Sign out: close Settings, then the account menu's Sign out. */
async function signOut(app: App) {
  if (app.shows("id:settings-close")) await app.press("id:settings-close");
  await app.press("id:account-menu");
  await app.press("id:disconnect");
}

/** Picks `label` in the picker `id`. */
async function pick(app: App, id: string, label: string) {
  await app.press(`id:${id}`);
  await app.press(`text:${label}`);
}

/** Alice's own chat, System One and embeddings connections, Bob's chat connection shared with Acme, and one he keeps private. */
function routingWorld() {
  const f = world();
  const chat = f.addConnection("alice", "openai", "Alice OpenAI", "gpt-fixture-2");
  const s1 = f.addConnection("alice", "systemone", "Alice System One", "jev-latest");
  const emb = f.addConnection("alice", "custom-embeddings", "Home embeddings", "nomic-embed-text");
  const shared = f.addConnection("bob", "device-openai-compatible", "Bob GPU", "qwen-fixture", { visibility: "spaces", space_ids: ["acme"] });
  const hidden = f.addConnection("bob", "openai", "Bob private", "gpt-fixture-1");
  return { f, chat, s1, emb, shared, hidden };
}

test("Model routing is off by default and says nothing is sent", async () => {
  const { f } = routingWorld();
  const app = await signIn(f, "alice");
  await settings(app, "routing");
  await app.waitFor("id:routing-dataflow");
  // Confidence is a whole percentage.
  expect(app.value("id:routing-min-route")).toBe("65");
  expect(app.value("id:routing-min-directed")).toBe("85");
  expect(text(app)).toContain("Off: nothing is sent to pick a model.");
  expect(text(app)).toContain("Off: your unaddressed messages are ordinary conversation and are sent to no model.");
  expect(text(app)).toContain("Messages that mention only people, and other members' messages for routing or classification.");
  expect(app.enabled("id:routing-save")).toBe(false); // nothing changed
  // Section headers are one sentence; the rest is under Learn more, closed until asked.
  // Each opt-in says, beside its toggle, that it's off by default and what it sends where.
  expect(app.shows("text:Off by default. When on, each agent request you start is sent to System One to pick a model.")).toBe(true);
  expect(
    app.shows("text:Off by default. When on, each message you write that mentions nobody is sent to System One, in rooms and your Chat. A start it allows runs as you."),
  ).toBe(true);
  expect(app.shows("text:A model you pick for one turn overrides it.")).toBe(false);
  await app.press("id:routing-learn-more:toggle");
  await app.waitFor("text:A model you pick for one turn overrides it.");
  await app.press("id:addressing-learn-more:toggle");
  await app.waitFor("text:A message that mentions a person only (@bob) never starts the agent and is never classified.");
  await app.press("id:classifier-learn-more:toggle");
  await app.waitFor("text:Typed questions: Noul, choice and score.");
  expect(f.posts().some((p) => p.path === "/v1/model-routing")).toBe(false);
});

test("pickers offer only models of their capability that I can use, mine and shared ones; a use case routes to a shared model", async () => {
  const { f, s1, emb, shared } = routingWorld();
  const app = await signIn(f, "alice");
  await settings(app, "routing");
  await app.waitFor("id:routing-classifier");

  // Turning routing on without a classifier is refused before any request.
  await app.press("id:routing-enabled");
  await app.press("id:routing-save");
  await app.waitFor("text:Choose a System One classifier first: routing and classification both need one.");
  expect(puts(f, "/v1/model-routing")).toHaveLength(0);

  // The classifier picker lists System One connections only.
  await app.press("id:routing-classifier");
  expect(app.shows("text:/Alice System One · jev-latest/")).toBe(true);
  expect(app.shows("text:/Alice OpenAI · gpt-fixture-2/")).toBe(false);
  expect(app.shows("text:/Home embeddings/")).toBe(false);
  await app.press("text:/Alice System One · jev-latest/");

  // A use case answered by Bob's shared connection; his private one is never offered.
  await app.press("id:routing-add-case");
  await app.type("id:case-label:0", "Code review");
  await app.type("id:case-description:0", "Reviewing a diff or a pull request");
  const classifierRows = app.count("text:/^Alice System One · /"); // the classifier's own trigger
  await app.press("id:case-model:0");
  expect(app.shows("text:/Bob GPU · qwen-fixture/")).toBe(true);
  expect(app.shows("text:/Bob private/")).toBe(false);
  expect(app.count("text:/^Alice System One · /")).toBe(classifierRows); // not offered: not a chat model
  await app.press("text:/Bob GPU · qwen-fixture/");

  await pick(app, "routing-embedding", "/Home embeddings · nomic-embed-text/");
  await app.clear("id:routing-min-route");
  await app.type("id:routing-min-route", "70");

  // What goes where, from what is chosen, in short rows.
  const fact = (row: string, value: string) => app.shows(`id:dataflow-${row}-facts >> text:${value}`);
  expect(fact("routing", "That message's text, and your use-case names and descriptions")).toBe(true);
  expect(fact("routing", "“Alice System One” (TypeSafe System One)")).toBe(true);
  expect(fact("routing", "The Metro server calls it with your settings")).toBe(true);
  expect(fact("search", "/other members' messages there included/")).toBe(true);
  expect(fact("search", "/“Home embeddings”/")).toBe(true);
  expect(fact("search", "Each query and its top results (at most 10) also go to the classifier")).toBe(true);
  expect(fact("search", "Classifier")).toBe(true);

  await app.press("id:routing-save");
  await app.waitFor("id:routing-notice");
  expect(text(app)).toContain("Saved. Use-case routing is on (1 use case); unaddressed messages stay ordinary conversation.");
  expect(puts(f, "/v1/model-routing").at(-1)?.body).toEqual({
    enabled: true,
    infer_addressing: false,
    classifier_id: s1.id,
    embedding_id: emb.id,
    default_chat_id: null,
    min_directedness: 0.85,
    min_route_probability: 0.7,
    cases: [{ key: "code_review", label: "Code review", description: "Reviewing a diff or a pull request", connection_id: shared.id }],
  });
  expect(app.enabled("id:routing-save")).toBe(false);

  // Coming back shows what was saved.
  await app.press("id:settings-close");
  await settings(app, "routing");
  await app.waitFor("id:case-label:0");
  expect(app.value("id:case-label:0")).toBe("Code review");
  expect(app.value("id:routing-min-route")).toBe("70");
});

test("classification of my unaddressed messages is a separate opt-in and names the endpoint that receives them", async () => {
  const { f, s1 } = routingWorld();
  const app = await signIn(f, "alice");
  await settings(app, "routing");
  await pick(app, "routing-classifier", "/Alice System One · jev-latest/");
  await app.press("id:routing-infer");
  expect(app.shows("id:dataflow-addressing-facts >> text:Each message you write that mentions nobody and isn't sent with Ask agent")).toBe(true);
  expect(app.shows("id:dataflow-addressing-facts >> text:“Alice System One” (TypeSafe System One)")).toBe(true);
  // A percentage below the floor, or not a whole number, can't be saved.
  await app.clear("id:routing-min-directed");
  await app.type("id:routing-min-directed", "2");
  await app.waitFor("id:routing-min-directed-error");
  expect(app.shows("text:Enter a whole number from 50 to 100.")).toBe(true);
  expect(app.enabled("id:routing-save")).toBe(false);
  await app.clear("id:routing-min-directed");
  await app.type("id:routing-min-directed", "0.9");
  expect(app.enabled("id:routing-save")).toBe(false);
  await app.clear("id:routing-min-directed");
  await app.type("id:routing-min-directed", "90");
  expect(app.shows("id:routing-min-directed-error")).toBe(false);
  await app.press("id:routing-save");
  await app.waitFor("id:routing-notice");
  expect(puts(f, "/v1/model-routing").at(-1)?.body).toMatchObject({ enabled: false, infer_addressing: true, classifier_id: s1.id, min_directedness: 0.9, cases: [] });
});

test("a model whose share was revoked is flagged and can't be saved", async () => {
  const { f, s1, shared } = routingWorld();
  f.state.routing.set("alice", {
    enabled: true,
    infer_addressing: false,
    classifier_id: s1.id,
    embedding_id: null,
    default_chat_id: null,
    min_directedness: 0.85,
    min_route_probability: 0.65,
    cases: [{ key: "gpu", label: "Heavy jobs", description: "", connection_id: shared.id }],
  });
  shared.share = { visibility: "personal", space_ids: [] }; // Bob stopped sharing
  const app = await signIn(f, "alice");
  await settings(app, "routing");
  await app.waitFor("id:case-missing:0");
  await app.press("id:routing-save");
  await app.waitFor("text:The model for “Heavy jobs” isn't available to you any more. Choose another one.");
  expect(puts(f, "/v1/model-routing")).toHaveLength(0);
});

test("a server without model routing says so", async () => {
  const { f } = routingWorld();
  f.state.noRouting = true;
  const app = await signIn(f, "alice");
  await settings(app, "routing");
  await app.waitFor("id:routing-unsupported");
});

test("a late routing answer never lands in the next user's session", async () => {
  const { f, s1 } = routingWorld();
  f.state.routing.set("alice", { enabled: false, infer_addressing: true, classifier_id: s1.id, embedding_id: null, default_chat_id: null, min_directedness: 0.55, min_route_probability: 0.6, cases: [] });
  f.state.delays.set("/v1/model-routing", 600);
  const app = await signIn(f, "alice");
  await app.press("id:nav-settings");
  await app.press("id:nav-settings:routing");
  // Sign out (in the account menu, under the Settings modal) while GET /v1/model-routing is on its way.
  await app.press("id:settings-close");
  await app.press("id:account-menu");
  await app.press("id:disconnect");
  f.state.delays.delete("/v1/model-routing");
  await signIn(f, "bob", app);
  await settings(app, "routing");
  await app.waitFor("id:routing-min-directed");
  await app.wait(800);
  expect(app.value("id:routing-min-directed")).toBe("85");
});

// ------------------------------------------------------------------ capabilities and sharing

test("each capability has its own default; a System One connection never becomes the chat default", async () => {
  const { f, chat } = routingWorld();
  const app = await signIn(f, "alice");
  await settings(app, "providers");
  await app.waitFor(`id:connection:${chat.id}`);
  expect(app.shows(`id:connection-default:${chat.id} >> text:Default chat`)).toBe(true);
  // Each capability's default, on one line at the top of the page.
  expect(app.shows(`id:provider-defaults:chat >> text:${chat.name}`)).toBe(true);
  expect(app.shows("id:provider-defaults:system_one >> text:Alice System One")).toBe(true);
  expect(app.shows("id:provider-defaults:embeddings >> text:Home embeddings")).toBe(true);
  const s1b = f.addConnection("alice", "device-systemone", "Laptop System One", "jev-latest");
  await app.press("id:settings-close");
  await settings(app, "providers");
  await app.waitFor(`id:connection:${s1b.id}`);
  expect(app.shows(`id:connection-capability:${s1b.id} >> text:System One`)).toBe(true);
  expect(app.shows(`id:connection-make-default:${s1b.id}`)).toBe(false); // inside Manage
  await app.press(`id:connection-manage:${s1b.id}`);
  await app.press(`id:connection-make-default:${s1b.id}`);
  await app.waitUntil(() => f.state.connections.get(s1b.id)!.is_default);
  await app.waitFor(`id:connection-default:${s1b.id}`);
  expect(app.shows(`id:connection-default:${s1b.id} >> text:Default System One`)).toBe(true);
  expect(f.state.connections.get(chat.id)!.is_default).toBe(true);
  expect(app.shows(`id:connection-default:${chat.id}`)).toBe(true);
});

test("the provider list filters by capability; an embeddings endpoint that can't list models needs a model ID", async () => {
  const { f } = routingWorld();
  const app = await signIn(f, "alice");
  await settings(app, "providers");
  // The capability filter lives in the Add connection picker.
  await app.press("id:provider-picker");
  await app.waitFor("id:provider-capability");
  await app.press("id:provider-capability:embeddings");
  await app.press("id:provider-picker"); // pressing Add connection again keeps the filter
  // Only providers with an embeddings way are offered; OpenAI keeps just that way.
  await app.waitFor("id:provider-picker:custom");
  expect(app.shows("id:provider-picker:openai")).toBe(true);
  expect(app.shows("id:provider-picker:systemone")).toBe(false);
  expect(app.shows("id:provider-picker:anthropic")).toBe(false);
  await app.press("id:provider-picker:custom");
  await app.waitFor("id:provider:custom-embeddings");
  expect(app.shows("id:provider:custom")).toBe(false); // the chat way is filtered out
  expect(app.shows("id:provider:device-embeddings")).toBe(true);
  await app.press("id:provider:custom-embeddings");
  await app.waitFor("id:server-form");
  expect(app.shows("id:provider-save-fetch")).toBe(false);
  await app.press("id:provider-url:http://127.0.0.1:11434/v1");
  const before = f.posts().length;
  await app.press("id:provider-save");
  await app.waitFor("text:This provider can't list its models: enter the model ID it documents.");
  expect(f.posts().length).toBe(before);
  await app.type("id:provider-model", "nomic-embed-text");
  await app.press("id:provider-save");
  await app.waitUntil(() => [...f.state.connections.values()].some((c) => c.name === "Self-hosted embeddings (OpenAI-compatible)" && c.capability === "embeddings"));
});

test("the owner shares a connection with chosen Spaces, others can use it without seeing its key, and the owner revokes it", async () => {
  const { f, chat } = routingWorld();
  const app = await signIn(f, "alice");
  await settings(app, "providers");
  await app.waitFor(`id:connection:${chat.id}`);
  await app.press(`id:connection-manage:${chat.id}`);
  await app.press(`id:connection-sharing:${chat.id}`);
  await app.waitFor(`id:sharing-visibility:${chat.id}`);
  expect(text(app)).toContain("Only you can use it. This is the default.");
  await app.press(`id:sharing-visibility:${chat.id}:spaces`);
  await app.waitFor(`id:sharing-space:${chat.id}:acme`);
  expect(app.shows(`id:sharing-space:${chat.id}:side`)).toBe(true); // only Spaces I'm in
  await app.press(`id:sharing-save:${chat.id}`);
  await app.waitFor("text:Pick at least one Space, or choose Only me.");
  await app.press(`id:sharing-space:${chat.id}:acme`);
  expect(text(app)).toContain("You see their requests and may pay for them");
  expect(text(app)).toContain("As the owner of the endpoint, you can see what they send, and you may pay for their use. They never see its base URL, key, sign-in or device token, and can't manage it.");
  await app.press(`id:sharing-save:${chat.id}`);
  await app.waitFor(`id:sharing-notice:${chat.id}`);
  expect(puts(f, `/v1/providers/${chat.id}/sharing`).at(-1)?.body).toEqual({ visibility: "spaces", space_ids: ["acme"] });
  expect(text(app)).toContain("Members of Acme can now use “Alice OpenAI”.");
  await app.waitFor(`id:connection-shared:${chat.id}`);
  expect(text(app)).not.toContain("sk-fixture-not-real");

  // Bob (in Acme) can pick it, never read it; Carol (in no Space with Alice) can't see it.
  await signOut(app);
  await signIn(f, "bob", app);
  await settings(app, "routing");
  await app.press("id:routing-add-case");
  await app.press("id:case-model:0");
  expect(app.shows("text:/Alice OpenAI · gpt-fixture-2/")).toBe(true);
  expect(text(app)).not.toContain("sk-fixture-not-real");
  await signOut(app);
  await signIn(f, "carol", app);
  await settings(app, "routing");
  await app.press("id:routing-add-case");
  await app.press("id:case-model:0");
  expect(app.shows("text:/Alice OpenAI/")).toBe(false);

  // Alice makes it private again.
  await signOut(app);
  await signIn(f, "alice", app);
  await settings(app, "providers");
  if (!app.shows(`id:connection-sharing:${chat.id}`)) await app.press(`id:connection-manage:${chat.id}`);
  await app.press(`id:connection-sharing:${chat.id}`);
  await app.waitFor(`id:sharing-visibility:${chat.id}`);
  await app.press(`id:sharing-visibility:${chat.id}:personal`);
  await app.press(`id:sharing-save:${chat.id}`);
  await app.waitFor(`id:sharing-notice:${chat.id}`);
  expect(text(app)).toContain("“Alice OpenAI” is private again. Nobody else can use it from now on.");
  expect(f.state.connections.get(chat.id)!.share.visibility).toBe("personal");
  await app.waitFor(`id:connection-shared:${chat.id}`, { state: "gone" });
});

test("a ChatGPT sign-in is shared only as a bring-your-own preset; a device share says it runs on the owner's computer", async () => {
  const f = world();
  const oauth = f.addConnection("alice", "device-openai-oauth", "My ChatGPT", "gpt-sub");
  const gpu = f.addConnection("alice", "device-openai-compatible", "Desk GPU", "qwen-fixture");
  const app = await signIn(f, "alice");
  await settings(app, "providers");
  await app.press(`id:connection-manage:${oauth.id}`);
  await app.press(`id:connection-sharing:${oauth.id}`);
  await app.waitFor(`id:sharing-preset:${oauth.id}`);
  expect(text(app)).toContain("Your ChatGPT sign-in and plan can't be shared or lent.");
  await app.press(`id:sharing-visibility:${oauth.id}:organization`);
  expect(app.shows(`id:sharing-save:${oauth.id} >> text:Share preset with the organization`)).toBe(true);
  expect(text(app)).toContain("Their requests go to their own sign-in, never through yours.");

  await app.press(`id:connection-manage:${gpu.id}`);
  await app.press(`id:connection-sharing:${gpu.id}`); // one panel at a time
  await app.waitFor(`id:sharing-visibility:${gpu.id}`);
  expect(app.shows(`id:sharing:${oauth.id}`)).toBe(false);
  await app.press(`id:sharing-visibility:${gpu.id}:organization`);
  expect(text(app)).toContain("your computer and your metro-connector (their requests fail while it's offline)");
  expect(text(app)).toContain("As the owner of the device, you can see what they send, and you may pay for their use.");
});

// ------------------------------------------------------------------ navigation

test("Search is the box at the top of every page and Scheduled tasks a sidebar destination, both with no Space", async () => {
  const f = world();
  const app = await signIn(f, "carol");
  expect(app.shows("id:nav-search")).toBe(false); // no second way into Search
  await app.waitFor("id:top-search");
  await app.type("id:top-search", "pricing");
  await app.wait(300);
  // Typing may ask Metro for literal quick matches (/v1/search/quick); it never runs the semantic search or indexing.
  expect(f.posts().some((p) => p.path === "/v1/search" || p.path === "/v1/search/index")).toBe(false);
  await app.submit("id:top-search"); // Return opens Search
  await app.waitFor("id:search-view");
  await app.press("id:nav-schedules");
  await app.waitFor("id:search-view", { state: "gone" });
  expect(app.shows("id:top-search")).toBe(true);
  expect(app.value("id:top-search")).toBe("pricing");
  await app.press("id:nav-chat");
  await app.waitFor("id:onboarding");
  expect(app.shows("id:top-search")).toBe(true);
});

test("in the narrowest window the opt-ins, their consent lines, Save and the runner controls stay reachable", async () => {
  const { f } = routingWorld();
  const app = await open("../../App.tsx", { network: f.network as any, platform: "linux", width: 720, height: 560 });
  await signIn(f, "alice", app);
  await settings(app, "routing");
  await app.waitFor("id:routing-enabled");
  // Consent sits beside each toggle, not behind hover or a disclosure.
  expect(app.shows("text:Off by default. When on, each agent request you start is sent to System One to pick a model.")).toBe(true);
  expect(app.shows("text:/each message you write that mentions nobody is sent to System One/")).toBe(true);
  await app.press("id:routing-infer");
  expect(app.shows("id:routing-save")).toBe(true); // the fixed footer
  expect(app.enabled("id:routing-save")).toBe(true);
  expect(app.shows("id:dataflow-addressing-facts >> text:Each message you write that mentions nobody and isn't sent with Ask agent")).toBe(true);
  await app.press("id:routing-revert");
  await settings(app, "runners");
  await app.waitFor("id:runner-name");
  expect(app.shows("id:add-runner")).toBe(true);
  expect(app.shows("text:You'll get its token once, with the steps to start it.")).toBe(true);
});
