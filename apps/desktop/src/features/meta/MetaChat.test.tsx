import { expect, open, test } from "nsh/test";
import { BASE, TOKENS, world } from "../../../test/fakeServer";
import { launched } from "../../../test/launched";
import type { SpacesAccess } from "../../api/meta";

const path = "/v1/chat/spaces-access";
const ok = (body: unknown, status = 200) => ({ status, body });
const turn = (id: string, phase?: string) => ({ id, name: "Metro", source: "agent" as const, phase });
const liveId = "chat_meta";
const oldRun = {
  id: "run_failed", space_id: "acme", room_id: "lobby", status: "failed", actor: "alice", task: "Summarize this",
  kind: "room", agent_id: "agt_scout", trigger_event_id: 102, started_at: "2026-11-26T00:00:00Z",
};

function setup(initial?: Partial<SpacesAccess>) {
  const f = world();
  f.state.events.set("chat:alice", []);
  let access: SpacesAccess = { enabled: true, notice_shown: false, excluded_space_ids: [], ...initial };
  const saves: Partial<SpacesAccess>[] = [];
  const stopped: unknown[] = [], retried: unknown[] = [];
  const control = { stopDelay: 0, retryDelay: 0, refuseNotice: false };
  const addChat = (e: Parameters<typeof f.post>[1]) => f.post("chat:alice", e);
  for (const method of ["GET", "PUT", "POST", "DELETE"]) {
    const key = `${method} ${BASE}/v1/*`;
    const fallback = f.network[key]!;
    f.network[key] = req => {
      const p = new URL(req.url).pathname;
      const body = req.body ? JSON.parse(req.body) : undefined;
      if (p === path && method === "GET") return ok({
        ...access, excluded_space_ids: access.excluded_space_ids.filter(id => f.state.spaces.get(id)?.members.has("alice")),
      });
      if (p === path && method === "PUT") {
        saves.push(body);
        if (body.notice_shown && control.refuseNotice) return ok({ error: "try again" }, 503);
        if (body.excluded_space_ids?.some((id: string) => !f.state.spaces.get(id)?.members.has("alice"))) {
          return ok({ error: "not found" }, 404);
        }
        access = { ...access, ...body };
        return ok(access);
      }
      if (p === "/v1/pages/drafts/pgd_fixture" && method === "GET") return ok({
        id: "pgd_fixture", scope: "space", space_id: "acme", status: "generating",
        diagnostics: [], outside_selection: [], page_id: null, error: null,
      });
      if (p === "/v1/pages/drafts/pgd_fixture" && method === "DELETE") return ok({});
      if (p === `/v1/chat/turns/${liveId}/stop` && method === "POST") {
        stopped.push(body);
        addChat({ kind: "activity", actor: "agent", text: "Turn stopped.", activity: turn(liveId, "cancelled") });
        return { ...ok({ turn_id: liveId, status: "cancelled" }), delay: control.stopDelay };
      }
      if (p === "/v1/runs/run_failed/retry" && method === "POST") {
        retried.push(body);
        const next = { ...oldRun, id: "run_fresh", status: "queued" };
        f.state.runs.push(next);
        return { ...ok(next, 201), delay: control.retryDelay };
      }
      return fallback(req);
    };
  }
  return { ...f, saves, stopped, retried, addChat, control };
}

async function launch(f: ReturnType<typeof world>, width = 1200, clock = false) {
  const app = await open("../../../App.tsx", { modules: launched(), network: f.network as any, platform: "linux",
    width, height: 800, clock: clock ? {} : undefined });
  await app.clear("id:server-url");
  await app.type("id:server-url", BASE);
  await app.type("id:access-token", TOKENS.alice!);
  await app.press("id:connect");
  await app.waitFor("id:chat-view");
  return app;
}

test("Chat records the notice on render, without a click or changing another setting", async () => {
  const f = setup();
  const app = await launch(f);
  await app.waitFor("id:chat-spaces-notice");
  expect(app.shows("text:When you ask, Metro reads the Spaces you're in, as you, and can post for you, marked 'via Metro'. The Space content it reads goes to your Chat model connection. If someone shared that connection with you, they can see those requests.")).toBe(true);
  await app.waitUntil(() => f.saves.length === 1 && app.enabled("id:chat-spaces-dismiss"));
  expect(f.saves).toEqual([{ notice_shown: true }]);
  await app.press("id:chat-spaces-dismiss");
  await app.waitFor("id:chat-spaces-notice", { state: "gone" });
  await app.press("id:nav-pages");
  await app.press("id:nav-chat");
  await app.waitFor("id:chat-view");
  expect(app.shows("id:chat-spaces-notice")).toBe(false);
  expect(f.saves).toHaveLength(1);
});

