// Headless tests: the real NSH engine against an in-memory fake server
// (test/fakeServer.ts) through nsh test's network stubs. No real network.

import { expect, open, test } from "nsh/test";
import type { App } from "nsh/test";
import { BASE, fakeServer, header, TOKENS, world } from "./test/fakeServer";
import { serverPlace } from "./src/features/Sidebar";

async function openApp(f: ReturnType<typeof fakeServer>, storage: Record<string, string> = {}) {
  return open("./App.tsx", { network: f.network as any, platform: "linux", width: 1200, height: 800, storage });
}

async function signIn(app: App, user: string, url = BASE) {
  await app.clear("id:server-url");
  await app.type("id:server-url", url);
  await app.type("id:access-token", TOKENS[user] ?? user);
  await app.press("id:connect");
}

async function openAcme(app: App) {
  await app.waitFor("id:space:acme");
  await app.press("id:space:acme");
  await app.waitFor("text:hello from bob in acme");
}

const text = (app: App) => app.texts().join(" | ");

/** Sign out is an item of the account menu at the bottom of the sidebar: open it first. */
async function signOut(app: App) {
  await app.press("id:account-menu");
  await app.press("id:disconnect");
}

/** Asks the agent with the composer's draft (the explicit to-the-agent case). */
async function askAgent(app: App) {
  await app.press("id:ask-agent");
}

test("refuses to send a token over plain HTTP to a remote host", async () => {
  const f = world();
  const app = await openApp(f);
  await signIn(app, "alice", "http://192.168.1.20:8787");
  await app.waitFor("id:connect-error");
  expect(text(app)).toContain("Refusing to send your access token over plain HTTP");
  expect(app.requests()).toHaveLength(0);
});

test("an account without Spaces lands in its Chat with instructions, no implicit lobby", async () => {
  const f = world();
  const app = await openApp(f);
  await signIn(app, "carol");
  await app.waitFor("id:onboarding");
  expect(app.shows("text:You're not in any Space yet")).toBe(true);
  // The ID isn't repeated here: it's in the sidebar's footer and Settings › Profile.
  expect(app.shows("id:onboarding-id")).toBe(false);
  expect(text(app)).toContain("carol");
  expect(f.state.log.some((l) => l.path.includes("/rooms"))).toBe(false);
  const first = app.requests()[0];
  expect(header(first as any, "authorization")).toBe(`Bearer ${TOKENS.carol}`);
  // Only the server URL is remembered; the token never reaches storage.
  await app.waitUntil(() => app.storage()["metro.serverUrl"] === BASE);
  expect(JSON.stringify(app.storage())).not.toContain(TOKENS.carol);
});

test("Spaces keep their own lobby, cursor and cache", async () => {
  const f = world();
  f.state.ignoreCursor = true; // the server repeats old events on every poll
  const app = await openApp(f);
  await signIn(app, "alice");
  await openAcme(app);
  await app.press("id:space:side");
  await app.waitFor("text:side lobby only");
  expect(app.shows("text:hello from bob in acme")).toBe(false);

  f.post("acme/lobby", { kind: "message", actor: "bob", text: "while you were away" });
  await app.press("id:space:acme");
  await app.waitFor("text:while you were away");
  await app.wait(1800);
  expect(app.count("text:hello from bob in acme")).toBe(1);
  expect(f.state.afters.some((a) => a.startsWith("acme/lobby:1"))).toBe(true); // resumed from its cursor
});

test("threads are nested under their parent room and show where they came from", async () => {
  const f = world();
  f.addRoom("acme", { id: "t1", name: "fix-build", parent_room_id: "lobby" });
  f.post("acme/t1", { kind: "agent", actor: "agent", text: "child agent working on the build" });
  const app = await openApp(f);
  await signIn(app, "alice");
  await openAcme(app);
  await app.press("id:room:t1");
  await app.waitFor("text:child agent working on the build");
  expect(app.shows("id:open-parent")).toBe(true);
  await app.press("id:open-parent");
  await app.waitFor("text:hello from bob in acme");
});

