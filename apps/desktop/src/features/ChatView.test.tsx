// The personal Chat's empty state and the runs aside: suggested drafts only
// fill an empty message box, run titles drop the @agent addressing, the task
// form stays folded behind "New task" and keeps its draft and target when
// folded. Real NSH engine, fake server.

import { expect, open, test } from "nsh/test";
import { BASE, TOKENS, world } from "../../test/fakeServer";
import { launched } from "../../test/launched";

type F = ReturnType<typeof world>;

async function inChat(f: F, width = 1200, user: "alice" | "carol" = "alice", useClock = false) {
  const app = await open("../../App.tsx", { modules: launched(), network: f.network as any, platform: "linux", width, height: 800, clock: useClock ? {} : undefined });
  await app.clear("id:server-url");
  await app.type("id:server-url", BASE);
  await app.type("id:access-token", TOKENS[user]!);
  await app.press("id:connect");
  await app.waitFor("id:chat-view");
  return app;
}

const chatPosts = (f: F) => f.posts().filter((p) => p.path.startsWith("/v1/chat/"));

test("an empty Chat suggests drafts that only fill an empty message box and never send", async () => {
  const f = world();
  const app = await inChat(f);
  await app.waitFor("id:chat-empty");
  expect(app.shows("text:Start a conversation")).toBe(true);

  await app.press("id:chat-suggestion:0");
  await app.waitUntil(() => app.value("id:composer") !== "");
  const filled = app.value("id:composer");
  expect(filled).toContain("plan my day");
  await app.wait(300);
  expect(chatPosts(f)).toHaveLength(0); // nothing was sent

  // What the user wrote is never replaced.
  await app.clear("id:composer");
  await app.type("id:composer", "my own words");
  expect(app.enabled("id:chat-suggestion:1")).toBe(false);
  await app.press("id:chat-suggestion:1").catch(() => {});
  expect(app.value("id:composer")).toBe("my own words");
  expect(chatPosts(f)).toHaveLength(0);

  // Sending is still the user's choice; then the transcript replaces the empty state.
  await app.press("id:send");
  await app.waitFor("text:my own words");
  expect(app.shows("id:chat-empty")).toBe(false);
});

test("run cards: titles without @agent, compact Open and Follow up, and Stop only while active", async () => {
  const f = world();
  f.state.runs.push(
    { id: "r-done", space_id: "acme", room_id: "lobby", status: "completed", actor: "alice", task: "@agent what is in the workspace?" },
    { id: "r-live", space_id: "acme", room_id: "lobby", status: "running", actor: "alice", task: "List the workspace and report the files." },
  );
  const app = await inChat(f);
  await app.waitFor("id:run:r-done");
  expect(app.shows("id:run:r-done >> text:What is in the workspace?")).toBe(true);
  expect(app.shows("id:run-title:r-done")).toBe(true);
  expect(app.shows("id:runs-aside >> text:@agent what is in the workspace?")).toBe(false);
  expect(app.shows("id:run-open:r-done")).toBe(true);
  expect(app.shows("id:run-follow:r-done")).toBe(true);
  expect(app.shows("id:run-stop:r-done")).toBe(false);
  expect(app.shows("id:run-stop:r-live")).toBe(true);
  expect(app.shows("label:Stop this run")).toBe(true);
});

test("the task form is folded behind New task, never covers the runs, and folding keeps the draft and target", async () => {
  const f = world();
  f.state.runs.push({ id: "run1", space_id: "acme", room_id: "lobby", status: "running", actor: "alice", task: "Audit the build" });
  const app = await inChat(f);
  await app.waitFor("id:run:run1");
  expect(app.shows("id:dispatch")).toBe(false);
  expect(app.shows("id:dispatch-new")).toBe(true);

  await app.press("id:dispatch-new");
  await app.waitFor("id:dispatch");
  expect(app.shows("id:dispatch-new")).toBe(false);
  expect(app.shows("id:run:run1")).toBe(true); // the list stays visible below the form
  await app.press("id:dispatch-space:acme");
  await app.waitFor("text:To: agent in Acme › #lobby");
  await app.type("id:dispatch-text", "half-written task");

  await app.press("id:dispatch-close");
  await app.waitFor("id:dispatch", { state: "gone" });
  await app.press("id:dispatch-new");
  await app.waitFor("id:dispatch");
  expect(app.value("id:dispatch-text")).toBe("half-written task");
  expect(app.shows("text:To: agent in Acme › #lobby")).toBe(true);
  expect(f.posts().some((p) => p.path.endsWith("/messages"))).toBe(false);
});

test("a leading slash is ordinary text in Dispatch", async () => {
  const f = world();
  f.state.runs.push({ id: "run1", space_id: "acme", room_id: "lobby", status: "completed", actor: "alice", task: "Audit the build" });
  const app = await inChat(f);
  await app.press("id:dispatch-new");
  await app.press("id:dispatch-space:acme");
  await app.waitFor("text:To: agent in Acme › #lobby");
  await app.type("id:dispatch-text", "/compact");
  expect(app.shows("id:command-suggestions")).toBe(false);
  await app.press("id:dispatch-send");
  await app.waitUntil(() => f.posts().some(p => p.path === "/v1/spaces/acme/rooms/lobby/messages" && p.body.text === "/compact"));
  expect(f.posts().some(p => p.path.endsWith("/compact"))).toBe(false);
});