test("Space access settings save switches and exclusions, but never acknowledge a notice behind Settings", async () => {
  const f = setup({ enabled: false });
  const app = await launch(f, 780);
  expect(app.shows("id:chat-spaces-notice")).toBe(false);
  await app.press("id:nav-settings");
  await app.press("id:nav-settings:metro-chat");
  await app.waitFor("id:metro-chat-space:acme");
  expect(app.enabled("id:metro-chat-space:acme")).toBe(false);
  expect(f.saves).toHaveLength(0);
  await app.press("id:metro-chat-enabled");
  await app.waitUntil(() => app.enabled("id:metro-chat-space:acme"));
  await app.press("id:metro-chat-space:acme");
  await app.waitUntil(() => f.saves.length === 2 && app.enabled("id:metro-chat-space:acme"));
  expect(app.shows("text:Saved")).toBe(true);
  await app.press("id:metro-chat-space:acme");
  await app.waitUntil(() => f.saves.length === 3 && app.enabled("id:metro-chat-space:acme"));
  expect(f.saves).toEqual([{ enabled: true }, { excluded_space_ids: ["acme"] }, { excluded_space_ids: [] }]);
  await app.press("id:settings-close");
  await app.waitFor("id:chat-spaces-notice");
  await app.waitUntil(() => app.enabled("id:chat-spaces-disable"));
  await app.press("id:chat-spaces-disable");
  await app.waitFor("id:chat-spaces-notice", { state: "gone" });
  expect(f.saves.slice(3)).toEqual([{ notice_shown: true }, { enabled: false }]);
});

test("an unacknowledged notice stays visible after a write refusal", async () => {
  const f = setup();
  f.control.refuseNotice = true;
  const app = await launch(f);
  await app.waitFor("id:chat-spaces-notice");
  await app.waitFor("id:chat-spaces-notice >> text:Retry");
  expect(app.enabled("id:chat-spaces-dismiss")).toBe(false);
  expect(f.saves).toEqual([{ notice_shown: true }]);
  f.control.refuseNotice = false;
  await app.press("id:chat-spaces-notice >> label:Retry");
  await app.waitUntil(() => app.enabled("id:chat-spaces-dismiss"));
  await app.press("id:chat-spaces-settings");
  await app.waitFor("id:metro-chat-settings");
  expect(f.saves).toEqual([{ notice_shown: true }, { notice_shown: true }]);
});

test("a refused exclusion does not replay or keep stale Space access in Settings", async () => {
  const f = setup({ notice_shown: true });
  const app = await launch(f, 780, true);
  await app.press("id:nav-settings");
  await app.press("id:nav-settings:metro-chat");
  await app.waitUntil(() => app.enabled("id:metro-chat-space:side"));
  f.state.spaces.get("side")!.members.delete("alice");
  await app.press("id:metro-chat-space:side");
  await app.waitFor("text:Your Space access changed. Nothing was saved.");
  await app.waitFor("id:metro-chat-space:side", { state: "gone" });
  expect(f.saves).toEqual([{ excluded_space_ids: ["side"] }]);
});

function postReceipt(f: ReturnType<typeof setup>, space = "acme") {
  const text = "The **exact message**.\n\nSecond paragraph.";
  f.addRoom(space, { id: "dispatch-thread", name: "Dispatch thread", parent_room_id: "lobby" });
  const destination = f.post(`${space}/dispatch-thread`, { kind: "message", actor: "alice", text });
  return f.addChat({ kind: "space_post", actor: "agent", text, activity: {
    ...turn(liveId), space_post: { space_id: space, space_name: space === "acme" ? "Acme" : "Side project",
      room_id: "dispatch-thread", room_name: "lobby", parent_room_id: "lobby", thread_name: "Dispatch thread",
      event_id: destination.id, agent: { id: "agt_scout", handle: "scout", name: "Scout" } },
  } as any });
}

test("a Space post starts collapsed and opens the exact destination thread", async () => {
  const f = setup({ notice_shown: true });
  const receipt = postReceipt(f);
  const app = await launch(f);
  await app.waitFor("text:Metro sent a message to Acme");
  expect(app.shows("id:chat-spaces-notice")).toBe(false);
  expect(app.shows(`id:space-post-message:${receipt.id}`)).toBe(false);
  await app.press(`id:space-post:${receipt.id}:toggle`);
  await app.waitFor("text:To: Acme › #lobby › Dispatch thread");
  expect(app.shows("text:Agent: @scout")).toBe(true);
  expect(app.shows(`id:space-post-message:${receipt.id}`)).toBe(true);
  await app.press(`id:space-post-open:${receipt.id}`);
  await app.waitFor("text:# Dispatch thread");
  await app.waitFor("text:Second paragraph.");
  await app.waitUntil(() => f.state.afters.some(p => p.includes("dispatch-thread")));
  expect(app.shows("id:chat-view")).toBe(false);
});

