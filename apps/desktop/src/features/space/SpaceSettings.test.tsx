// Space settings against the shared fake server plus this lane's Space
// routes (./test/fixtures.ts). Real NSH engine, real store and views, no
// network; fake tokens only.

import { expect, open, test } from "nsh/test";
import type { App } from "nsh/test";
import { BASE, TOKENS } from "../../../test/fakeServer";
import { spaceWorld } from "./test/fixtures";
import { launched } from "../../../test/launched";

type W = ReturnType<typeof spaceWorld>;

async function signIn(w: W, user: "alice" | "bob" | "carol" = "alice"): Promise<App> {
  const app = await open("./test/Harness.tsx", { modules: launched(), network: w.network as any, platform: "linux", width: 1280, height: 900 });
  await app.clear("id:server-url");
  await app.type("id:server-url", BASE);
  await app.type("id:access-token", TOKENS[user]!);
  await app.press("id:connect");
  await app.waitFor("id:h-space-settings");
  return app;
}

async function openSettingsFor(app: App, space = "acme") {
  await app.waitFor(`id:space:${space}`);
  await app.press(`id:space:${space}`);
  await app.waitFor("id:room-title");
  await app.press("id:h-space-settings");
  await app.waitFor("id:space-settings");
}

const calls = (w: W, method: string, path: string) => w.s.log.filter((r) => r.method === method && r.path === path);

for (const legacy of [false, true]) {
  test(`new agent tool defaults stay opt-in (${legacy ? "legacy catalogue" : "explicit defaults"})`, async () => {
    const w = spaceWorld();
    w.s.toolCatalog = ["workspace.list", "workspace.read", "threads", "pages_list", "page_read", "page_propose", "web_search", "web_fetch"];
    w.s.defaultTools = legacy ? undefined : []; // An explicit empty list is not a legacy response.
    const app = await signIn(w);
    await openSettingsFor(app);
    await app.press("id:space-settings-tab:agents");
    await app.waitFor("id:agent:ag-1");
    await app.press("id:add-agent");
    await app.type("id:agent-name", "Fixture assistant");
    await app.type("id:agent-description", "Checks tool defaults.");
    if (!legacy) {
      await app.press("id:agent-tool:page_read");
      await app.press("id:agent-tool:web_search");
    }
    await app.press("id:agent-save");
    await app.waitUntil(() => !app.shows("id:agent-editor"));
    expect(calls(w, "POST", "/v1/spaces/acme/agents")[0]!.body.tools).toEqual(
      legacy ? ["workspace.list", "workspace.read", "threads"] : ["page_read", "web_search"],
    );
  });
}

test("the owner edits the Space's name and description; the new name shows everywhere", async () => {
  const w = spaceWorld();
  const app = await signIn(w);
  await openSettingsFor(app);
  await app.waitFor("id:space-name");
  expect(app.shows("id:space-settings-tab:members")).toBe(true);
  expect(app.value("id:space-description")).toContain("Where Acme ships.");
  // Nothing changed yet: Save is disabled.
  expect(app.enabled("id:space-general-save")).toBe(false);
  await app.clear("id:space-name");
  await app.type("id:space-name", "Acme HQ");
  await app.clear("id:space-description");
  await app.type("id:space-description", "Ship it.");
  await app.press("id:space-general-save");
  await app.waitFor("id:space-settings-notice");
  const patch = calls(w, "PATCH", "/v1/spaces/acme");
  expect(patch.length).toBe(1);
  expect(patch[0]!.body).toEqual({ name: "Acme HQ", description: "Ship it." });
  expect(w.f.state.spaces.get("acme")!.name).toBe("Acme HQ");
  await app.waitUntil(() => app.texts().some((t) => t === "Acme HQ"));
});