test("Follow up opens the folded form, aimed at the run's room", async () => {
  const f = world();
  f.state.runs.push({ id: "run1", space_id: "acme", room_id: "lobby", status: "completed", actor: "alice", task: "Audit the build" });
  const app = await inChat(f);
  await app.waitFor("id:run:run1");
  await app.press("id:run-follow:run1");
  await app.waitFor("id:dispatch");
  await app.waitFor("text:To: agent in Acme › #lobby");
});

function lobbyRuns(f: F) {
  for (const space_id of ["acme", "side"]) {
    f.state.runs.push({ id: `run-${space_id}`, space_id, room_id: "lobby", status: "completed", actor: "alice", task: "Earlier task" });
  }
}

for (const width of [1200, 780]) {
  test(`Follow up across identical lobby names asks explicitly and restores keyboard focus at ${width}px`, async () => {
    const f = world();
    lobbyRuns(f);
    const app = await inChat(f, width);
    if (width === 780) await app.press("id:runs-toggle");
    await app.waitFor("id:run:run-acme >> label:Open Acme › #lobby");
    await app.waitFor("id:run:run-side >> label:Open Side project › #lobby");
    await app.press("label:Follow up in Acme › #lobby");
    await app.waitFor("text:To: agent in Acme › #lobby");
    const draft = "\u2003🌿 @bob\nDraft  \t";
    await app.paste("id:dispatch-text", { text: draft });
    await app.press("id:dispatch-close");
    const sideReads = () => app.requests().filter(r => r.method === "GET" && r.url.endsWith("/side/rooms")).length;
    const before = sideReads();
    await app.press("label:Follow up in Side project › #lobby");
    await app.waitFor("id:dispatch-move-confirm");
    expect(app.shows("id:dispatch-move-confirm >> text:/Follow up in Side project › #lobby instead/")).toBe(true);
    expect(app.shows("id:dispatch-move-confirm >> text:To: agent in Acme › #lobby")).toBe(true);
    expect(sideReads()).toBe(before); // Previewing the choice doesn't probe or move.
    await app.keyboard.press("enter"); // The initial focus is Keep draft, not Move.
    await app.waitFor("id:dispatch-move-confirm", { state: "gone" });
    expect(app.value("id:dispatch-text")).toBe(draft);
    expect(app.shows("text:To: agent in Acme › #lobby")).toBe(true);
    expect(sideReads()).toBe(before);

    await app.keyboard.press("enter"); // Focus returned to the correct Follow up card.
    await app.waitFor("id:dispatch-move-confirm");
    await app.keyboard.press("escape");
    await app.waitFor("id:dispatch-move-confirm", { state: "gone" });
    expect(app.shows("id:dispatch")).toBe(true); // Escape didn't close Agent runs.
    await app.keyboard.press("enter");
    await app.waitFor("id:dispatch-move-confirm");
    await app.keyboard.press("tab");
    await app.keyboard.press("enter");
    await app.waitFor("text:To: agent in Side project › #lobby");
    expect(sideReads()).toBe(before + 1);
    expect(app.value("id:dispatch-text")).toBe(draft);
    expect(sentAnything(f)).toBe(false);
    await app.press("id:dispatch-send");
    await app.waitFor("id:dispatch-notice");
    expect(f.posts().filter(p => p.path.endsWith("/messages"))).toMatchObject([
      { path: "/v1/spaces/side/rooms/lobby/messages", body: { text: draft, invoke_agent: true, infer_text_mentions: false } },
    ]);
  });
}

test("a held same-room Follow up keeps its recipient visible and can't repaint a newer confirmed move", async () => {
  const f = world();
  lobbyRuns(f);
  const get = `GET ${BASE}/v1/*`, base = f.network[get]!;
  let holdSame = false, holdMove = false;
  let sameRead = false, moveRead = false;
  f.network[get] = req => {
    const result = base(req);
    if (holdSame && req.url.endsWith("/acme/rooms")) {
      holdSame = false;
      sameRead = true;
      return { status: 200, body: { rooms: [{ id: "lobby", name: "Older label from held check" }] }, delay: 1_500 };
    }
    if (holdMove && req.url.endsWith("/side/rooms")) {
      holdMove = false;
      moveRead = true;
      return { ...(result as object), delay: 3_000 };
    }
    return result;
  };
  const app = await inChat(f, 1200, "alice", true);
  await app.waitFor("label:Open Side project › #lobby");
  await app.press("id:run-follow:run-acme");
  await app.waitFor("text:To: agent in Acme › #lobby");
  const draft = "\u2003🌿 @bob\nDraft  \t";
  await app.paste("id:dispatch-text", { text: draft });

  holdSame = true;
  await app.press("id:run-follow:run-acme");
  await app.waitUntil(() => sameRead);
  expect(app.shows("text:To: agent in Acme › #lobby")).toBe(true);
  expect(app.shows("text:Checking the selected destination…")).toBe(true);
  expect(app.shows("id:dispatch-move-confirm")).toBe(false);
  expect(app.enabled("id:dispatch-send")).toBe(false);

  await app.press("id:run-follow:run-side");
  await app.waitFor("id:dispatch-move-confirm");
  expect(app.shows("id:dispatch-move-confirm >> text:To: agent in Acme › #lobby")).toBe(true);
  await app.press("id:dispatch-keep-draft");
  expect(app.shows("text:Checking the selected destination…")).toBe(true);
  expect(app.enabled("id:dispatch-send")).toBe(false);

  holdMove = true;
  await app.press("id:run-follow:run-side");
  await app.waitFor("id:dispatch-move-confirm");
  await app.press("id:dispatch-move-draft");
  await app.waitUntil(() => moveRead);
  await app.clock!.runFor(1_600); // Release the older same-room read first.
  expect(app.shows("text:To: agent in Acme › #lobby")).toBe(true);
  expect(app.shows("text:/Older label from held check/")).toBe(false);
  expect(app.shows("text:Checking the new destination…")).toBe(true);
  expect(app.enabled("id:dispatch-send")).toBe(false);
  await app.clock!.runFor(1_500);
  await app.waitFor("text:To: agent in Side project › #lobby");
  expect(app.value("id:dispatch-text")).toBe(draft);
  expect(sentAnything(f)).toBe(false);
  await app.press("id:dispatch-send");
  await app.waitFor("id:dispatch-notice");
  expect(f.posts().filter(p => p.path.endsWith("/messages"))).toMatchObject([
    { path: "/v1/spaces/side/rooms/lobby/messages", body: { text: draft, invoke_agent: true, infer_text_mentions: false } },
  ]);
});