test("access loss disables a Space post's Open and keeps the Chat record", async () => {
  const f = setup({ notice_shown: true });
  const receipt = postReceipt(f, "side");
  f.state.spaces.get("side")!.members.delete("alice");
  const app = await launch(f);
  await app.waitFor(`id:space-post:${receipt.id}:toggle`);
  await app.press(`id:space-post:${receipt.id}:toggle`);
  await app.waitFor("text:You no longer have access");
  expect(app.enabled(`id:space-post-open:${receipt.id}`)).toBe(false);
  expect(app.shows(`id:space-post-message:${receipt.id}`)).toBe(true);
  expect(app.shows("id:chat-view")).toBe(true);
});

test("a Space post rechecks access before navigating and never opens a fallback room", async () => {
  const f = setup({ notice_shown: true });
  const receipt = postReceipt(f, "side");
  const app = await launch(f, 1200, true);
  await app.waitFor(`id:space-post:${receipt.id}:toggle`);
  await app.press(`id:space-post:${receipt.id}:toggle`);
  await app.waitUntil(() => app.enabled(`id:space-post-open:${receipt.id}`));
  f.state.spaces.get("side")!.members.delete("alice");
  await app.press(`id:space-post-open:${receipt.id}`);
  await app.waitFor("text:You no longer have access");
  expect(app.enabled(`id:space-post-open:${receipt.id}`)).toBe(false);
  expect(app.shows("id:chat-view")).toBe(true);
});

test("Stop replaces Send, leaves the draft intact, and uses the running Chat turn ID", async () => {
  const f = setup({ notice_shown: true });
  f.control.stopDelay = 500;
  f.addChat({ kind: "activity", actor: "agent", text: "Started", activity: turn(liveId, "started") });
  const app = await launch(f, 1200, true);
  await app.waitFor("label:Stop Metro's reply");
  expect(app.shows("id:send")).toBe(false);
  await app.type("id:composer", "Next message");
  await app.submit("id:composer");
  expect(f.posts().filter(p => p.path === "/v1/chat/messages")).toHaveLength(0);
  await app.press("id:chat-stop");
  await app.waitFor("text:Stopping…");
  expect(app.enabled("id:chat-stop")).toBe(false);
  await app.wait(600);
  await app.waitFor("id:send");
  expect(f.stopped).toEqual([{}]);
  expect(app.value("id:composer")).toBe("Next message");
});

test("Retry is for your retriable runs only, is busy once, and displays the new run", async () => {
  const f = setup({ notice_shown: true });
  f.control.retryDelay = 600;
  f.state.runs.push(oldRun, { ...oldRun, id: "run_other", actor: "bob" },
    { id: "run_legacy", space_id: "acme", room_id: "lobby", status: "cancelled", actor: "alice", task: "Earlier task" });
  const app = await launch(f, 1200, true);
  await app.waitFor("id:run-retry:run_failed");
  expect(app.shows("id:run-retry:run_other")).toBe(false);
  expect(app.shows("id:run-retry:run_legacy")).toBe(false);
  await app.press("id:run-retry:run_failed");
  await app.waitFor("text:Retrying…");
  expect(app.enabled("id:run-retry:run_failed")).toBe(false);
  expect(f.retried).toEqual([{}]);
  await app.wait(800);
  await app.waitFor("text:Retried as a new run");
  await app.waitFor("id:run:run_fresh");
  expect(app.shows("id:run:run_failed")).toBe(true);
});

