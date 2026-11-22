// Profiles and the transcript against the shared fake server plus the
// profile routes (test/fixtures.ts). Real NSH engine, real store and views,
// no network; fake tokens only.

import { expect, open, test } from "nsh/test";
import type { App } from "nsh/test";
import { BASE, TOKENS } from "../../../test/fakeServer";
import { BOB_PNG, profileWorld } from "./test/fixtures";

type W = ReturnType<typeof profileWorld>;

async function signIn(w: W, user: "alice" | "bob" | "carol" = "alice"): Promise<App> {
  const app = await open("./test/Harness.tsx", {
    network: w.network as any,
    platform: "linux",
    width: 1280,
    height: 900,
    localeSettings: { timeZone: "UTC", hourCycle: "h12", languages: ["en-US"] },
  });
  await app.clear("id:server-url");
  await app.type("id:server-url", BASE);
  await app.type("id:access-token", TOKENS[user]!);
  await app.press("id:connect");
  await app.waitFor("id:h-profile");
  return app;
}

async function openAcme(app: App) {
  await app.waitFor("id:space:acme");
  await app.press("id:space:acme");
  await app.waitFor("text:hello from bob in acme");
}

const text = (app: App) => app.texts().join(" | ");
const calls = (w: W, path: string, method = "GET") => w.p.log.filter((r) => r.path === path && r.method === method);

function seedAcme(w: W) {
  const bob = w.say("acme/lobby", { kind: "message", actor: "Bob Tan", actor_id: "bob", text: "Deploy is green on staging.", created_at: "2026-11-21T23:50:00.000Z" });
  const a1 = w.say("acme/lobby", { kind: "message", actor: "Alice Moreno", actor_id: "alice", text: "Thanks! Shipping to prod.", created_at: "2026-11-22T01:02:03.000Z", mentions: [{ kind: "user", id: "bob" }], audience: "users" });
  const a2 = w.say("acme/lobby", { kind: "message", actor: "Alice Moreno", actor_id: "alice", text: "Will post the tag here.", created_at: "2026-11-22T01:03:00.000Z" });
  const ask = w.say("acme/lobby", { kind: "message", actor: "Bob Tan", actor_id: "bob", text: "@agent summarize the rollout", created_at: "2026-11-22T01:10:00.000Z", mentions: [{ kind: "agent" }], audience: "agent" });
  const reply = w.say("acme/lobby", { kind: "agent", actor: "agent", text: "Staging is green; prod rollout started at 01:02 UTC.", created_at: "2026-11-22T01:10:20.000Z" });
  const failed = w.say("acme/lobby", { kind: "error", actor: "system", text: "Runner went offline.", created_at: "2026-11-22T01:11:00.000Z" });
  return { bob, a1, a2, ask, reply, failed };
}