test("same-room Follow up keeps the draft but clears a room authoritatively missing from its recheck", async () => {
  const f = world();
  lobbyRuns(f);
  const get = `GET ${BASE}/v1/*`, base = f.network[get]!;
  let omitRoom = false;
  f.network[get] = req => omitRoom && req.url.endsWith("/acme/rooms")
    ? { status: 200, body: { rooms: [{ id: "backup", name: "Backup" }] } } : base(req);
  const app = await inChat(f, 1200, "alice", true);
  await app.waitFor("id:run-follow:run-acme");
  await app.press("id:run-follow:run-acme");
  await app.waitFor("text:To: agent in Acme › #lobby");
  await app.type("id:dispatch-text", "Original draft");
  omitRoom = true;
  await app.press("id:run-follow:run-acme");
  await app.waitFor("text:/That room or thread is no longer available/");
  expect(app.shows("text:Choose a Space and a room.")).toBe(true);
  expect(app.shows("id:dispatch-room:backup")).toBe(true);
  expect(app.value("id:dispatch-text")).toBe("Original draft");
  expect(app.enabled("id:dispatch-send")).toBe(false);
  expect(sentAnything(f)).toBe(false);
});

for (const order of ["move first", "purge first", "purge then explicit retarget", "purge then leave the view"] as const) {
  test(`a confirmed Follow up and an older Space denial settle independently: ${order}`, async () => {
    const f = world();
    lobbyRuns(f);
    f.addRoom("side", { id: "scratch", name: "Scratch" });
    const get = `GET ${BASE}/v1/*`, base = f.network[get]!;
    let deny = false, confirming = false, moveRead = false;
    f.network[get] = req => {
      if (deny && req.url.endsWith("/acme/rooms")) {
        return { status: 404, body: { error: "no such space" }, delay: 2_000 };
      }
      const result = base(req);
      if (confirming && req.url.endsWith("/side/rooms")) {
        moveRead = true;
        return { ...(result as object), delay: order === "move first" ? 0 : 4_000 };
      }
      return result;
    };
    const app = await inChat(f, 1200, "alice", true);
    await app.waitFor("label:Open Side project › #lobby");
    await app.press("id:run-follow:run-acme");
    await app.waitFor("text:To: agent in Acme › #lobby");
    const draft = "\u2003  🌿 @bob, résumé\nKeep my exact task  \t";
    await app.paste("id:dispatch-text", { text: draft });
    deny = true;
    f.state.spaces.get("acme")!.members.delete("alice");
    await app.press("id:run-follow:run-acme");
    await app.waitFor("text:Checking the selected destination…");
    await app.press("id:run-follow:run-side");
    await app.waitFor("id:dispatch-move-confirm");
    confirming = true;
    await app.press("id:dispatch-move-draft");
    await app.waitUntil(() => moveRead);
    if (order === "move first") await app.waitFor("text:To: agent in Side project › #lobby");

    await app.clock!.runFor(2_100);
    await app.waitFor("id:space:acme", { state: "gone" });
    if (order !== "move first") {
      expect(app.shows("text:Choose a Space and a room.")).toBe(true);
      expect(app.shows("text:Checking the new destination…")).toBe(true);
      expect(app.enabled("id:dispatch-send")).toBe(false);
    }
    const retarget = order === "purge then explicit retarget";
    if (retarget) {
      await app.press("id:dispatch-space:side");
      await app.press("id:dispatch-room:scratch");
      await app.waitFor("text:To: agent in Side project › #Scratch");
    }
    const left = order === "purge then leave the view";
    if (left) {
      await app.press("id:nav-settings");
      await app.waitFor("id:settings-dialog");
      await app.press("id:settings-close");
      await app.waitFor("id:settings-dialog", { state: "gone" });
    }
    await app.clock!.runFor(2_100);
    expect(app.value("id:dispatch-text")).toBe(draft);
    expect(sentAnything(f)).toBe(false);
    if (left) {
      expect(app.shows("text:Choose a Space and a room.")).toBe(true);
      expect(app.enabled("id:dispatch-send")).toBe(false);
      return;
    }
    await app.waitFor(`text:To: agent in Side project › #${retarget ? "Scratch" : "lobby"}`);
    expect(app.shows("id:dispatch-error")).toBe(false);
    await app.press("id:dispatch-send");
    await app.waitFor("id:dispatch-notice");
    expect(f.posts().filter(p => p.path.endsWith("/messages"))).toMatchObject([
      { path: `/v1/spaces/side/rooms/${retarget ? "scratch" : "lobby"}/messages`,
        body: { text: draft, invoke_agent: true, infer_text_mentions: false } },
    ]);
  });
}