test("meta tool results are concise, alongside legacy tool events and Chat draft cards", async () => {
  const f = setup({ notice_shown: true });
  const call = f.addChat({ kind: "tool_call", actor: "agent", text: "space_read",
    activity: { ...turn(liveId), meta: { tool: "space_read" } } as any });
  f.addChat({ kind: "tool_result", actor: "agent", text: "Read #lobby in Acme",
    activity: { ...turn(liveId), meta: { tool: "space_read", spaces: ["Acme"], count: 3 } } as any });
  const legacy = f.addChat({ kind: "tool_result", actor: "runner", text: "workspace.list ok\nlegacy output" });
  const draft = f.addChat({ kind: "page_draft", actor: "agent", text: "A Chat draft",
    activity: { ...turn(liveId), page_draft: { draft_id: "pgd_fixture", title: "Chat Page", summary: "A Chat draft",
      kind: "new", page_id: null, author_id: "alice", expires_at: "2099-01-01T00:00:00Z" } } as any });
  const app = await launch(f);
  await app.waitFor(`id:page-draft-card:${draft.id}`);
  expect(app.shows(`id:review-page-draft:${draft.id}`)).toBe(true);
  await app.press(`id:activity-toggle:agent:${liveId}`);
  await app.waitFor(`id:activity-meta:${call.id}`);
  expect(app.shows("text:Read #lobby in Acme")).toBe(true);
  await app.press(`id:activity-toggle:event:${legacy.id}`);
  await app.waitFor("text:legacy output");
  await app.press(`id:review-page-draft:${draft.id}`);
  await app.waitFor("id:page-draft-view");
  await app.waitFor("text:‹ Back to Chat");
  await app.waitFor("id:page-discard");
  await app.press("id:page-draft-back");
  await app.waitFor("id:chat-view");
  await app.press(`id:review-page-draft:${draft.id}`);
  await app.waitFor("id:page-discard");
  await app.press("id:page-discard");
  await app.press("id:page-discard-now");
  await app.waitFor("id:chat-view");
  await app.waitFor("text:This draft is no longer available.");
  expect(app.shows(`id:review-page-draft:${draft.id}`)).toBe(false);
});

test("older servers leave Chat usable with no notice and no Stop for legacy turn markers", async () => {
  const f = world();
  f.state.events.set("chat:alice", []);
  f.post("chat:alice", { kind: "activity", actor: "agent", text: "Legacy turn", activity: turn("chat_legacy", "started") });
  const app = await launch(f);
  await app.waitFor("text:Metro is thinking…");
  expect(app.shows("id:chat-spaces-notice")).toBe(false);
  expect(app.shows("id:chat-stop")).toBe(false);
  expect(app.shows("id:send")).toBe(true);
});

test("Stop works on a turn that hasn't started yet, by the id its POST returned", async () => {
  const f = setup({ notice_shown: true });
  const base = f.network[`POST ${BASE}/v1/*`]!;
  f.network[`POST ${BASE}/v1/*`] = req => {
    if (new URL(req.url).pathname !== "/v1/chat/messages") return base(req);
    const body = JSON.parse(req.body!);
    // Stored; the turn waits (routing or an earlier turn), no marker yet.
    const ev = f.addChat({ kind: "message", actor: "alice", actor_id: "alice", text: body.text, audience: "agent" } as any);
    return ok({ ...ev, actor_id: "alice", turn_id: liveId }, 202);
  };
  const app = await launch(f);
  await app.type("id:composer", "wait for it");
  await app.press("id:send");
  await app.waitFor("id:chat-stop");
  await app.press("id:chat-stop");
  await app.waitUntil(() => f.stopped.length === 1);
  await app.waitFor("id:chat-stop", { state: "gone" });
  expect(app.shows("id:send")).toBe(true);
});

function droppingServer(f: ReturnType<typeof setup>) {
  const base = f.network[`POST ${BASE}/v1/*`]!;
  f.network[`POST ${BASE}/v1/*`] = req => {
    if (new URL(req.url).pathname !== "/v1/chat/messages") return base(req);
    const body = JSON.parse(req.body!);
    const ev = f.addChat({ kind: "message", actor: "alice", actor_id: "alice", text: body.text, audience: "agent" } as any);
    return ok({ ...ev, actor_id: "alice", turn_id: liveId }, 202);
  };
}

test("a pending turn the server drops without a word stops offering Stop after two minutes", async () => {
  const f = setup({ notice_shown: true });
  droppingServer(f);
  const app = await launch(f, 1200, true);
  await app.type("id:composer", "dropped");
  await app.press("id:send");
  await app.waitFor("id:chat-stop");
  await app.clock!.runFor(60_000);
  expect(app.shows("id:chat-stop")).toBe(true);
  await app.clock!.runFor(61_000);
  await app.waitFor("id:chat-stop", { state: "gone" });
  expect(app.shows("id:send")).toBe(true);
});

test("a Chat error after a pending turn's message clears its Stop", async () => {
  const f = setup({ notice_shown: true });
  droppingServer(f);
  const app = await launch(f);
  await app.type("id:composer", "refused later");
  await app.press("id:send");
  await app.waitFor("id:chat-stop");
  f.addChat({ kind: "error", actor: "agent", text: "That turn was dropped." } as any);
  await app.waitFor("id:chat-stop", { state: "gone" });
  expect(app.shows("id:send")).toBe(true);
});