test("@mentions: pick a member, send, prune and clear on room change", async () => {
  const f = world();
  const app = await openApp(f);
  await signIn(app, "alice");
  await openAcme(app);

  await app.type("id:composer", "can you look @");
  await app.waitFor("id:mention-picker");
  await app.press("id:pick:bob");
  await app.waitFor("id:mention-chip:bob");
  expect(app.value("id:composer")).toBe("can you look @bob ");
  await app.press("id:send");
  await app.waitFor("text:can you look @bob");
  const sent = f.posts().find((p) => p.path.endsWith("/messages"))!;
  expect(sent.body).toEqual({ text: "can you look @bob", invoke_agent: false, mentions: [{ kind: "user", id: "bob" }] });
  expect(f.state.events.get("acme/lobby")!.some((e) => e.actor === "agent")).toBe(false); // a person only
  // The header names the recipient itself: a pill for bob, no agent badge, no "to people" note.
  const toBob = f.state.events.get("acme/lobby")!.find((e) => e.text === "can you look @bob")!;
  await app.waitFor(`id:recipient:${toBob.id}:bob`);
  expect(app.shows("text:@bob")).toBe(true);
  expect(app.shows("label:Addressed to @bob")).toBe(true);
  expect(app.shows(`id:recipient:${toBob.id}:agent`)).toBe(false);
  expect(app.shows(`id:recipient:${toBob.id}:ask-agent`)).toBe(false);
  expect(app.shows(`id:audience:${toBob.id}`)).toBe(false);
  expect(app.shows("text:to people")).toBe(false);

  // Ask agent with no mention: invoke_agent on the wire, and one "to the agent" badge.
  await app.type("id:composer", "summarise the lobby");
  await askAgent(app);
  expect(f.posts().filter((p) => p.path.endsWith("/messages")).at(-1)!.body).toEqual({ text: "summarise the lobby", invoke_agent: true });
  const asked = f.state.events.get("acme/lobby")!.find((e) => e.text === "summarise the lobby")!;
  await app.waitFor(`id:recipient:${asked.id}:ask-agent`);
  expect(app.count(`id:recipient:${asked.id}:ask-agent`)).toBe(1);
  expect(app.shows(`id:recipient:${asked.id}:agent`)).toBe(false);
  expect(app.shows("label:Addressed to the agent (Ask agent)")).toBe(true);
  await app.waitFor("text:(agent) on it: summarise the lobby");

  // A chip disappears when its text is deleted, and on room change.
  await app.press("id:mention");
  await app.press("id:pick:agent");
  await app.waitFor("id:mention-chip:agent");
  await app.clear("id:composer");
  await app.waitFor("id:mention-chip:agent", { state: "gone" });
  await app.press("id:mention");
  await app.press("id:pick:bob");
  await app.waitFor("id:mention-chip:bob");
  await app.press("id:space:side");
  await app.waitFor("text:side lobby only");
  expect(app.shows("id:mention-chips")).toBe(false);
});

test("mixed @bob @agent is addressed to both and starts the agent with no extra decision", async () => {
  const f = world();
  const app = await openApp(f);
  await signIn(app, "alice");
  await openAcme(app);
  await app.type("id:composer", "please both look @");
  await app.press("id:pick:bob");
  await app.type("id:composer", "@");
  await app.press("id:pick:agent");
  await app.waitFor("id:mention-chip:agent");
  await app.waitFor("id:mention-chip:bob");
  await app.press("id:send");
  await app.waitFor("text:please both look @bob @agent");
  const sent = f.posts().filter((p) => p.path.endsWith("/messages")).at(-1)!;
  const body = sent.body as { text: string; invoke_agent: boolean; mentions: { kind: string; id?: string }[] };
  // On the wire: both recipients, each once; the agent is addressed explicitly.
  expect(body.mentions).toContainEqual({ kind: "user", id: "bob" });
  expect(body.mentions.filter((m) => m.kind === "agent")).toHaveLength(1);
  expect(body.mentions).toHaveLength(2);
  expect(body.invoke_agent === true || body.mentions.some((m) => m.kind === "agent")).toBe(true);
  expect(body.text).toBe("please both look @bob @agent");
  // In the transcript: exactly one agent badge (the @agent pill, no second
  // Ask agent badge) and one pill for bob; no verbose audience sentence.
  const ev = f.state.events.get("acme/lobby")!.find((e) => e.actor === "alice" && e.text.startsWith("please both look"))!;
  await app.waitFor(`id:recipient:${ev.id}:agent`);
  expect(app.count(`id:recipient:${ev.id}:agent`)).toBe(1);
  expect(app.shows(`id:recipient:${ev.id}:ask-agent`)).toBe(false);
  expect(app.count(`id:recipient:${ev.id}:bob`)).toBe(1);
  expect(app.shows("label:Addressed to the agent")).toBe(true);
  expect(app.shows("label:Addressed to @bob")).toBe(true);
  expect(app.shows("text:to the agent and people")).toBe(false);
  expect(app.shows("text:to people")).toBe(false);
  // The agent is addressed explicitly, so it answers without a classification call.
  await app.waitFor("text:/\\(agent\\) on it: please both look/");
  expect(f.posts().some((p) => p.path === "/v1/systemone")).toBe(false);
});

test("a message that mentions me is highlighted as @you", async () => {
  const f = world();
  const ev = f.post("acme/lobby", { kind: "message", actor: "bob", text: "@alice please review", mentions: [{ kind: "user", id: "alice" }, { kind: "agent" }], audience: "mixed" });
  const app = await openApp(f);
  await signIn(app, "alice");
  await openAcme(app);
  await app.waitFor(`id:recipient:${ev.id}:alice`);
  // My pill reads @you, and says so to a screen reader; the agent has its one pill.
  expect(app.shows("text:@you")).toBe(true);
  expect(app.shows("label:Addressed to you")).toBe(true);
  expect(app.count(`id:recipient:${ev.id}:agent`)).toBe(1);
  expect(app.shows(`id:recipient:${ev.id}:ask-agent`)).toBe(false);
  expect(app.shows("text:to the agent and people")).toBe(false);
});