for (const recovery of ["retry", "different Space"] as const) {
  test(`an inconclusive same-room recheck keeps Send blocked until ${recovery}`, async () => {
    const f = world();
    lobbyRuns(f);
    // Select a non-default original room: re-clicking its Space must not silently pick #lobby.
    f.addRoom("acme", { id: "releases", name: "Releases" });
    f.addRoom("side", { id: "releases", name: "Releases" });
    f.state.runs.push({ id: "releases", space_id: "acme", room_id: "releases", status: "completed", actor: "alice", task: "Earlier release task" });
    const get = `GET ${BASE}/v1/*`, base = f.network[get]!;
    let reply: "503" | "fresh" | undefined;
    f.network[get] = req => {
      if (!req.url.endsWith("/acme/rooms")) return base(req);
      if (reply === "503") return { status: 503, body: { error: "temporarily unavailable" }, delay: 500 };
      const result = base(req);
      return reply === "fresh" ? { ...(result as object), delay: 500 } : result;
    };
    const app = await inChat(f, 1200, "alice", true);
    await app.waitFor("label:Open Side project › #lobby");
    await app.press("id:run-follow:releases");
    await app.waitFor("text:To: agent in Acme › #Releases");
    const draft = "\u2003  🌿 @bob, résumé\nRetry without losing text  \t";
    await app.paste("id:dispatch-text", { text: draft });

    reply = "503";
    await app.press("id:run-follow:releases");
    await app.waitFor("text:Checking the selected destination…");
    expect(app.enabled("id:dispatch-send")).toBe(false);
    await app.clock!.runFor(600);
    await app.waitFor("text:/Follow up again to retry, or choose another room/");
    expect(app.shows("id:dispatch-verifying")).toBe(false);
    expect(app.shows("text:To: agent in Acme › #Releases")).toBe(true);
    await expect(app.press("id:dispatch-send")).rejects.toMatchObject({ code: "ERR_NSH_TEST_DISABLED" });

    await app.press("id:dispatch-close");
    await app.press("id:dispatch-new");
    await app.press("id:dispatch-space:acme");
    await app.press("id:dispatch-room:releases");
    expect(app.shows("text:To: agent in Acme › #Releases")).toBe(true);
    expect(app.shows("id:dispatch-retry-destination")).toBe(true);
    expect(app.enabled("id:dispatch-send")).toBe(false);
    expect(app.value("id:dispatch-text")).toBe(draft);
    expect(sentAnything(f)).toBe(false);

    if (recovery === "retry") {
      reply = "fresh";
      await app.press("id:run-follow:releases");
      await app.waitFor("text:Checking the selected destination…");
      expect(app.enabled("id:dispatch-send")).toBe(false);
      await app.clock!.runFor(600);
      await app.waitUntil(() => app.enabled("id:dispatch-send"));
      expect(app.shows("text:To: agent in Acme › #Releases")).toBe(true);
    } else {
      await app.press("id:dispatch-space:side");
      await app.press("id:dispatch-room:releases");
      await app.waitFor("text:To: agent in Side project › #Releases");
      expect(app.enabled("id:dispatch-send")).toBe(true);
      // The other destination is usable; returning to the failed scope from
      // the cache is not a successful exact verification there.
      await app.press("id:dispatch-space:acme");
      await app.press("id:dispatch-room:releases");
      expect(app.enabled("id:dispatch-send")).toBe(false);
      await app.press("id:dispatch-space:side");
      await app.press("id:dispatch-room:releases");
      await app.waitFor("text:To: agent in Side project › #Releases");
    }
    expect(app.shows("id:dispatch-retry-destination")).toBe(false);
    expect(app.value("id:dispatch-text")).toBe(draft);
    expect(sentAnything(f)).toBe(false);
    await app.press("id:dispatch-send");
    await app.waitFor("id:dispatch-notice");
    expect(f.posts().filter(p => p.path.endsWith("/messages"))).toMatchObject([
      { path: recovery === "retry" ? "/v1/spaces/acme/rooms/releases/messages" : "/v1/spaces/side/rooms/releases/messages",
        body: { text: draft, invoke_agent: true, infer_text_mentions: false } },
    ]);
  });
}