test("deleting needs the exact name typed, says what goes, and forgets the Space", async () => {
  const w = spaceWorld();
  const app = await signIn(w);
  await openSettingsFor(app);
  await app.press("id:delete-space");
  await app.waitFor("id:delete-space-confirm-panel");
  const words = app.texts().join(" ");
  expect(words).toContain("rooms");
  expect(words).toContain("library files");
  expect(words).toContain("scheduled prompts");
  expect(words).toContain("detaches its runner");
  await app.type("id:delete-space-name", "acme");
  expect(app.enabled("id:delete-confirm")).toBe(false);
  await app.clear("id:delete-space-name");
  await app.type("id:delete-space-name", "Acme");
  await app.press("id:delete-confirm");
  await app.waitUntil(() => !app.shows("id:space-settings"));
  const del = calls(w, "DELETE", "/v1/spaces/acme");
  expect(del.length).toBe(1);
  expect(del[0]!.body).toEqual({ confirm_name: "Acme" });
  await app.waitUntil(() => !app.shows("id:space:acme"));
});

test("a member reads General and the agents, can't edit them, and can leave", async () => {
  const w = spaceWorld();
  const app = await signIn(w, "bob");
  await openSettingsFor(app);
  await app.waitFor("id:space-name-readonly");
  expect(app.shows("id:space-name")).toBe(false);
  expect(app.shows("id:delete-space")).toBe(false);
  // Members and runner: the runner's status and Test runner, no management.
  await app.press("id:space-settings-tab:members");
  await app.waitFor("id:space-members");
  expect(app.shows("id:open-manage")).toBe(false);
  expect(app.shows("id:space-runner-status >> text:Runner attached")).toBe(true);
  await app.press("id:test-runner");
  await app.waitFor("text:/^Requested\\. The listing appears in # lobby/");
  expect(w.f.posts().at(-1)).toMatchObject({ path: "/v1/spaces/acme/rooms/lobby/demo" });

  await app.press("id:space-settings-tab:agents");
  await app.waitFor("id:agent-list");
  const def = w.s.agents.get("acme")![0]!;
  await app.waitFor(`id:agent:${def.id}`);
  expect(app.shows(`id:agent-default:${def.id}`)).toBe(true);
  expect(app.shows("id:add-agent")).toBe(false);
  expect(app.shows(`id:edit-agent:${def.id}`)).toBe(false);

  await app.press("id:space-settings-tab:general");
  await app.press("id:leave-space");
  await app.press("id:leave-confirm");
  await app.waitUntil(() => !app.shows("id:space-settings"));
  expect(calls(w, "POST", "/v1/spaces/acme/leave").length).toBe(1);
  await app.waitUntil(() => !app.shows("id:space:acme"));
});