test("reactions: add and remove mine, fold the agent's acknowledgement, no extra rows", async () => {
  const f = world();
  const app = await openApp(f);
  await signIn(app, "alice");
  await openAcme(app);
  const target = f.state.events.get("acme/lobby")![0].id;

  await app.press(`id:react:${target}`);
  await app.press(`id:pick-reaction:${target}:👍`);
  await app.waitFor(`id:reaction:${target}:👍`);
  expect(f.posts().at(-1)).toMatchObject({ method: "PUT", path: `/v1/spaces/acme/rooms/lobby/events/${target}/reactions`, body: { emoji: "👍" } });

  f.react("acme/lobby", target, "✅", true, "agent", "agent");
  await app.waitFor(`id:reaction:${target}:✅`);
  expect(app.shows(`id:reaction:${target}:✅ >> text:agent`)).toBe(true);

  await app.press(`id:reaction:${target}:👍`);
  await app.waitFor(`id:reaction:${target}:👍`, { state: "gone" });
  expect(f.posts().at(-1)).toMatchObject({ method: "DELETE", body: { emoji: "👍" } });
  expect(text(app)).not.toContain("target_event_id");
});

test("Demo needs this Space's runner and shows its tool output collapsed", async () => {
  const f = world();
  const app = await openApp(f);
  await signIn(app, "alice");
  await openAcme(app);
  expect(app.shows("text:Runner attached")).toBe(true);
  await app.press("id:run-demo");
  // One collapsed Runner activity group: Demo is deterministic, so nothing says "thinking".
  await app.waitFor("text:Runner activity");
  expect(app.shows("text:/thinking/")).toBe(false);
  expect(app.shows("text:README.md\nsrc/")).toBe(false); // collapsed until opened
  expect(f.posts().at(-1)).toMatchObject({ path: "/v1/spaces/acme/rooms/lobby/demo", body: {} });

  await app.press("id:space:side");
  await app.waitFor("text:No runner");
  expect(app.enabled("id:run-demo")).toBe(false);
});

test("owners manage members and see a runner token once; members can't manage", async () => {
  const f = world();
  const app = await openApp(f);
  await signIn(app, "alice");
  await openAcme(app);
  await app.press("id:manage-space");
  await app.waitFor("id:member:bob");
  await app.type("id:add-member-id", "carol");
  await app.press("id:add-member");
  await app.waitFor("id:member:carol");
  await app.press("id:remove-member:carol");
  await app.waitFor("id:member:carol", { state: "gone" });

  // Acme has a dedicated (legacy) token: the owner can still rotate it.
  await app.waitFor("id:dedicated-runner");
  await app.press("id:issue-runner-token");
  await app.waitFor("id:runner-token-value");
  expect(text(app)).not.toContain("secret-runner-token-"); // masked until asked
  await app.press("id:copy-runner-token");
  expect(app.clipboard()).toContain("secret-runner-token-");
  await app.press("id:reveal-toggle");
  expect(text(app)).toContain("secret-runner-token-");
  await app.press("id:close-manage"); // leaving the view forgets it
  await app.press("id:manage-space");
  await app.waitFor("id:runner-card");
  expect(text(app)).not.toContain("secret-runner-token-");
  expect(JSON.stringify(app.storage())).not.toContain("secret-runner-token-");

  await app.press("id:space:side"); // Bob owns Side; Alice is a member
  await app.waitFor("text:side lobby only");
  expect(app.shows("id:manage-space")).toBe(false);
});

test("losing membership purges the Space and returns to the Chat", async () => {
  const f = world();
  const app = await openApp(f);
  await signIn(app, "bob");
  await openAcme(app);
  f.state.spaces.get("acme")!.members.delete("bob");
  await app.waitFor("text:You no longer have access to Acme.", { timeout: 15_000 });
  expect(app.shows("id:space:acme")).toBe(false);
  expect(app.shows("text:hello from bob in acme")).toBe(false);
  expect(app.shows("id:chat-view")).toBe(true);
});

test("Chat: personal messages, runs, follow-up to an explicit Space and room, stop", async () => {
  const f = world();
  f.state.runs.push({ id: "run1", space_id: "acme", room_id: "lobby", status: "running", actor: "alice", task: "Audit the build" });
  const app = await openApp(f);
  await signIn(app, "alice");
  await app.waitFor("id:chat-view");
  await app.type("id:composer", "what's running?");
  await askAgent(app);
  await app.waitFor("text:(agent) re: what's running?");
  expect(f.posts().find((p) => p.path === "/v1/chat/messages")?.body).toMatchObject({ text: "what's running?", invoke_agent: true });

  await app.waitFor("id:run:run1");
  expect(app.shows("text:Acme › #lobby")).toBe(true);
  expect(app.shows("id:run:run1 >> text:Started by you")).toBe(true); // alice started it
  expect(app.shows("id:dispatch")).toBe(false); // the task form stays folded
  await app.press("id:run-follow:run1"); // Follow up opens it, aimed at the run's room
  await app.waitFor("id:dispatch");
  await app.waitFor("text:To: agent in Acme › #lobby");
  await app.type("id:dispatch-text", "also check the tests");
  await app.press("id:dispatch-send");
  await app.waitFor("id:dispatch-notice");
  expect(f.posts().at(-1)).toMatchObject({ path: "/v1/spaces/acme/rooms/lobby/messages", body: { text: "also check the tests", invoke_agent: true } });

  await app.press("id:run-stop:run1");
  await app.waitFor("text:stopped");
  expect(f.posts().at(-1)).toMatchObject({ path: "/v1/runs/run1/stop", body: {} });

  await app.press("id:run-open:run1");
  await app.waitFor("text:hello from bob in acme");
});