for (const flow of ["empty draft", "confirmed move"] as const) {
  test(`${flow} Follow up blocks a cached reselect while held and after failure, until fresh exact proof`, async () => {
    const f = world();
    lobbyRuns(f);
    f.addRoom("side", { id: "review", name: "Review" });
    f.addRoom("side", { id: "scratch", name: "Scratch" });
    f.state.runs.push({ id: "review", space_id: "side", room_id: "review", status: "completed", actor: "alice", task: "Prior review" });
    const get = `GET ${BASE}/v1/*`, base = f.network[get]!;
    let next: "fail" | "verify" | undefined;
    f.network[get] = req => {
      const result = base(req);
      if (!req.url.endsWith("/side/rooms") || !next) return result;
      const reply = next;
      next = undefined;
      return reply === "fail" ? { status: 503, body: { error: "temporarily unavailable" }, delay: 2_000 }
        : { ...(result as object), delay: 2_000 };
    };
    const app = await inChat(f, 1200, "alice", true);
    await app.waitFor("label:Open Side project › #Review");
    const draft = "\u2003  🌿 @bob, résumé\nKeep this exact draft  \t";
    if (flow === "confirmed move") {
      await app.press("id:run-follow:run-acme");
      await app.waitFor("text:To: agent in Acme › #lobby");
      await app.paste("id:dispatch-text", { text: draft });
    }

    next = "fail";
    await app.press("id:run-follow:review");
    if (flow === "confirmed move") await app.press("id:dispatch-move-draft");
    else await app.paste("id:dispatch-text", { text: draft });
    await app.waitUntil(() => next === undefined);
    await app.press("id:dispatch-space:side");
    await app.press("id:dispatch-room:review");
    expect(app.shows("text:To: agent in Side project › #Review")).toBe(true);
    expect(app.shows("text:Checking the selected destination…")).toBe(true);
    expect(app.enabled("id:dispatch-send")).toBe(false);

    // Deliberately choosing a different room in the same Space is allowed;
    // returning to the implicated cached room must still be blocked.
    await app.press("id:dispatch-room:scratch");
    expect(app.shows("text:To: agent in Side project › #Scratch")).toBe(true);
    expect(app.enabled("id:dispatch-send")).toBe(true);
    await app.press("id:dispatch-room:review");
    await app.press("id:dispatch-close");
    await app.press("id:dispatch-new");
    expect(app.enabled("id:dispatch-send")).toBe(false);
    await app.clock!.runFor(2_100);
    await app.waitFor("id:dispatch-retry-destination");
    expect(app.shows("id:dispatch-verifying")).toBe(false);
    expect(app.shows("text:To: agent in Side project › #Review")).toBe(true);
    await expect(app.press("id:dispatch-send")).rejects.toMatchObject({ code: "ERR_NSH_TEST_DISABLED" });
    expect(app.value("id:dispatch-text")).toBe(draft);
    expect(sentAnything(f)).toBe(false);

    await app.press("id:dispatch-room:scratch");
    next = "verify";
    await app.press("id:run-follow:review");
    await app.press("id:dispatch-move-draft");
    await app.waitUntil(() => next === undefined);
    await app.press("id:dispatch-room:review"); // Retires the move intent, not its exact room check.
    expect(app.shows("text:Checking the selected destination…")).toBe(true);
    expect(app.enabled("id:dispatch-send")).toBe(false);
    await app.clock!.runFor(2_100);
    await app.waitUntil(() => app.enabled("id:dispatch-send"));
    expect(app.shows("id:dispatch-retry-destination")).toBe(false);
    expect(app.value("id:dispatch-text")).toBe(draft);
    expect(sentAnything(f)).toBe(false);
    await app.press("id:dispatch-send");
    await app.waitFor("id:dispatch-notice");
    expect(f.posts().filter(p => p.path.endsWith("/messages"))).toMatchObject([
      { path: "/v1/spaces/side/rooms/review/messages", body: { text: draft, invoke_agent: true, infer_text_mentions: false } },
    ]);
  });
}

test("an older positive Follow up can't release a newer check for the same cached room", async () => {
  const f = world();
  lobbyRuns(f);
  const get = `GET ${BASE}/v1/*`, base = f.network[get]!;
  let next: "older success" | "newer failure" | undefined;
  f.network[get] = req => {
    const result = base(req);
    if (!req.url.endsWith("/side/rooms") || !next) return result;
    const reply = next;
    next = undefined;
    return reply === "older success" ? { ...(result as object), delay: 1_000 }
      : { status: 503, body: { error: "temporarily unavailable" }, delay: 2_000 };
  };
  const app = await inChat(f, 1200, "alice", true);
  await app.waitFor("label:Open Side project › #lobby");
  next = "older success";
  await app.press("id:run-follow:run-side");
  await app.waitUntil(() => next === undefined);
  await app.type("id:dispatch-text", "This draft needs current proof");
  await app.press("id:dispatch-room:lobby");
  next = "newer failure";
  await app.press("id:run-follow:run-side");
  await app.waitUntil(() => next === undefined);
  await app.clock!.runFor(1_100);
  expect(app.shows("text:Checking the selected destination…")).toBe(true);
  expect(app.enabled("id:dispatch-send")).toBe(false);
  await app.clock!.runFor(1_000);
  await app.waitFor("id:dispatch-retry-destination");
  expect(app.shows("text:To: agent in Side project › #lobby")).toBe(true);
  expect(app.enabled("id:dispatch-send")).toBe(false);
  expect(sentAnything(f)).toBe(false);
});