test("the transcript shows profile names, 'you', the agent, local times and day lines; undated history shows no time", async () => {
  const w = profileWorld();
  const s = seedAcme(w);
  const app = await signIn(w);
  await openAcme(app);
  await app.waitFor("text:Bob Tan");

  // Day lines, in the device zone (UTC here), and each entry's server time.
  expect(app.shows("text:Saturday, November 21, 2026")).toBe(true);
  expect(app.shows("text:Sunday, November 22, 2026")).toBe(true);
  expect(text(app)).toContain("11:50 PM");
  expect(app.shows(`id:time:${s.a1.id}`)).toBe(true);
  expect(text(app)).toContain("1:02 AM");

  // The older, undated message keeps its snapshot name and shows no time.
  const legacy = w.f.state.events.get("acme/lobby")!.find((e) => e.text === "hello from bob in acme")!;
  expect(app.shows(`id:time:${legacy.id}`)).toBe(false);
  expect(app.shows(`id:author:${legacy.id}`)).toBe(true);

  // Mine: marked "you", recipients kept, by display name.
  expect(app.shows(`id:you:${s.a1.id}`)).toBe(true);
  expect(app.shows(`id:you:${s.bob.id}`)).toBe(false);
  expect(app.shows(`id:recipient:${s.a1.id}:bob`)).toBe(true);
  expect(text(app)).toContain("@Bob Tan");
  expect(text(app)).toContain("to people");
  // A second message a minute later continues the first: no repeated header.
  expect(app.shows(`id:author:${s.a2.id}`)).toBe(false);
  expect(app.shows(`id:time:${s.a2.id}`)).toBe(true);

  // The agent is recognizable; ordinary messages carry no kind badge; errors still do.
  expect(app.shows(`id:author:${s.reply.id}`)).toBe(true);
  expect(text(app)).toContain("Metro");
  expect(app.shows(`id:recipient:${s.ask.id}:agent`)).toBe(true);
  expect(app.shows(`id:kind:${s.bob.id}`)).toBe(false);
  expect(app.shows(`id:kind:${s.failed.id}`)).toBe(true);
  expect(text(app)).not.toContain("user message");

  // Reactions: one quiet "+" per message, a row only once there are some.
  expect(app.shows(`id:react:${s.bob.id}`)).toBe(true);
  expect(app.shows(`id:reactions:${s.bob.id}`)).toBe(false);
  await app.press(`id:react:${s.bob.id}`);
  await app.press(`id:pick-reaction:${s.bob.id}:👍`);
  await app.waitFor(`id:reaction:${s.bob.id}:👍`);
  expect(app.shows(`id:palette:${s.bob.id}`)).toBe(false);

  // Bob's picture is asked for as bytes on the authenticated transport, never a URL with a
  // credential. (The test network carries only text, so the bytes can't be a real PNG here and
  // initials stay: an answer that isn't a PNG or JPEG is never shown. Native captures show it.)
  await app.waitUntil(() => calls(w, "/v1/profiles/bob/avatar").length > 0);
  const pic = calls(w, "/v1/profiles/bob/avatar");
  expect(pic.length).toBe(1); // asked once for the session
  expect(pic[0]!.headers["authorization"]).toBe(`Bearer ${TOKENS.alice}`);
  expect(app.requests().some((r) => r.url.includes(TOKENS.alice!))).toBe(false);
  // Alice has no picture: initials.
  expect(app.shows(`id:avatar:${s.a1.id}:image`)).toBe(false);
});

test("a profile that stops being visible falls back to the snapshot name and initials; 404 is never an error", async () => {
  const w = profileWorld();
  w.f.state.spaces.get("acme")!.members.add("carol");
  w.p.profiles.get("carol")!.avatar = BOB_PNG;
  w.p.profiles.get("carol")!.version = 1;
  const c = w.say("acme/lobby", { kind: "message", actor: "Carol", actor_id: "carol", text: "carol was here", created_at: "2026-11-22T02:00:00.000Z" });
  // Someone the user shares nothing with (the server answers 404).
  const stranger = w.say("acme/lobby", { kind: "message", actor: "Old Name", actor_id: "dave", text: "from a former member", created_at: "2026-11-22T02:01:00.000Z" });
  w.p.profiles.get("carol")!.display_name = "Carol Lee";
  const app = await signIn(w);
  await openAcme(app);
  await app.waitFor("text:Carol Lee");
  expect(calls(w, "/v1/profiles/carol/avatar").length).toBe(1);
  // dave: 404, so his snapshot name and initials, no picture asked for, no error.
  expect(text(app)).toContain("Old Name");
  expect(calls(w, "/v1/profiles/dave").length).toBe(1);
  expect(calls(w, "/v1/profiles/dave/avatar").length).toBe(0);
  expect(app.shows("id:action-error")).toBe(false);

  // Carol leaves Acme (and shares nothing else with Alice).
  w.f.state.spaces.get("acme")!.members.delete("carol");
  w.p.profiles.get("carol")!.display_name = "Carol Renamed";
  await app.press("id:space:side");
  await app.waitFor("text:side lobby only");
  await app.press("id:space:acme");
  await app.waitFor("text:carol was here");
  // Checked again on entering the Space: now a 404, so the snapshot name and initials.
  await app.waitUntil(() => calls(w, "/v1/profiles/carol").length >= 2);
  await app.waitUntil(() => !text(app).includes("Carol Lee"));
  expect(text(app)).toContain("Carol");
  expect(text(app)).not.toContain("Carol Renamed");
  expect(app.shows(`id:avatar:${c.id}:image`)).toBe(false);
});