test("Chat: New task asks for an explicit Space and room; closing the form keeps the draft", async () => {
  const f = world();
  const app = await openApp(f);
  await signIn(app, "alice");
  await app.waitFor("id:chat-view");
  expect(app.shows("id:dispatch")).toBe(false); // folded until asked for
  await app.press("id:dispatch-new");
  await app.waitFor("id:dispatch");
  await app.type("id:dispatch-text", "tidy the changelog");
  expect(app.enabled("id:dispatch-send")).toBe(false); // no implicit Space or room
  await app.press("id:dispatch-space:acme");
  await app.press("id:dispatch-room:lobby");
  await app.waitFor("text:To: agent in Acme › #lobby");

  const before = f.posts().length;
  await app.press("id:dispatch-close");
  await app.waitFor("id:dispatch", { state: "gone" });
  expect(f.posts().length).toBe(before); // closing sends nothing
  await app.press("id:dispatch-new");
  expect(app.value("id:dispatch-text")).toBe("tidy the changelog");
  expect(app.shows("text:To: agent in Acme › #lobby")).toBe(true);
  await app.press("id:dispatch-send");
  await app.waitFor("id:dispatch-notice");
  expect(f.posts().at(-1)).toMatchObject({ path: "/v1/spaces/acme/rooms/lobby/messages", body: { text: "tidy the changelog", invoke_agent: true } });
});

test("a rejected token returns to the connect screen with the reason", async () => {
  const f = world();
  const app = await openApp(f);
  await signIn(app, "nobody");
  await app.waitFor("id:connect-error");
  expect(text(app)).toContain("rejected the access token (HTTP 401)");
  expect(app.value("id:access-token")).toBe("");
});

test("unreachable server: clear error, then retry banner and recovery mid-session", async () => {
  const f = world();
  f.state.down = true;
  const app = await openApp(f);
  await signIn(app, "alice");
  await app.waitFor("id:connect-error");
  expect(text(app)).toContain(`Cannot reach ${BASE}`);

  f.state.down = false;
  await signIn(app, "alice");
  await openAcme(app);
  f.state.down = true;
  await app.waitFor("id:connection-banner", { timeout: 10_000 });
  expect(app.shows("text:hello from bob in acme")).toBe(true);
  f.state.down = false;
  f.post("acme/lobby", { kind: "message", actor: "bob", text: "back online" });
  await app.press("id:retry-now");
  await app.waitFor("text:back online", { timeout: 10_000 });
  await app.waitFor("id:connection-banner", { state: "gone" });
});

test("long tool output starts folded and expands on demand", async () => {
  const f = world();
  const long = Array.from({ length: 30 }, (_, i) => `line ${i + 1}`).join("\n");
  f.post("acme/lobby", { kind: "tool_result", actor: "runner", text: long });
  const app = await openApp(f);
  await signIn(app, "alice");
  await openAcme(app);
  const ev = f.state.events.get("acme/lobby")!.at(-1)!;
  // Uncorrelated (older) output: its own collapsed group, opened once.
  await app.waitFor(`id:activity-toggle:event:${ev.id}`);
  await app.press(`id:activity-toggle:event:${ev.id}`);
  await app.waitFor("text:Show all 30 lines");
  await app.press("text:Show all 30 lines");
  await app.waitFor("text:Show less");
});

// ------------------------------------------------------------------ Settings

const RUNNER_SECRET = "metro_rt_v2_secret";
const DEVICE_SECRET = "metro_pc_secret";

async function openSettings(app: App, section: "runners" | "providers" | "appearance") {
  if (!app.shows(`id:nav-settings:${section}`)) await app.press("id:nav-settings");
  await app.press(`id:nav-settings:${section}`);
  await app.waitFor(`id:${section}-settings`);
}

/** Add a connection: the provider from the drop-down, then one of its ways to connect. */
async function chooseProvider(app: App, vendor: string, id: string) {
  await app.press("id:provider-picker");
  await app.press(`id:provider-picker:${vendor}`);
  await app.waitFor(`id:provider-ways:${vendor}`);
  await app.press(`id:provider:${id}`);
}