test("a failed name lookup stays blocked when a different room's exact check fills the cache", async () => {
  const f = world();
  lobbyRuns(f);
  f.state.runs.push({ id: "review", space_id: "side", room_id: "review", status: "completed", actor: "alice", task: "Prior review" });
  const get = `GET ${BASE}/v1/*`, base = f.network[get]!;
  let failLookup = false;
  f.network[get] = req => {
    if (failLookup && req.url.endsWith("/side/rooms")) {
      failLookup = false;
      return { status: 503, body: { error: "temporarily unavailable" } };
    }
    return base(req);
  };
  const app = await inChat(f, 1200, "alice", true);
  await app.waitFor("label:Open Side project › #lobby");
  await app.press("id:run-follow:run-acme");
  await app.waitFor("text:To: agent in Acme › #lobby");
  const draft = "🌿 @bob, résumé\nUse the chosen room  \t";
  await app.paste("id:dispatch-text", { text: draft });
  f.addRoom("side", { id: "review", name: "Review", parent_room_id: "lobby" });
  failLookup = true;
  await app.press("id:run-follow:review");
  await app.waitFor("id:dispatch-error");
  expect(app.shows("id:dispatch-move-confirm")).toBe(false);
  expect(app.shows("text:To: agent in Acme › #lobby")).toBe(true);

  await app.press("id:run-follow:run-side");
  await app.press("id:dispatch-move-draft");
  await app.waitFor("text:To: agent in Side project › #lobby");
  await app.press("id:dispatch-room:review");
  expect(app.shows("text:To: agent in Side project › #Review")).toBe(true);
  expect(app.shows("id:dispatch-retry-destination")).toBe(true);
  expect(app.enabled("id:dispatch-send")).toBe(false);
  await app.press("id:dispatch-room:lobby");
  expect(app.enabled("id:dispatch-send")).toBe(true);
  expect(app.value("id:dispatch-text")).toBe(draft);
  expect(sentAnything(f)).toBe(false);
  await app.press("id:dispatch-send");
  await app.waitFor("id:dispatch-notice");
  expect(f.posts().filter(p => p.path.endsWith("/messages"))).toMatchObject([
    { path: "/v1/spaces/side/rooms/lobby/messages", body: { text: draft, invoke_agent: true, infer_text_mentions: false } },
  ]);
});

test("Follow up resolves an uncached thread in a cached Space before asking and checks it again on consent", async () => {
  const f = world();
  lobbyRuns(f);
  const thread = "th_opaque_fixture_291", roomName = "Release review";
  f.state.runs.push({ id: "new-thread", space_id: "acme", room_id: thread, status: "completed", actor: "alice", task: "Earlier thread task" });
  const get = `GET ${BASE}/v1/*`, base = f.network[get]!;
  let held = false, holdNext = false;
  f.network[get] = req => {
    const result = base(req);
    if (holdNext && req.url.endsWith("/acme/rooms")) {
      holdNext = false;
      held = true;
      return { ...(result as object), delay: 1_500 };
    }
    return result;
  };
  const app = await inChat(f, 1200, "alice", true);
  await app.waitFor("label:Open Side project › #lobby"); // Let both Spaces' initial room reads finish.
  await app.press("id:run-follow:run-acme");
  await app.waitFor("text:To: agent in Acme › #lobby");
  await app.type("id:dispatch-text", "Preserve this draft");
  expect(app.shows("id:run:new-thread >> label:Open run destination in Acme")).toBe(true);
  expect(app.shows("id:run:new-thread >> label:Look up the follow-up destination in Acme")).toBe(true);
  expect(app.texts().join(" ")).not.toContain(thread);
  const roomReads = () => app.requests().filter(r => r.method === "GET" && new URL(r.url).pathname.endsWith("/rooms"));
  const before = roomReads().length;

  f.addRoom("acme", { id: thread, name: roomName, parent_room_id: "lobby" });
  holdNext = true;
  await app.press("id:run-follow:new-thread");
  await app.waitUntil(() => held);
  expect(app.shows("text:Looking up the room or thread…")).toBe(true);
  expect(app.shows("id:dispatch-move-confirm")).toBe(false);
  expect(app.shows("text:To: agent in Acme › #lobby")).toBe(true);
  await app.clock!.runFor(1_600);
  await app.waitFor("id:dispatch-move-confirm");
  expect(app.shows("id:run:new-thread >> label:Open Acme › #Release review")).toBe(true);
  expect(app.shows("id:run:new-thread >> label:Follow up in Acme › #Release review")).toBe(true);
  expect(app.shows("id:dispatch-move-confirm >> text:/Follow up in Acme › #Release review instead/")).toBe(true);
  expect(app.texts().join(" ")).not.toContain(thread);

  f.state.rooms.set("acme", f.state.rooms.get("acme")!.filter(r => r.id !== thread));
  await app.press("id:dispatch-move-draft");
  await app.waitFor("text:/That room or thread is no longer available/");
  expect(roomReads().slice(before).map(r => new URL(r.url).pathname)).toEqual([
    "/v1/spaces/acme/rooms", "/v1/spaces/acme/rooms",
  ]);
  expect(app.shows("text:To: agent in Acme › #lobby")).toBe(true);
  expect(app.value("id:dispatch-text")).toBe("Preserve this draft");
  expect(sentAnything(f)).toBe(false);
});