test("the owner adds an agent with live handle checks, makes it the default and deletes the other", async () => {
  const w = spaceWorld();
  const app = await signIn(w);
  await openSettingsFor(app);
  await app.press("id:space-settings-tab:agents");
  const def = w.s.agents.get("acme")![0]!;
  await app.waitFor(`id:agent:${def.id}`);
  await app.press("id:add-agent");
  await app.waitFor("id:agent-editor");

  // The handle follows the name until typed; reserved and taken handles are refused at once.
  await app.type("id:agent-name", "Code Reviewer");
  await app.waitUntil(() => app.texts().length > 0);
  await app.clear("id:agent-handle");
  await app.type("id:agent-handle", "agent");
  await app.waitFor("id:agent-handle-problem");
  expect(app.texts().join(" ")).toContain("reserved");
  await app.clear("id:agent-handle");
  await app.type("id:agent-handle", "Bad Handle");
  await app.waitFor("id:agent-handle-problem");
  await app.clear("id:agent-handle");
  await app.type("id:agent-handle", "reviewer");
  await app.waitUntil(() => !app.shows("id:agent-handle-problem"));

  // The description is required; the hint says the classifier uses it.
  expect(app.texts().join(" ")).toContain("The classifier uses this to choose the agent");
  expect(app.texts().join(" ")).toContain("Every member of this Space can read them");
  expect(app.enabled("id:agent-save")).toBe(false);
  await app.type("id:agent-description", "Reviews pull requests");
  await app.type("id:agent-instructions", "Be brief.");
  await app.press("id:agent-tool:check.run");
  await app.press("id:agent-color:#059669");
  await app.press("id:agent-save");
  await app.waitUntil(() => !app.shows("id:agent-editor"));
  const post = calls(w, "POST", "/v1/spaces/acme/agents");
  expect(post.length).toBe(1);
  expect(post[0]!.body).toEqual({
    name: "Code Reviewer",
    handle: "reviewer",
    description: "Reviews pull requests",
    instructions: "Be brief.",
    tools: ["workspace.list", "workspace.read", "threads"],
    color: "#059669",
  });
  const added = w.s.agents.get("acme")![1]!;
  await app.waitFor(`id:agent:${added.id}`);

  await app.press(`id:default-agent:${added.id}`);
  await app.waitFor(`id:agent-default:${added.id}`);
  expect(calls(w, "PATCH", `/v1/spaces/acme/agents/${added.id}`)[0]!.body).toEqual({ is_default: true });
  expect(app.shows(`id:agent-default:${def.id}`)).toBe(false);

  await app.press(`id:delete-agent:${def.id}`);
  await app.press(`id:delete-agent-confirm:${def.id}`);
  await app.waitUntil(() => !app.shows(`id:agent:${def.id}`));
  expect(calls(w, "DELETE", `/v1/spaces/acme/agents/${def.id}`).length).toBe(1);
  // The last agent can't be deleted.
  expect(app.enabled(`id:delete-agent:${added.id}`)).toBe(false);
});

test("editing an agent sends only what changed; a taken handle shows the server's reason", async () => {
  const w = spaceWorld();
  const other = w.agent("acme", { name: "Writer", handle: "writer", description: "Writes docs." });
  const app = await signIn(w);
  await openSettingsFor(app);
  await app.press("id:space-settings-tab:agents");
  await app.waitFor(`id:edit-agent:${other.id}`);
  await app.press(`id:edit-agent:${other.id}`);
  await app.waitFor("id:agent-editor");
  // "bob" is a member's id: the server refuses it.
  await app.clear("id:agent-handle");
  await app.type("id:agent-handle", "bob");
  await app.press("id:agent-save");
  await app.waitFor("id:agent-error");
  expect(app.texts().join(" ")).toContain("already used");
  await app.clear("id:agent-handle");
  await app.type("id:agent-handle", "docs");
  await app.press("id:agent-save");
  await app.waitUntil(() => !app.shows("id:agent-editor"));
  const patches = calls(w, "PATCH", `/v1/spaces/acme/agents/${other.id}`);
  expect(patches[patches.length - 1]!.body).toEqual({ handle: "docs" });
});

test("your preferences: the classifier switch needs System One; routing and models are your own", async () => {
  const w = spaceWorld();
  const chat = w.f.addConnection("alice", "openai", "My OpenAI", "gpt-test");
  const app = await signIn(w);
  await openSettingsFor(app);
  await app.press("id:space-settings-tab:preferences");
  await app.waitFor("id:classify-unaddressed");
  // No System One connection: off, disabled, with the way to add one.
  expect(app.enabled("id:classify-unaddressed")).toBe(false);
  expect(app.shows("id:classifier-missing")).toBe(true);
  expect(app.texts().join(" ")).toContain("Never other people's messages");

  await app.press("id:space-routing");
  await app.waitUntil(() => calls(w, "PUT", "/v1/spaces/acme/prefs").length === 1);
  expect(calls(w, "PUT", "/v1/spaces/acme/prefs")[0]!.body).toEqual({ classify_unaddressed: false, default_model: null, agent_models: {}, routing: false });
  expect(chat.id).toBeTruthy();
  await app.press("id:space-settings-close");
  await app.waitUntil(() => !app.shows("id:space-settings"));

  // With a usable classifier, turning it on is saved.
  w.s.classifier.add("alice");
  await app.press("id:h-space-settings");
  await app.waitFor("id:space-settings");
  await app.press("id:space-settings-tab:preferences");
  await app.waitFor("id:classify-unaddressed");
  await app.waitUntil(() => !app.shows("id:classifier-missing"));
  await app.press("id:classify-unaddressed");
  await app.waitUntil(() => calls(w, "PUT", "/v1/spaces/acme/prefs").length === 2);
  expect(calls(w, "PUT", "/v1/spaces/acme/prefs")[1]!.body.classify_unaddressed).toBe(true);
});