test("Settings work without any Space; a personal runner's token is shown once, masked, with a Docker template that never holds it", async () => {
  const f = world();
  const app = await openApp(f);
  await signIn(app, "carol");
  await app.waitFor("id:onboarding");
  await openSettings(app, "runners");
  await app.waitFor("text:No runners yet");
  // Not an org admin: organization runners can't be chosen.
  expect(app.enabled("id:runner-visibility:organization")).toBe(false);

  await app.type("id:runner-name", "Carol laptop");
  await app.press("id:add-runner");
  await app.waitFor("id:token-reveal");
  expect(f.posts().at(-1)).toMatchObject({ method: "POST", path: "/v1/runners", body: { name: "Carol laptop", visibility: "personal" } });
  expect(text(app)).not.toContain(RUNNER_SECRET);
  // The prebuilt standalone runner comes first: this version's release files, a mode-600 token file, never the token.
  const run = app.texts().find((t) => t.startsWith("mkdir -p")) ?? "";
  expect(run).toContain("METRO_RUNNER_TOKEN_FILE=\"$PWD/runner.token\"");
  expect(run).toContain("METRO_WORKSPACE=\"$HOME/metro-runner-carol-laptop\"");
  expect(run).not.toContain(RUNNER_SECRET);
  expect(text(app)).toContain("Get the matching prebuilt runner from Metro Releases, or pull this version's image when available.");
  for (const banned of ["docker build", "cargo", "git clone", "isn't published"]) expect(text(app)).not.toContain(banned);
  // Docker is the alternative: env file, named volume, never the token.
  expect(app.shows("id:setup-docker")).toBe(false);
  await app.press("id:setup-docker-toggle");
  const docker = app.texts().find((t) => t.startsWith("docker run")) ?? "";
  expect(docker).toContain("--env-file ./runner.env");
  expect(docker).toContain("-v metro-runner-carol-laptop-workspace:/workspace");
  expect(docker).toContain("--network host"); // the server is on this machine
  expect(docker).not.toContain(RUNNER_SECRET);
  expect(text(app)).toContain("METRO_RUNNER_TOKEN=<paste the runner token here>");
  await app.press("id:setup-docker-copy");
  expect(app.clipboard()).not.toContain(RUNNER_SECRET);
  await app.press("id:copy-runner-token");
  expect(app.clipboard()).toContain(RUNNER_SECRET);
  await app.press("id:reveal-toggle");
  expect(text(app)).toContain(RUNNER_SECRET);

  // Closing Settings forgets it; the runner is listed without it.
  await app.press("id:settings-close");
  await openSettings(app, "runners");
  await app.waitFor("text:Carol laptop");
  expect(text(app)).not.toContain(RUNNER_SECRET);
  expect(JSON.stringify(app.storage())).not.toContain(RUNNER_SECRET);
});

test("org admins add organization runners; others see them but can't manage them; rotate and delete say which Spaces they affect", async () => {
  const f = world();
  const app = await openApp(f);
  await signIn(app, "alice");
  await openSettings(app, "runners");
  await app.type("id:runner-name", "Shared CI");
  await app.press("id:runner-visibility:organization");
  await app.press("id:add-runner");
  await app.waitFor("id:token-reveal");
  await app.press("id:hide-runner-token");
  const org = [...f.state.runners.values()].find((r) => r.name === "Shared CI")!;
  expect(org.visibility).toBe("organization");
  f.state.attached.set("side", org.id); // Bob attached it to his Space

  await app.waitFor(`id:rotate-runner:${org.id}`);
  await app.press(`id:rotate-runner:${org.id}`);
  await app.waitFor("text:/Spaces you belong to that use it: Side project/");
  await app.press("id:confirm-rotate-runner");
  await app.waitFor("text:/New token for/");
  expect(text(app)).not.toContain(org.token);
  await app.press("id:hide-runner-token");

  await app.press(`id:delete-runner:${org.id}`);
  await app.waitFor("text:/Deleting revokes its token/");
  await app.press("id:confirm-delete-runner");
  await app.waitFor("text:/It no longer serves any Space/");
  expect(f.state.runners.has(org.id)).toBe(false);
  expect(f.state.attached.has("side")).toBe(false);

  // Bob sees an organization runner but has no rotate or delete for it.
  const other = f.addRunner("Org builder", "organization", "alice");
  const bob = await openApp(f);
  await signIn(bob, "bob");
  await openSettings(bob, "runners");
  await bob.waitFor("text:Org builder");
  expect(bob.shows(`id:rotate-runner:${other.id}`)).toBe(false);
  expect(bob.shows(`id:delete-runner:${other.id}`)).toBe(false);
  expect(bob.shows("text:Organization · managed by org admins. You can attach it to Spaces you own.")).toBe(true);
});

test("a Space owner attaches and detaches a runner from the same list; attached is not online", async () => {
  const f = world();
  const org = f.addRunner("Org builder", "organization", "alice");
  const mine = f.addRunner("Bob desktop", "personal", "bob");
  f.addRunner("Alice private", "personal", "alice"); // invisible to Bob
  const app = await openApp(f);
  await signIn(app, "bob");
  await app.press("id:space:side");
  await app.waitFor("text:side lobby only");
  await app.press("id:manage-space");
  await app.waitFor("id:attach-runner-list");
  expect(app.shows(`id:attachable:${org.id}`)).toBe(true);
  expect(app.shows(`id:attachable:${mine.id}`)).toBe(true);
  expect(app.shows("text:Alice private")).toBe(false);

  await app.press(`id:attach-runner:${mine.id}`);
  await app.waitFor("id:attached-runner");
  expect(f.posts().at(-1)).toMatchObject({ method: "PUT", path: "/v1/spaces/side/runner", body: { runner_id: mine.id } });
  // Attached only routes this Space's jobs; it never claims the runner is up.
  expect(text(app)).toMatch(/Attached or paired means [^|]*(it doesn't mean the runner is online|not that it[’']s online)/);
  expect(app.shows("id:runner-card >> text:Online")).toBe(false);
  expect(text(app)).not.toContain(mine.token);

  await app.press("id:detach-runner");
  await app.press("id:confirm-detach");
  await app.waitFor("id:attach-runner-list");
  expect(f.state.attached.has("side")).toBe(false);
  await app.press("id:open-runner-settings");
  await app.waitFor("id:runners-settings");
});