test("a held confirmed Follow up can't move a draft edited away and back; an omitted room doesn't replace its target", async () => {
  const f = world();
  lobbyRuns(f);
  const get = `GET ${BASE}/v1/*`, base = f.network[get]!;
  let scenario: "hold" | "omit" | undefined;
  let held = false;
  f.network[get] = req => {
    const result = base(req);
    if (!req.url.endsWith("/side/rooms")) return result;
    if (scenario === "hold") {
      held = true;
      return { ...(result as object), delay: 3_000 };
    }
    if (scenario === "omit") return { status: 200, body: { rooms: [] } };
    return result;
  };
  const app = await inChat(f, 1200, "alice", true);
  await app.waitFor("id:run-follow:run-acme");
  await app.press("id:run-follow:run-acme");
  await app.waitFor("text:To: agent in Acme › #lobby");
  const draft = "same words, new intent";
  await app.paste("id:dispatch-text", { text: draft });
  scenario = "hold";
  await app.press("id:run-follow:run-side");
  await app.press("id:dispatch-move-draft");
  await app.waitUntil(() => held);
  expect(app.shows("id:dispatch-verifying")).toBe(true);
  expect(app.shows("text:To: agent in Acme › #lobby")).toBe(true);
  expect(app.enabled("id:dispatch-send")).toBe(false);
  await app.clear("id:dispatch-text");
  await app.paste("id:dispatch-text", { text: draft });
  await app.clock!.runFor(3_100);
  expect(app.shows("text:To: agent in Acme › #lobby")).toBe(true);
  expect(app.value("id:dispatch-text")).toBe(draft);
  expect(app.shows("id:dispatch-verifying")).toBe(false);

  scenario = "omit";
  await app.press("id:run-follow:run-side");
  await app.press("id:dispatch-move-draft");
  await app.waitFor("text:/That room or thread is no longer available/");
  expect(app.shows("text:To: agent in Acme › #lobby")).toBe(true);
  expect(app.value("id:dispatch-text")).toBe(draft);
  expect(sentAnything(f)).toBe(false);
});

test("Follow up discards an unsubmitted move choice if its Space access is lost", async () => {
  const f = world();
  lobbyRuns(f);
  const app = await inChat(f, 1200, "alice", true);
  await app.press("id:run-follow:run-acme");
  await app.waitFor("text:To: agent in Acme › #lobby");
  await app.type("id:dispatch-text", "Keep this in Acme");
  await app.press("id:run-follow:run-side");
  await app.waitFor("id:dispatch-move-confirm");
  f.state.spaces.get("side")!.members.delete("alice");
  await app.clock!.runFor(12_000);
  await app.waitFor("id:dispatch-move-confirm", { state: "gone" });
  expect(app.shows("id:run-follow:run-side")).toBe(false);
  expect(app.shows("text:To: agent in Acme › #lobby")).toBe(true);
  expect(app.value("id:dispatch-text")).toBe("Keep this in Acme");
  expect(sentAnything(f)).toBe(false);
});

test("Dispatch sends exact Unicode and whitespace to the explicit thread, never replays 422 or uncertain sends", async () => {
  const f = world();
  f.addRoom("acme", { id: "t1", name: "Thread", parent_room_id: "lobby" });
  f.state.runs.push({ id: "held", space_id: "acme", room_id: "t1", status: "completed", actor: "alice", task: "Earlier task" });
  const key = `POST ${BASE}/v1/*`;
  const post = f.network[key]!;
  const path = "/v1/spaces/acme/rooms/t1/messages";
  let status = 422;
  f.network[key] = req => new URL(req.url).pathname === path && status
    ? { status, body: { error: "not supported" } } : post(req);
  const app = await inChat(f);
  await app.waitFor("id:run-follow:held");
  await app.press("id:run-follow:held");
  await app.waitFor("text:To: agent in Acme › #Thread");
  const text = "\u2003  🌿 @bob, résumé\ncheck the task  \t";
  await app.paste("id:dispatch-text", { text });
  const posts = () => app.requests().filter(r => r.method === "POST" && r.url.endsWith("/messages"));
  const body = { text, invoke_agent: true, infer_text_mentions: false };
  for (const [i, code] of [422, 503, 0].entries()) {
    status = code;
    await app.press("id:dispatch-send");
    if (code) {
      await app.waitFor(code === 422 ? "text:/Update Metro Server/" : "text:/Check the conversation before sending again/");
      expect(app.value("id:dispatch-text")).toBe(text);
    } else await app.waitFor("id:dispatch-notice");
    expect(posts()).toHaveLength(i + 1);
    expect(new URL(posts()[i].url).pathname).toBe(path);
    expect(JSON.parse(posts()[i].body!)).toEqual(body);
  }
});

test("Follow up with a missing explicit thread stays unselected and never falls back to the lobby", async () => {
  const f = world();
  f.state.runs.push({ id: "missing", space_id: "acme", room_id: "missing", status: "completed", actor: "alice", task: "Earlier task" });
  const app = await inChat(f);
  await app.waitFor("id:run-follow:missing");
  await app.press("id:run-follow:missing");
  await app.waitFor("id:dispatch-error");
  await app.type("id:dispatch-text", "keep this draft");
  expect(app.shows("text:Choose a Space and a room.")).toBe(true);
  expect(app.enabled("id:dispatch-send")).toBe(false);
  expect(f.posts().some(p => p.path.endsWith("/messages"))).toBe(false);
});