test("your default model in this Space is one of your own chat connections; No preference clears it", async () => {
  const w = spaceWorld();
  const chat = w.f.addConnection("alice", "openai", "My OpenAI", "gpt-test");
  const app = await signIn(w);
  await openSettingsFor(app);
  await app.press("id:space-settings-tab:preferences");
  await app.waitFor("id:space-default-model");
  await app.press("id:space-default-model");
  await app.press(`id:space-default-model:${chat.id}\u0001gpt-test`);
  await app.waitUntil(() => calls(w, "PUT", "/v1/spaces/acme/prefs").length === 1);
  expect(calls(w, "PUT", "/v1/spaces/acme/prefs")[0]!.body.default_model).toEqual({ provider_connection_id: chat.id, model_id: "gpt-test" });
  // Per agent: the default agent gets its own choice.
  const def = w.s.agents.get("acme")![0]!;
  await app.waitFor(`id:agent-model:${def.id}`);
  await app.press(`id:agent-model:${def.id}`);
  await app.press(`id:agent-model:${def.id}:${chat.id}\u0001gpt-test`);
  await app.waitUntil(() => calls(w, "PUT", "/v1/spaces/acme/prefs").length === 2);
  expect(calls(w, "PUT", "/v1/spaces/acme/prefs")[1]!.body.agent_models).toEqual({ [def.id]: { provider_connection_id: chat.id, model_id: "gpt-test" } });
  await app.press("id:space-default-model");
  await app.press("id:space-default-model:none");
  await app.waitUntil(() => calls(w, "PUT", "/v1/spaces/acme/prefs").length === 3);
  expect(calls(w, "PUT", "/v1/spaces/acme/prefs")[2]!.body.default_model).toBe(null);
});