test("Model providers come from the server's catalog: an API profile with a key, Save and fetch models, then pick one", async () => {
  const f = world();
  const app = await openApp(f);
  await signIn(app, "alice");
  await openSettings(app, "providers");
  await app.waitFor("id:provider-picker");
  // No wall of provider cards: one compact drop-down until a provider is chosen.
  expect(app.shows("id:provider:openai")).toBe(false);
  expect(app.shows("id:provider:device-openai-oauth")).toBe(false);

  await chooseProvider(app, "openai", "openai");
  await app.waitFor("id:server-form");
  expect(app.shows("id:provider-choice >> text:API key")).toBe(true);
  expect(app.shows("id:provider-choice >> text:Only you, until you share it")).toBe(true);
  await app.press("id:provider-save-fetch");
  await app.waitFor("id:provider-form-error"); // a key is required
  await app.type("id:provider-key", "sk-test-not-real");
  await app.press("id:provider-save-fetch");
  const c = await waitConnection(app, f, "OpenAI API");
  expect(f.posts().find((p) => p.path === "/v1/providers")?.body).toMatchObject({ provider_id: "openai", api_key: "sk-test-not-real" });
  await app.waitFor(`id:model:${c.id}:gpt-fixture-2`);
  await app.press(`id:model:${c.id}:gpt-fixture-2`);
  await app.waitUntil(() => f.state.connections.get(c.id)!.model === "gpt-fixture-2");
  expect(text(app)).not.toContain("sk-test-not-real"); // never read back
  expect(app.shows(`id:connection-default:${c.id}`)).toBe(true);
});

async function waitConnection(app: App, f: ReturnType<typeof fakeServer>, name: string) {
  await app.waitUntil(() => [...f.state.connections.values()].some((c) => c.name === name));
  const c = [...f.state.connections.values()].find((x) => x.name === name)!;
  await app.waitFor(`id:connection:${c.id}`);
  return c;
}

test("custom endpoints must be approved exactly; an endpoint that can't list models asks for a model ID", async () => {
  const f = world();
  const app = await openApp(f);
  await signIn(app, "alice");
  await openSettings(app, "providers");
  await chooseProvider(app, "custom", "custom");
  await app.type("id:provider-base-url", "http://127.0.0.1:11434/v1/other");
  const before = f.posts().length;
  await app.press("id:provider-save-fetch");
  await app.waitFor("text:/isn't on the server's approved list/");
  expect(f.posts().length).toBe(before); // refused before any request

  await app.clear("id:provider-base-url");
  await app.press("id:provider-url:http://127.0.0.1:11434/v1");
  await app.press("id:provider-save-fetch");
  const c = await waitConnection(app, f, "Custom OpenAI-compatible endpoint");
  await app.waitFor(`id:listing-error:${c.id}`);
  expect(text(app)).toContain("enter the model ID by hand");
  await app.type(`id:connection-model-input:${c.id}`, "llama-local");
  await app.press(`id:connection-model-save:${c.id}`);
  await app.waitUntil(() => f.state.connections.get(c.id)!.model === "llama-local");
});

test("a device connection: no vendor key in Metro, a masked device token with metro-connector steps, offline then online", async () => {
  const f = world();
  const app = await openApp(f);
  await signIn(app, "bob");
  await openSettings(app, "providers");
  await chooseProvider(app, "custom", "device-openai-compatible");
  await app.waitFor("id:device-form");
  expect(app.shows("id:provider-key")).toBe(false); // no key or password field for devices
  await app.clear("id:provider-name");
  await app.type("id:provider-name", "Home GPU");
  await app.press("id:provider-create-device");
  await app.waitFor("id:token-reveal");
  const c = [...f.state.connections.values()].find((x) => x.name === "Home GPU")!;
  expect(f.posts().map((p) => p.path)).toContain(`/v1/providers/${c.id}/device-token`);
  expect(text(app)).not.toContain(DEVICE_SECRET);
  expect(text(app)).toContain(`metro-connector setup --profile home-gpu --server-url ${BASE} --kind device-openai-compatible`);
  expect(text(app)).toContain("metro-connector run --profile home-gpu");
  expect(text(app)).toContain("1. Run setup.");
  expect(text(app)).not.toContain("cargo");

  await app.waitFor(`id:device-status:${c.id} >> text:Paired · offline`);
  await app.press("id:reveal-test-connection");
  await app.waitFor(`id:listing-error:${c.id}`);
  expect(text(app)).toContain("metro-connector for this connection didn't answer");
  f.state.online.add(c.id);
  await app.press("id:reveal-test-connection");
  await app.waitFor(`id:listing-ok:${c.id}`);
  await app.waitFor(`id:device-status:${c.id} >> text:Online`);
  await app.press("id:hide-runner-token");
  await app.waitFor(`id:connection:${c.id}`);
  expect(app.shows(`id:connection:${c.id} >> text:Online`)).toBe(true);
  expect(text(app)).not.toContain(DEVICE_SECRET);
  await app.press(`id:model:${c.id}:fixture-local-large`);
  await app.waitUntil(() => f.state.connections.get(c.id)!.model === "fixture-local-large");

  // Unpairing asks first and leaves the connection (and Metro sign-in) in place.
  await app.press(`id:connection-unpair:${c.id}`);
  await app.waitFor(`id:connection-confirm-unpair:${c.id}`);
  await app.press(`id:connection-confirm-unpair-now:${c.id}`);
  await app.waitFor(`id:device-status:${c.id} >> text:Not paired`);
  expect(f.state.connections.get(c.id)!.deviceToken).toBe(null);
  expect(app.shows("id:workspace")).toBe(true);

  // A new device token pairs it again; delete asks first.
  await app.press(`id:connection-token:${c.id}`);
  await app.waitFor("text:/New device token for/");
  await app.press("id:settings-close"); // closing Settings wipes it
  await openSettings(app, "providers");
  expect(text(app)).not.toContain(DEVICE_SECRET);
  await app.press(`id:connection-delete:${c.id}`);
  await app.press(`id:connection-confirm-delete-now:${c.id}`);
  await app.waitFor(`id:connection:${c.id}`, { state: "gone" });
  expect(f.state.connections.has(c.id)).toBe(false);
});

