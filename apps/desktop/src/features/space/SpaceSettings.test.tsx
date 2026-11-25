// Space settings against the shared fake server plus this lane's Space
// routes (./test/fixtures.ts). Real NSH engine, real store and views, no
// network; fake tokens only.

import { expect, open, test } from "nsh/test";
import type { App } from "nsh/test";
import { BASE, TOKENS } from "../../../test/fakeServer";
import { spaceWorld } from "./test/fixtures";
import { launched } from "../../../test/launched";

type W = ReturnType<typeof spaceWorld>;

async function signIn(w: W, user: "alice" | "bob" | "carol" = "alice", manual = false): Promise<App> {
  const app = await open("./test/Harness.tsx", { modules: launched(), network: w.network as any, platform: "linux", width: 1280, height: 900, ...(manual ? { clock: {} } : {}) });
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

test("Tools uses server-supported groups and saves explicit Web and Pages permission changes only", async () => {
  const w = spaceWorld();
  w.s.toolCatalog = ["workspace.list", "threads", "web_fetch", "pages_list", "page_propose"];
  const a = w.agent("acme", { name: "Reader", handle: "reader", tools: ["threads", "page_propose"] });
  const app = await signIn(w);
  await openSettingsFor(app);
  await app.press("id:space-settings-tab:agents");
  await app.waitFor(`id:agent:${a.id} >> text:Propose Page changes`);
  await app.press(`id:edit-agent:${a.id}`);
  await app.waitFor("id:agent-tool:web_fetch");
  expect(app.shows("id:agent-tools:web >> text:Read web pages")).toBe(true);
  expect(app.shows("id:agent-tools:workspace >> text:List files")).toBe(true);
  expect(app.shows("id:agent-tool:web_search")).toBe(false);
  expect(app.shows("id:agent-tool:page_read")).toBe(false);
  expect(app.texts().join(" ")).toContain("Members can see exact queries in run activity");
  expect(app.texts().join(" ")).toContain("nothing changes until the starter applies it with edit rights");
  await app.press("id:agent-tool:web_fetch");
  await app.press("id:agent-tool:page_propose");
  await app.press("id:agent-tool:pages_list");
  await app.press("id:agent-save");
  await app.waitFor("id:agent-editor", { state: "gone" });
  const route = `/v1/spaces/acme/agents/${a.id}`;
  expect(calls(w, "PATCH", route).map((p) => p.body)).toEqual([{ tools: ["threads", "web_fetch", "pages_list"] }]);
  await app.waitFor(`id:agent:${a.id} >> text:List Pages`);
  await app.press(`id:edit-agent:${a.id}`);
  await app.press("id:agent-tool:web_fetch");
  await app.press("id:agent-tool:pages_list");
  await app.press("id:agent-save");
  await app.waitFor("id:agent-editor", { state: "gone" });
  expect(calls(w, "PATCH", route).at(-1)?.body).toEqual({ tools: ["threads"] });
});

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
  // Members can see the roster and test the runner, without management.
  await app.press("id:space-settings-tab:members");
  await app.waitFor("id:space-members");
  await app.waitFor("id:member:alice >> text:Owner");
  await app.waitFor("id:member:bob >> text:Member");
  expect(app.shows("id:add-member")).toBe(false);
  expect(app.shows("id:remove-member:bob")).toBe(false);
  await app.press("id:space-settings-tab:runner");
  await app.waitFor("id:space-runner-status >> text:Dedicated token");
  expect(app.shows("id:space-runner-advanced:toggle")).toBe(false);
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

test("legacy room shortcut opens Members and preserves the room draft; owner adds and removes by ID", async () => {
  const w = spaceWorld();
  const app = await signIn(w);
  await app.press("id:space:acme");
  await app.waitFor("id:manage-space");
  await app.type("id:composer", "Unsent room draft");
  await app.press("id:manage-space");
  await app.waitFor("id:space-members");
  await app.waitFor("id:member:alice >> text:Owner");
  expect(app.shows("id:remove-member:alice")).toBe(false);
  await app.type("id:add-member-id", "carol");
  await app.press("id:add-member");
  await app.waitFor("id:member:carol");
  expect(w.f.posts().filter((p) => p.path === "/v1/spaces/acme/members").map((p) => p.body)).toEqual([{ user_id: "carol" }]);
  await app.press("id:remove-member:carol");
  await app.press("id:remove-member:carol:cancel");
  expect(w.f.state.spaces.get("acme")!.members.has("carol")).toBe(true);
  await app.press("id:remove-member:carol");
  await app.press("id:remove-member:carol:confirm");
  await app.waitFor("id:member:carol", { state: "gone" });
  expect(w.f.state.spaces.get("acme")!.members.has("carol")).toBe(false);
  await app.press("id:space-settings-close");
  await app.waitFor("id:space-settings-dialog", { state: "gone" });
  expect(app.value("id:composer")).toBe("Unsent room draft");
  expect(app.texts()).toContain("# lobby");
});

test("a Members page refreshes live and closes when the viewer loses access", async () => {
  const w = spaceWorld();
  const rosterPath = "/v1/spaces/acme/members";
  const get = `GET ${BASE}/v1/*`;
  const respond = w.network[get]!;
  let slow = false;
  w.network[get] = (req) => {
    const reply = respond(req);
    return slow && new URL(req.url).pathname === rosterPath ? { ...(reply as object), delay: 6_000 } : reply;
  };
  const rosterReads = () => w.f.state.log.filter((p) => p.method === "GET" && p.path === rosterPath).length;
  const app = await signIn(w, "bob", true);
  // Open Acme settings over the other Space, without changing rooms.
  await app.press("id:space:side");
  await app.waitFor("text:side lobby only");
  await app.press("id:space-menu:acme");
  await app.press("id:space-settings:acme");
  await app.waitFor("id:space-name-readonly");
  await app.press("id:space-settings-tab:runner");
  await app.waitFor("id:test-runner");
  expect(app.enabled("id:test-runner")).toBe(false);
  expect(app.texts().join(" ")).toContain("Open a room of this Space first");
  await app.press("id:space-settings-tab:members");
  await app.waitFor("id:member:alice");
  const before = rosterReads();
  w.f.state.spaces.get("acme")!.members.add("carol");
  slow = true;
  await app.clock!.runFor(10_000);
  expect(rosterReads()).toBe(before + 1); // A slow read is not starved by the next tick.
  await app.clock!.runFor(1_000);
  await app.waitFor("id:member:carol");
  slow = false;
  w.f.state.spaces.get("acme")!.members.delete("bob");
  await app.clock!.runFor(5_000);
  await app.waitFor("id:space-settings-dialog", { state: "gone" });
  await app.waitFor("id:space:acme", { state: "gone" });
  expect(app.shows("text:side lobby only")).toBe(true);
});

for (const mode of ["closed", "superseded", "replacement account"] as const) {
  test(`a held hosted Members 401 is handled after ${mode}`, async () => {
    const w = spaceWorld();
    w.f.addSpace("bob-only", "Bob only", "bob");
    const get = `GET ${BASE}/v1/*`;
    const respond = w.network[get]!;
    let armed = false;
    let parked = false;
    w.network[get] = (req) => {
      if (armed && !parked && new URL(req.url).pathname === "/v1/spaces/acme/members") {
        parked = true;
        return { status: 401, body: {}, delay: 3_000 };
      }
      return respond(req);
    };
    const app = await signIn(w, "alice", true);
    await openSettingsFor(app);
    armed = true;
    await app.press("id:space-settings-tab:members");
    await app.waitUntil(() => parked);
    if (mode === "superseded") {
      await app.press("id:space-settings-tab:general");
      await app.press("id:space-settings-tab:members");
      await app.waitFor("id:member:bob");
    } else {
      await app.press("id:space-settings-close");
    }

    if (mode === "replacement account") {
      await app.press("id:account-menu");
      await app.press("id:disconnect");
      await app.waitFor("id:connect");
      await app.type("id:access-token", TOKENS.bob!);
      await app.press("id:connect");
      await app.waitFor("id:space:bob-only");
    }
    await app.clock!.runFor(3_100);
    if (mode === "replacement account") {
      expect(app.shows("id:space:bob-only")).toBe(true);
      expect(app.shows("id:connect")).toBe(false);
    } else {
      await app.waitFor("id:connect");
      expect(app.shows("id:space:acme")).toBe(false);
    }
  });
}

test("attach and confirmed detach affect only the Space being configured", async () => {
  const w = spaceWorld();
  w.f.addSpace("empty", "Empty", "alice");
  w.f.addSpace("other", "Other", "alice");
  const r = w.f.addRunner("Shared", "organization", "bob");
  const foreign = w.f.addRunner("Bob’s private runner", "personal", "bob");
  w.f.state.attached.set("other", r.id);
  const app = await signIn(w);
  await openSettingsFor(app, "empty");
  await app.press("id:space-settings-tab:runner");
  await app.waitFor(`id:space-runner-attach:${r.id}`);
  expect(app.shows(`id:space-runner-attach:${foreign.id}`)).toBe(false);
  expect(w.f.posts()).toEqual([]);
  await app.press(`id:space-runner-attach:${r.id}`);
  await app.waitFor("id:space-runner-status >> text:Attached · Shared");
  expect(w.f.posts().filter((p) => p.path.includes("/runner"))).toEqual([
    { user: "alice", method: "POST", path: "/v1/spaces/empty/runner/attach-if-empty", body: { runner_id: r.id } },
  ]);
  await app.press("id:space-runner-detach");
  await app.press("id:space-runner-detach:cancel");
  expect(w.f.state.attached.get("empty")).toBe(r.id);
  await app.press("id:space-runner-detach");
  await app.press("id:space-runner-detach:confirm");
  await app.waitFor("id:space-runner-status >> text:No runner");
  expect(w.f.state.attached.has("empty")).toBe(false);
  expect(w.f.state.attached.get("other")).toBe(r.id);
});

test("an old server cannot offer or fall back to a replacing attach", async () => {
  const w = spaceWorld();
  w.f.addSpace("empty", "Empty", "alice");
  const r = w.f.addRunner("Runner", "personal", "alice");
  w.f.state.attachIfEmpty = false;
  const app = await signIn(w);
  await openSettingsFor(app, "empty");
  await app.press("id:space-settings-tab:runner");
  await app.waitFor("id:space-runner-attach-unsupported");
  expect(app.shows(`id:space-runner-attach:${r.id}`)).toBe(false);
  await app.press("id:space-runner-advanced:toggle");
  expect(app.shows("id:issue-runner-token")).toBe(true);
  expect(app.shows("id:local-tools-card")).toBe(false);
  expect(w.f.posts()).toEqual([]);
});

test("a concurrent dedicated pairing is never replaced, and detach confirms the same runner", async () => {
  const w = spaceWorld();
  w.f.addSpace("empty", "Empty", "alice");
  const r = w.f.addRunner("First runner", "personal", "alice");
  const other = w.f.addRunner("Different runner", "organization", "alice");
  const app = await signIn(w);
  await openSettingsFor(app, "empty");
  await app.press("id:space-settings-tab:runner");
  await app.waitFor(`id:space-runner-attach:${r.id}`);
  w.f.state.beforeEmptyAttach = (sid) => { w.f.state.spaces.get(sid)!.runnerToken = "fixture-dedicated"; };
  await app.press(`id:space-runner-attach:${r.id}`);
  await app.waitFor("id:space-runner-status >> text:Dedicated token");
  expect(app.shows("id:manage-error")).toBe(true);
  expect(w.f.state.attached.has("empty")).toBe(false);
  expect(w.f.state.spaces.get("empty")!.runnerToken).toBe("fixture-dedicated");
  expect(app.texts().join(" ")).not.toContain("fixture-dedicated");
  expect(app.shows(`id:space-runner-attach:${r.id}`)).toBe(false);
  w.f.state.beforeEmptyAttach = null;
  w.f.state.spaces.get("empty")!.runnerToken = null;
  w.f.state.attached.set("empty", r.id);
  await app.press("id:space-settings-tab:members");
  await app.press("id:space-settings-tab:runner");
  await app.waitFor("id:space-runner-detach");
  await app.press("id:space-runner-detach");
  // This lands inside the fixture handling the destructive request, after
  // any reads the desktop could have made for its old confirmation.
  w.f.state.beforeGuardedDetach = (sid) => { w.f.state.attached.set(sid, other.id); };
  await app.press("id:space-runner-detach:confirm");
  await app.waitFor("id:space-runner-status >> text:Attached · Different runner");
  expect(app.shows("id:manage-error")).toBe(true);
  expect(app.texts().join(" ")).toContain("Nothing was removed. Review the current runner before confirming again.");
  expect(app.shows("id:space-runner-detach:confirm")).toBe(false);
  expect(w.f.state.attached.get("empty")).toBe(other.id);
  const attempts = () => w.f.posts().filter((p) => p.path.includes("/runner"));
  expect(attempts().map((p) => [p.method, p.path])).toEqual([
    ["POST", "/v1/spaces/empty/runner/attach-if-empty"],
    ["POST", "/v1/spaces/empty/runner/detach-if-current"],
  ]);
  const oldRevision = attempts().at(-1)!.body.assignment_revision;
  w.f.state.beforeGuardedDetach = null;
  await app.press("id:space-runner-detach");
  expect(app.texts().join(" ")).toContain("This Space stops sending jobs to Different runner.");
  await app.press("id:space-runner-detach:confirm");
  await app.waitFor("id:space-runner-status >> text:No runner");
  expect(attempts()).toHaveLength(3);
  expect(attempts().at(-1)!.body.assignment_revision).not.toBe(oldRevision);
});

test("delayed attach and detach responses cannot overwrite newer actual assignments", async () => {
  const w = spaceWorld();
  w.f.addSpace("empty", "Empty", "alice");
  const first = w.f.addRunner("First runner", "personal", "alice");
  const second = w.f.addRunner("Second runner", "personal", "alice");
  const gate = (method: string, path: string) => {
    const key = `${method} ${BASE}/v1/*`;
    const respond = w.network[key]!;
    w.network[key] = (req) => {
      const reply = respond(req);
      return new URL(req.url).pathname === path ? { ...(reply as object), delay: 800 } : reply;
    };
  };
  gate("POST", "/v1/spaces/empty/runner/attach-if-empty");
  gate("POST", "/v1/spaces/empty/runner/detach-if-current");
  const app = await signIn(w, "alice", true);
  await openSettingsFor(app, "empty");
  await app.press("id:space-settings-tab:runner");
  await app.press(`id:space-runner-attach:${first.id}`);
  await app.waitUntil(() => w.f.state.attached.get("empty") === first.id);
  w.f.state.attached.set("empty", second.id);
  await app.clock!.runFor(800);
  await app.waitFor("id:space-runner-status >> text:Attached · Second runner");
  await app.waitFor("id:manage-error");
  expect(app.shows("id:manage-notice")).toBe(false);

  await app.press("id:space-runner-detach");
  await app.press("id:space-runner-detach:confirm");
  await app.waitUntil(() => !w.f.state.attached.has("empty"));
  w.f.state.attached.set("empty", first.id);
  await app.clock!.runFor(800);
  await app.waitFor("id:space-runner-status >> text:Attached · First runner");
  await app.waitFor("id:manage-error");
  expect(app.shows("id:manage-notice")).toBe(false);
  expect(app.shows("id:test-runner")).toBe(true);
});

test("revoke requires a fresh confirmation after a dedicated token rotates", async () => {
  const w = spaceWorld();
  const rotated = "fixture-rotated-dedicated";
  const app = await signIn(w);
  await openSettingsFor(app);
  await app.press("id:space-settings-tab:runner");
  await app.press("id:space-runner-advanced:toggle");
  await app.press("id:revoke-runner-token");
  w.f.state.beforeGuardedDetach = (sid) => { w.f.state.spaces.get(sid)!.runnerToken = rotated; };
  await app.press("id:revoke-runner-token:confirm");
  await app.waitFor("id:manage-error");
  await app.waitFor("id:revoke-runner-token");
  expect(app.shows("id:revoke-runner-token:confirm")).toBe(false);
  expect(w.f.state.spaces.get("acme")!.runnerToken).toBe(rotated);
  expect(app.texts().some((t) => t.includes(rotated))).toBe(false);
  w.f.state.beforeGuardedDetach = null;
  await app.press("id:revoke-runner-token");
  await app.press("id:revoke-runner-token:confirm");
  await app.waitFor("id:space-runner-status >> text:No runner");
  const writes = w.f.posts().filter((p) => p.path.includes("/runner"));
  expect(writes.map((p) => [p.method, p.path])).toEqual([
    ["POST", "/v1/spaces/acme/runner/detach-if-current"],
    ["POST", "/v1/spaces/acme/runner/detach-if-current"],
  ]);
  expect(writes[0]!.body.assignment_revision).not.toBe(writes[1]!.body.assignment_revision);
});

test("an old or inconsistent server never gets an unguarded detach", async () => {
  for (const legacy of [true, false]) {
    const w = spaceWorld();
    w.f.addSpace("paired", "Paired", "alice");
    const r = w.f.addRunner("Existing runner", "personal", "alice");
    w.f.state.attached.set("paired", r.id);
    w.f.state.guardedDetachSupported = !legacy;
    w.f.state.missingGuardedDetachRoute = !legacy;
    const app = await signIn(w);
    await openSettingsFor(app, "paired");
    await app.press("id:space-settings-tab:runner");
    await app.waitFor("id:space-runner-status >> text:Attached · Existing runner");
    if (legacy) {
      await app.waitFor("id:space-runner-removal-unavailable");
      expect(app.shows("id:space-runner-detach")).toBe(false);
      expect(w.f.posts()).toEqual([]);
    } else {
      await app.press("id:space-runner-detach");
      await app.press("id:space-runner-detach:confirm");
      await app.waitFor("id:manage-error");
      expect(app.texts().join(" ")).toContain("No unguarded removal was attempted.");
      expect(w.f.posts().map((p) => [p.method, p.path])).toEqual([["POST", "/v1/spaces/paired/runner/detach-if-current"]]);
    }
    expect(w.f.state.attached.get("paired")).toBe(r.id);
  }
});

test("a dedicated token starts masked and cannot be recovered by reopening Runner", async () => {
  const w = spaceWorld();
  w.f.addSpace("empty", "Empty", "alice");
  const app = await signIn(w);
  await openSettingsFor(app, "empty");
  await app.press("id:space-settings-tab:runner");
  expect(app.shows("id:issue-runner-token")).toBe(false);
  await app.press("id:space-runner-advanced:toggle");
  await app.press("id:issue-runner-token");
  await app.waitFor("id:token-reveal");
  const issued = w.f.state.spaces.get("empty")!.runnerToken!;
  expect(!!issued).toBe(true);
  expect(app.texts().some((t) => t.includes(issued))).toBe(false);
  await app.press("id:space-settings-tab:members");
  await app.press("id:space-settings-tab:runner");
  await app.waitFor("id:space-runner-status >> text:Dedicated token");
  expect(app.shows("id:token-reveal")).toBe(false);
  await app.press("id:space-runner-advanced:toggle");
  await app.waitFor("id:issue-runner-token");
  expect(app.shows("id:copy-runner-token")).toBe(false);
});

test("issue refuses both kinds of runner paired before the guarded request commits", async () => {
  for (const kind of ["registered", "dedicated"]) {
    const w = spaceWorld();
    w.f.addSpace("empty", "Empty", "alice");
    const r = w.f.addRunner("Racing runner", "personal", "alice");
    const dedicated = "fixture-concurrent-pair";
    const app = await signIn(w);
    await openSettingsFor(app, "empty");
    await app.press("id:space-settings-tab:runner");
    await app.press("id:space-runner-advanced:toggle");
    w.f.state.beforeGuardedTokenChange = (sid) => {
      if (kind === "registered") w.f.state.attached.set(sid, r.id);
      else w.f.state.spaces.get(sid)!.runnerToken = dedicated;
    };
    await app.press("id:issue-runner-token");
    await app.waitFor("id:manage-error");
    const status = kind === "registered" ? "Attached · Racing runner" : "Dedicated token";
    await app.waitFor(`id:space-runner-status >> text:${status}`);
    expect(app.shows("id:token-reveal")).toBe(false);
    expect(w.f.state.attached.get("empty")).toBe(kind === "registered" ? r.id : undefined);
    expect(w.f.state.spaces.get("empty")!.runnerToken === (kind === "dedicated" ? dedicated : null)).toBe(true);
    expect(w.f.posts().map((p) => p.path)).toEqual(["/v1/spaces/empty/runner-token/issue-if-empty"]);
  }
});

test("a server without guarded token changes receives no dedicated token write", async () => {
  const w = spaceWorld();
  w.f.addSpace("empty", "Empty", "alice");
  w.f.state.guardedTokenChanges = false;
  const app = await signIn(w);
  for (const sid of ["empty", "acme"]) {
    await openSettingsFor(app, sid);
    await app.press("id:space-settings-tab:runner");
    await app.press("id:space-runner-advanced:toggle");
    await app.waitFor("id:space-runner-token-change-unavailable");
    expect(app.shows("id:issue-runner-token")).toBe(false);
    await app.press("id:space-settings-close");
  }
  expect(w.f.posts()).toEqual([]);
});

test("rotate refuses a dedicated revision revoked or rotated after the confirmation opened", async () => {
  for (const revoked of [true, false]) {
    const w = spaceWorld();
    const changed = revoked ? null : "fixture-concurrent-rotation";
    const app = await signIn(w);
    await openSettingsFor(app);
    await app.press("id:space-settings-tab:runner");
    await app.press("id:space-runner-advanced:toggle");
    await app.press("id:issue-runner-token");
    expect(w.f.posts()).toEqual([]); // The prompt itself is only a review.
    w.f.state.beforeGuardedTokenChange = (sid) => { w.f.state.spaces.get(sid)!.runnerToken = changed; };
    await app.press("id:issue-runner-token:confirm");
    await app.waitFor("id:manage-error");
    await app.waitFor(`id:space-runner-status >> text:${revoked ? "No runner" : "Dedicated token"}`);
    expect(app.shows("id:token-reveal")).toBe(false);
    expect(app.shows("id:issue-runner-token:confirm")).toBe(false);
    expect(w.f.state.spaces.get("acme")!.runnerToken === changed).toBe(true);
    expect(w.f.posts().map((p) => p.path)).toEqual(["/v1/spaces/acme/runner-token/rotate-if-current"]);
  }
});

test("a delayed or missing token receipt never reveals a superseded token or retries the write", async () => {
  for (const mode of ["changed", "lost", "scope left"]) {
    const w = spaceWorld();
    const key = `POST ${BASE}/v1/*`;
    const respond = w.network[key]!;
    const route = "/v1/spaces/acme/runner-token/rotate-if-current";
    w.network[key] = (req) => {
      const reply = respond(req);
      if (new URL(req.url).pathname !== route) return reply;
      return mode === "lost" ? { status: 200, body: {}, delay: 800 } : { ...(reply as object), delay: 800 };
    };
    const app = await signIn(w, "alice", true);
    await openSettingsFor(app);
    await app.press("id:space-settings-tab:runner");
    await app.press("id:space-runner-advanced:toggle");
    await app.press("id:issue-runner-token");
    await app.press("id:issue-runner-token:confirm");
    await app.waitUntil(() => w.f.posts().some((p) => p.path === route));
    const candidate = w.f.state.spaces.get("acme")!.runnerToken!;
    if (mode === "changed") w.f.state.spaces.get("acme")!.runnerToken = "fixture-newer-dedicated";
    if (mode === "scope left") await app.press("id:space-settings-tab:members");
    await app.clock!.runFor(800);
    if (mode === "scope left") {
      await app.press("id:space-settings-tab:runner");
      await app.waitFor("id:space-runner-status >> text:Dedicated token");
    } else {
      await app.waitFor("id:manage-error");
      expect(app.texts().join(" ")).toContain(mode === "lost" ? "Check the current runner before deciding" : "No token was shown");
    }
    expect(app.shows("id:token-reveal")).toBe(false);
    expect(app.texts().some((t) => t.includes(candidate))).toBe(false);
    expect(JSON.stringify(app.storage()).includes(candidate)).toBe(false);
    expect(w.f.posts().map((p) => p.path)).toEqual([route]);
  }
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