test("a model with thinking levels offers them under your Space and per-agent choices; Provider default drops the level", async () => {
  const w = spaceWorld();
  const chat = w.f.addConnection("alice", "openai", "My OpenAI", "gpt-test");
  chat.models.find((m) => m.model_id === "gpt-test")!.thinking = { efforts: ["low", "high"], wire: "reasoning_effort" };
  // The grouped list the server sends (`chat_providers`), with the levels each model offers.
  const base = w.network[`GET ${BASE}/v1/*`]!;
  w.network[`GET ${BASE}/v1/*`] = (req: any) => {
    const res = base(req) as any;
    if (!new URL(req.url).pathname.endsWith("/models/available") || res?.status !== 200) return res;
    const chat_providers = [
      {
        provider_id: "openai",
        provider_name: "OpenAI",
        connections: [
          { connection_id: chat.id, name: chat.name, owned: true, owner_id: "alice", models: [{ id: "gpt-test", display_name: "gpt-test", reasoning_efforts: ["low", "high"], available: true }] },
        ],
      },
    ];
    return { ...res, body: { ...res.body, chat_providers } };
  };
  const app = await signIn(w);
  await openSettingsFor(app);
  await app.press("id:space-settings-tab:preferences");
  await app.waitFor("id:space-default-model");
  expect(app.shows("id:space-default-thinking")).toBe(false); // nothing chosen yet
  await app.press("id:space-default-model");
  await app.press(`id:space-default-model:${chat.id}\u0001gpt-test`);
  await app.waitFor("id:space-default-thinking");
  await app.press("id:space-default-thinking:high");
  await app.waitUntil(() => calls(w, "PUT", "/v1/spaces/acme/prefs").length === 2);
  expect(calls(w, "PUT", "/v1/spaces/acme/prefs")[1]!.body.default_model).toEqual({ provider_connection_id: chat.id, model_id: "gpt-test", reasoning_effort: "high" });
  await app.press("id:space-default-thinking:");
  await app.waitUntil(() => calls(w, "PUT", "/v1/spaces/acme/prefs").length === 3);
  expect(calls(w, "PUT", "/v1/spaces/acme/prefs")[2]!.body.default_model).toEqual({ provider_connection_id: chat.id, model_id: "gpt-test" });
  // Per agent, the same row.
  const def = w.s.agents.get("acme")![0]!;
  await app.press(`id:agent-model:${def.id}`);
  await app.press(`id:agent-model:${def.id}:${chat.id}\u0001gpt-test`);
  await app.waitFor(`id:agent-thinking:${def.id}`);
  await app.press(`id:agent-thinking:${def.id}:low`);
  await app.waitUntil(() => calls(w, "PUT", "/v1/spaces/acme/prefs").length === 5);
  expect(calls(w, "PUT", "/v1/spaces/acme/prefs")[4]!.body.agent_models).toEqual({ [def.id]: { provider_connection_id: chat.id, model_id: "gpt-test", reasoning_effort: "low" } });
});

test("an agent's picture comes from the file picker, checked by its bytes; a new agent's is sent after it's created", async () => {
  const w = spaceWorld();
  const app = await signIn(w);
  await openSettingsFor(app);
  await app.press("id:space-settings-tab:agents");
  const def = w.s.agents.get("acme")![0]!;
  await app.waitFor(`id:edit-agent:${def.id}`);
  await app.press(`id:edit-agent:${def.id}`);
  app.onPick(() => [{ name: "fake.png", type: "image/png", data: new TextEncoder().encode("<svg/>") }]);
  await app.press("id:agent-picture-choose");
  await app.waitFor("text:/isn't a PNG or JPEG/");
  expect(calls(w, "PUT", `/v1/spaces/acme/agents/${def.id}/avatar`).length).toBe(0);
  const PNG = Uint8Array.from([0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 0, 0, 0, 0]);
  app.onPick(() => [{ name: "bot.png", type: "image/png", data: PNG }]);
  await app.press("id:agent-picture-choose");
  await app.waitFor("text:Picture updated.");
  expect(calls(w, "PUT", `/v1/spaces/acme/agents/${def.id}/avatar`).length).toBe(1);
  await app.waitFor("id:agent-picture-remove");
  await app.press("id:agent-picture-remove");
  await app.waitFor("text:/Picture removed/");
  expect(calls(w, "DELETE", `/v1/spaces/acme/agents/${def.id}/avatar`).length).toBe(1);
  await app.press("id:agent-cancel");

  // A new agent: the picture waits for the agent to exist.
  await app.press("id:add-agent");
  await app.type("id:agent-name", "Bot");
  await app.type("id:agent-description", "Does chores");
  app.onPick(() => [{ name: "bot.png", type: "image/png", data: PNG }]);
  await app.press("id:agent-picture-choose");
  await app.waitFor("id:agent-picture-pending");
  expect(w.s.log.filter((r) => r.method === "PUT" && r.path.endsWith("/avatar")).length).toBe(1);
  await app.press("id:agent-save");
  await app.waitUntil(() => !app.shows("id:agent-editor"));
  const bot = w.s.agents.get("acme")!.find((a) => a.handle === "bot")!;
  expect(calls(w, "PUT", `/v1/spaces/acme/agents/${bot.id}/avatar`).length).toBe(1);
});