test("Appearance is a Settings page", async () => {
  const f = world();
  const app = await openApp(f);
  await signIn(app, "carol");
  await openSettings(app, "appearance");
  expect(app.shows("id:appearance-settings")).toBe(true);
});

test("the footer says On this device only for the app-hosted workspace, never from a loopback URL", () => {
  // A loopback URL may be a port forward to another computer: it shows as itself.
  expect(serverPlace("http://127.0.0.1:8787", false)).toEqual({ local: false, title: "127.0.0.1:8787", detail: "http://127.0.0.1:8787" });
  expect(serverPlace("http://localhost:9000", false).title).toBe("localhost:9000");
  expect(serverPlace("https://metro.example.org", false).title).toBe("metro.example.org");
  expect(serverPlace("http://127.0.0.1:41234", true)).toEqual({ local: true, title: "On this device", detail: "http://127.0.0.1:41234" });
  expect(serverPlace(null, false).title).toBe("Not connected");
});

test("a manually connected loopback server shows its actual host once; the account menu copies its exact origin and my ID", async () => {
  const f = world();
  const app = await openApp(f);
  await signIn(app, "alice");
  await app.waitFor("id:account-menu");
  // Compact footer: the host it connected to, once, and never "On this device".
  expect(app.count("id:active-server >> text:127.0.0.1:8787")).toBe(1);
  expect(app.shows("label:/connected to 127\\.0\\.0\\.1:8787/")).toBe(true);
  expect(text(app)).not.toContain("On this device");
  expect(text(app)).not.toContain(TOKENS.alice);

  const before = f.posts().length;
  await app.press("id:account-menu");
  await app.press("id:copy-server-url");
  expect(app.clipboard()).toBe(BASE); // the exact origin, not the host label
  await app.press("id:account-menu");
  await app.press("id:copy-id");
  expect(app.clipboard()).toBe("alice"); // the stable id, never a token
  expect(f.posts().length).toBe(before); // copying sends nothing

  await signOut(app);
  await app.waitFor("id:connect-screen");
  expect(JSON.stringify(app.storage())).not.toContain(TOKENS.alice);
});

test("Settings opens as a modal over a room and closes back to the same room and draft", async () => {
  const f = world();
  const app = await openApp(f);
  await signIn(app, "alice");
  await openAcme(app);
  await app.type("id:composer", "half-written thought");
  // The sidebar has one Settings entry; its sections live only in the modal.
  expect(app.shows("id:nav-settings:runners")).toBe(false);
  await app.press("id:nav-settings");
  await app.waitFor("id:settings-dialog");
  expect(app.count("id:nav-settings:providers")).toBe(1);
  await app.press("id:nav-settings:profile");
  await app.waitFor("id:profile-settings");
  await app.press("id:nav-settings:providers");
  await app.waitFor("id:providers-settings");

  // Escape (back) closes it: the same Space, room and draft, nothing sent.
  const before = f.posts().length;
  await app.back();
  await app.waitFor("id:settings-dialog", { state: "gone" });
  expect(app.shows("text:# lobby")).toBe(true);
  expect(app.value("id:composer")).toBe("half-written thought");
  expect(f.posts().length).toBe(before);

  // It reopens where it was; Close returns again.
  await app.press("id:nav-settings");
  await app.waitFor("id:providers-settings");
  await app.press("id:settings-close");
  await app.waitFor("id:settings-dialog", { state: "gone" });
  expect(app.value("id:composer")).toBe("half-written thought");
  expect(app.shows("text:hello from bob in acme")).toBe(true);
});

test("Settings opened from a Space's management returns to it; a runner token shown in Settings is gone after closing", async () => {
  const f = world();
  const app = await openApp(f);
  await signIn(app, "bob");
  await app.press("id:space:side");
  await app.waitFor("text:side lobby only");
  await app.press("id:manage-space");
  await app.waitFor("id:attach-runner-list");
  await app.press("id:open-runner-settings");
  await app.waitFor("id:runners-settings");
  await app.type("id:runner-name", "Bob laptop");
  await app.press("id:add-runner");
  await app.waitFor("id:token-reveal");
  await app.press("id:settings-close");
  await app.waitFor("id:attach-runner-list"); // still managing Side
  await app.press("id:nav-settings");
  await app.waitFor("id:runners-settings");
  expect(app.shows("id:token-reveal")).toBe(false);
  expect(text(app)).not.toContain(RUNNER_SECRET);
});