test("Settings › Profile edits the name and bio, and sets, changes and removes the picture from the scoped picker", async () => {
  const w = profileWorld();
  const s = seedAcme(w);
  const app = await signIn(w);
  await openAcme(app);
  await app.press("id:h-profile");
  await app.waitFor("id:profile-name");
  expect(app.value("id:profile-name")).toBe("Alice Moreno");
  expect(app.value("id:profile-bio")).toBe("Runs the Acme rollout.");
  expect(app.shows("id:profile-id")).toBe(true);
  expect(app.enabled("id:profile-save")).toBe(false); // nothing changed yet
  expect(app.shows("id:profile-picture-remove")).toBe(false); // no picture: initials

  // Name and bio.
  await app.clear("id:profile-name");
  expect(app.enabled("id:profile-save")).toBe(false);
  await app.waitFor("id:profile-problem");
  await app.type("id:profile-name", "agent");
  await app.press("id:profile-save");
  await app.waitFor("text:/that name is reserved/");
  await app.clear("id:profile-name");
  await app.type("id:profile-name", "Alice M.");
  await app.clear("id:profile-bio");
  await app.type("id:profile-bio", "On call this week.");
  await app.press("id:profile-save");
  await app.waitFor("text:/Profile saved/");
  const patches = calls(w, "/v1/me/profile", "PATCH");
  expect(JSON.parse(patches.at(-1)!.body!)).toEqual({ display_name: "Alice M.", bio: "On call this week." });

  // Picture: only PNG and JPEG are offered, one file, within the limit; the bytes are checked.
  let asked: any = null;
  app.onPick((pick) => {
    asked = pick;
    return [{ name: "fake.png", type: "image/png", data: new TextEncoder().encode("<svg/>") }];
  });
  await app.press("id:profile-picture-choose");
  await app.waitFor("text:/isn't a PNG or JPEG/");
  expect(asked.kind).toBe("files");
  expect(asked.multiple).toBe(false);
  expect(asked.maxSize).toBe(1024 * 1024);
  expect([...asked.filters[0].extensions].sort()).toEqual(["jpeg", "jpg", "png"]);
  expect(calls(w, "/v1/me/profile/avatar", "PUT").length).toBe(0);

  app.onPick(() => [{ name: "me.png", type: "image/png", data: BOB_PNG }]);
  await app.press("id:profile-picture-choose");
  await app.waitFor("text:Picture updated.");
  const put = calls(w, "/v1/me/profile/avatar", "PUT");
  expect(put.length).toBe(1);
  expect(put[0]!.headers["content-type"]).toBe("image/png");
  await app.waitFor("id:profile-avatar:image");
  expect(app.shows("id:profile-picture-remove")).toBe(true);
  expect(app.shows("text:Change picture…")).toBe(true);

  await app.press("id:profile-picture-remove");
  await app.waitFor("text:/Picture removed/");
  expect(calls(w, "/v1/me/profile/avatar", "DELETE").length).toBe(1);
  await app.waitUntil(() => !app.shows("id:profile-avatar:image"));

  // The transcript shows the new name on my messages.
  await app.press("id:h-profile");
  await app.waitFor(`id:author:${s.a1.id}`);
  expect(text(app)).toContain("Alice M.");
});

test("signing out drops profiles, drafts and pictures; the next user starts clean", async () => {
  const w = profileWorld();
  seedAcme(w);
  const app = await signIn(w);
  await openAcme(app);
  await app.press("id:h-profile");
  await app.waitFor("id:profile-name");
  await app.type("id:profile-name", " (draft)");
  await app.press("id:h-profile");
  await app.press("id:disconnect");
  await app.waitFor("id:connect");
  await app.type("id:access-token", TOKENS.bob!);
  await app.press("id:connect");
  await app.waitFor("id:h-profile");
  await app.press("id:h-profile");
  await app.waitFor("id:profile-name");
  expect(app.value("id:profile-name")).toBe("Bob Tan");
  expect(text(app)).not.toContain("(draft)");
  expect(text(app)).not.toContain("Alice Moreno");
});

test("a server without profiles: the page says so, the transcript still works", async () => {
  const w = profileWorld();
  const network = { ...w.network } as Record<string, (req: any) => unknown>;
  for (const key of Object.keys(network)) {
    const inner = network[key]!;
    network[key] = (req: any) => (new URL(req.url).pathname.includes("profile") ? { status: 404, body: { error: "not found" } } : inner(req));
  }
  const app = await open("./test/Harness.tsx", { network: network as any, platform: "linux", width: 1280, height: 900, localeSettings: { timeZone: "UTC" } });
  await app.clear("id:server-url");
  await app.type("id:server-url", BASE);
  await app.type("id:access-token", TOKENS.alice!);
  await app.press("id:connect");
  await openAcme(app);
  await app.press("id:h-profile");
  await app.waitFor("text:/doesn't support profiles yet/");
});