for (const width of [780, 860, 960]) {
  test(`at ${width} wide the runs open in a dialog over a full-width Chat; a task sent there leaves the Chat intact`, async () => {
    const f = world();
    f.state.runs.push({ id: "run1", space_id: "acme", room_id: "lobby", status: "running", actor: "alice", task: "Audit the build" });
    const app = await inChat(f, width);
    await app.waitFor("label:Show agent runs, 1 active");
    expect(app.shows("id:runs-toggle-attention")).toBe(true);
    expect(app.shows("id:runs-toggle >> text:/Runs/")).toBe(false); // icon only
    expect(app.shows("id:runs-aside")).toBe(false); // never squeezed beside the Chat
    await app.type("id:composer", "my chat draft");

    await app.press("id:runs-toggle");
    await app.waitFor("id:runs-dialog");
    await app.press("id:run-follow:run1");
    await app.waitFor("text:To: agent in Acme › #lobby");
    await app.type("id:dispatch-text", "also check the tests");
    await app.press("id:dispatch-send");
    await app.waitFor("id:dispatch-notice");
    expect(f.posts().at(-1)).toMatchObject({ path: "/v1/spaces/acme/rooms/lobby/messages", body: { text: "also check the tests", invoke_agent: true } });

    await app.press("id:runs-dialog-close");
    await app.waitFor("id:runs-dialog", { state: "gone" });
    expect(app.shows("id:chat-view")).toBe(true);
    expect(app.value("id:composer")).toBe("my chat draft");

    // Escape closes it too.
    await app.press("id:runs-toggle");
    await app.waitFor("id:runs-dialog");
    await app.back();
    await app.waitFor("id:runs-dialog", { state: "gone" });
    expect(app.value("id:composer")).toBe("my chat draft");
  });
}

test("wide runs collapse and reopen without losing either draft or affecting workspace navigation", async () => {
  const f = world();
  const app = await inChat(f, 1200);
  await app.waitFor("id:runs-aside");
  await app.press("id:dispatch-new");
  await app.type("id:dispatch-text", "unfinished task");
  await app.type("id:composer", "unfinished message");
  await app.press("label:Hide agent runs");
  await app.waitFor("id:runs-aside", { state: "gone" });
  expect(app.shows("id:sidebar")).toBe(true);
  expect(app.shows("label:Show agent runs")).toBe(true);
  expect(app.storage()["metro.layout.runs"]).toBe("collapsed");
  await app.keyboard.press("mod+shift+b");
  await app.waitFor("id:runs-aside");
  expect(app.value("id:dispatch-text")).toBe("unfinished task");
  expect(app.value("id:composer")).toBe("unfinished message");
  expect(f.posts().some(p => p.path.endsWith("/messages"))).toBe(false);
});

test("Open from the dialog closes it and goes to the run's room", async () => {
  const f = world();
  f.state.runs.push({ id: "run1", space_id: "acme", room_id: "lobby", status: "running", actor: "alice", task: "Audit the build" });
  const app = await inChat(f, 860);
  await app.press("id:runs-toggle");
  await app.waitFor("id:run-open:run1");
  await app.press("id:run-open:run1");
  await app.waitFor("text:hello from bob in acme");
  expect(app.shows("id:runs-dialog")).toBe(false);
});

const sentAnything = (f: F) => f.posts().some((p) => p.path.endsWith("/messages"));

test("no runs: a short explanation and task suggestions that only fill an empty task draft", async () => {
  const f = world();
  const app = await inChat(f);
  await app.waitFor("id:runs-empty");
  expect(app.shows("text:No runs yet. Tasks you send to a Space's agent show up here.")).toBe(true);
  expect(app.shows("id:dispatch")).toBe(false);

  // The Chat's own message box has the user's words; a task suggestion never touches it.
  await app.type("id:composer", "personal note");
  await app.press("id:runs-suggestion:0");
  await app.waitFor("id:dispatch");
  expect(app.value("id:dispatch-text")).toBe("Summarize what happened in this room recently.");
  expect(app.value("id:composer")).toBe("personal note");
  // No Space or room is chosen for the user, so nothing can be sent yet.
  expect(app.shows("text:Choose a Space and a room.")).toBe(true);
  expect(app.enabled("id:dispatch-send")).toBe(false);

  // A draft the user wrote is never replaced.
  await app.clear("id:dispatch-text");
  await app.type("id:dispatch-text", "my own task");
  await app.press("id:runs-suggestion:1");
  expect(app.value("id:dispatch-text")).toBe("my own task");
  expect(app.shows("label:List open work: open the task form (your draft is kept)")).toBe(true);

  // Folded and reopened from a suggestion: still the user's draft.
  await app.press("id:dispatch-close");
  await app.waitFor("id:dispatch", { state: "gone" });
  await app.press("id:runs-suggestion:0");
  await app.waitFor("id:dispatch");
  expect(app.value("id:dispatch-text")).toBe("my own task");
  expect(app.value("id:composer")).toBe("personal note");

  await app.wait(300);
  expect(sentAnything(f)).toBe(false);
});

test("no runs and no Space: the suggestions are disabled and say why, and never open or send", async () => {
  const f = world();
  const app = await inChat(f, 1200, "carol");
  await app.waitFor("id:runs-empty");
  expect(app.shows("text:No runs yet. Join or create a Space to send its agent a task.")).toBe(true);
  for (const i of [0, 1]) expect(app.enabled(`id:runs-suggestion:${i}`)).toBe(false);
  expect(app.shows("label:Summarize a room: unavailable, you're not in any Space yet")).toBe(true);
  await app.press("id:runs-suggestion:0").catch(() => {});
  await app.wait(200);
  expect(app.shows("id:dispatch")).toBe(false);
  expect(sentAnything(f)).toBe(false);
});