test("the provider picker is one compact drop-down; OpenAI and Anthropic each group their ways to connect", async () => {
  const f = world();
  const app = await openApp(f);
  await signIn(app, "alice");
  await openSettings(app, "providers");
  await app.press("id:provider-picker");
  await app.waitFor("id:provider-picker:openai");
  expect(app.count("id:provider-picker:openai")).toBe(1); // ChatGPT sign-in, API key and embeddings: one entry
  expect(app.shows("id:provider-picker:anthropic")).toBe(true);
  expect(app.shows("id:provider-picker:custom")).toBe(true);
  expect(app.shows("id:provider-picker:device-openai-oauth")).toBe(false);
  // Bundled marks for named providers; a lettermark for custom and self-hosted endpoints.
  expect(app.shows("id:provider-picker:openai >> id:mark-logo:openai")).toBe(true);
  expect(app.shows("id:provider-picker:anthropic >> id:mark-logo:anthropic")).toBe(true);
  expect(app.shows("id:provider-picker:custom >> id:mark-letters:{}")).toBe(true);
  expect(app.shows("id:provider-picker:custom >> id:mark-logo:openai")).toBe(false);
  await app.press("id:provider-picker:openai");
  await app.waitFor("id:provider-ways:openai");
  expect(app.shows("id:provider:device-openai-oauth")).toBe(true);
  expect(app.shows("id:provider:openai")).toBe(true);
  expect(app.shows("id:provider:openai-embeddings")).toBe(true);
  expect(app.shows("id:provider-way:device-openai-oauth >> text:Sign in with ChatGPT")).toBe(true);
  expect(app.shows("id:provider:anthropic")).toBe(false);

  await app.press("id:provider:device-openai-oauth");
  await app.waitFor("id:device-form");
  expect(app.shows("id:provider-choice >> text:On your device")).toBe(true);
  expect(app.shows("id:provider-choice >> text:Your own ChatGPT plan; can't be shared or lent")).toBe(true);
  expect(app.shows("id:provider-key")).toBe(false);

  // Another provider starts afresh: Anthropic's ways, no OpenAI form left behind.
  await app.press("id:provider-picker");
  await app.press("id:provider-picker:anthropic");
  await app.waitFor("id:provider-ways:anthropic");
  expect(app.shows("id:device-form")).toBe(false);
  expect(app.shows("id:provider:device-claude-code")).toBe(true);
  expect(app.shows("id:provider:anthropic")).toBe(true);
  expect(app.shows("id:provider:openai")).toBe(false);
});

test("saved connections are grouped by provider, two ways of one provider under one heading", async () => {
  const f = world();
  const api = f.addConnection("alice", "openai", "Work API", "gpt-fixture-2");
  const sub = f.addConnection("alice", "device-openai-oauth", "My ChatGPT", "gpt-sub");
  const emb = f.addConnection("alice", "custom-embeddings", "Home embeddings", "nomic-embed-text");
  const app = await openApp(f);
  await signIn(app, "alice");
  await openSettings(app, "providers");
  await app.waitFor("id:connection-group:openai");
  expect(app.shows(`id:connection-group:openai >> id:connection:${api.id}`)).toBe(true);
  expect(app.shows(`id:connection-group:openai >> id:connection:${sub.id}`)).toBe(true);
  expect(app.shows(`id:connection-group:custom >> id:connection:${emb.id}`)).toBe(true);
  expect(app.count("id:connection-group:openai >> id:mark-logo:openai")).toBe(1); // one heading for both ways
  expect(app.shows("id:connection-group:custom >> id:mark-letters:{}")).toBe(true);
  expect(app.shows(`id:connection:${api.id} >> text:/API key · gpt-fixture-2/`)).toBe(true);
  expect(app.shows(`id:connection:${sub.id} >> text:/Sign in with ChatGPT · gpt-sub/`)).toBe(true);
  // Compact until opened: management controls stay hidden.
  expect(app.shows(`id:connection-delete:${api.id}`)).toBe(false);
  await app.press(`id:connection-manage:${api.id}`);
  await app.waitFor(`id:connection-delete:${api.id}`);
  expect(text(app)).not.toContain("sk-fixture-not-real");
});

test("a late settings answer never lands after sign-out or in the next user's session", async () => {
  const f = world();
  f.addRunner("Alice private", "personal", "alice");
  f.state.delays.set("/v1/runners", 600);
  const app = await openApp(f);
  await signIn(app, "alice");
  await app.press("id:nav-settings");
  await app.press("id:settings-close");
  await signOut(app); // while GET /v1/runners is still on its way
  f.state.delays.delete("/v1/runners");
  await signIn(app, "bob");
  await openSettings(app, "runners");
  await app.waitFor("text:No runners yet");
  await app.wait(800);
  expect(app.shows("text:Alice private")).toBe(false);
});
