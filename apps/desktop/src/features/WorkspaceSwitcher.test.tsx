import { expect, open, test } from "nsh/test";
import { BASE, TOKENS, world } from "../../test/fakeServer";
import { launched } from "../../test/launched";

test("Add verifies an explicit token even for the active URL, and renames only after success", async () => {
  const app = await open("../../App.tsx", { modules: launched(), network: world().network as any });
  await app.type("id:access-token", TOKENS.alice!);
  await app.press("id:connect");
  const list = () => JSON.parse(app.storage()["metro.savedServers"] ?? "[]");
  await app.waitUntil(() => list().length === 1);
  const original = list();
  await app.press("id:space:acme");
  await app.type("id:composer", "still writing");
  await app.press("id:account-menu");
  await app.press("id:add-server");
  await app.type("id:add-server-url", BASE);
  await app.type("id:add-server-name", "New name");
  await app.type("id:add-server-token", "wrong");
  await app.press("id:add-server-submit");
  await app.press("id:workspace-discard-switch");
  await app.waitFor("id:workspace-error");
  expect(list()).toEqual(original);
  expect(app.value("id:composer")).toBe("still writing");
  await app.clear("id:add-server-token");
  await app.type("id:add-server-token", TOKENS.bob!);
  await app.press("id:add-server-submit");
  await app.press("id:workspace-discard-switch");
  await app.waitFor("id:workspace-dialog", { state: "gone" });
  expect(list()).toEqual([{ ...original[0], name: "New name" }]);
  expect(app.shows("text:bob")).toBe(true);
});

test("a file-only draft survives backing out while Add is checking a new account", async () => {
  const w = world();
  // A manual clock: Bob's verification answers only when the test moves time.
  const app = await open("../../App.tsx", { modules: launched(), network: w.network as any, platform: "linux", width: 1200, height: 820, clock: {} });
  await app.type("id:access-token", TOKENS.alice!);
  await app.press("id:connect");
  await app.waitFor("id:attach");
  app.onPick(() => [{ name: "pending.txt", type: "text/plain", data: new TextEncoder().encode("pending") }]);
  await app.press("id:attach");
  await app.waitFor("id:staged:pending.txt");
  const before = app.storage()["metro.savedServers"];
  // A successful answer, 5 s of app time later: inside the app's own 10 s
  // limit, so it really arrives, after the person has backed out.
  const bobCheck = w.answerLate("/v1/me", TOKENS.bob!, 5_000);
  await app.press("id:account-menu");
  await app.press("id:add-server");
  await app.type("id:add-server-name", "Late rename");
  await app.type("id:add-server-url", BASE);
  await app.type("id:add-server-token", TOKENS.bob!);
  await app.press("id:add-server-submit");
  await app.press("id:workspace-discard-switch");
  const bobRequests = () => w.state.log.filter((l) => l.user === "bob").map((l) => `${l.method} ${l.path}`).sort();
  // Verification asks who Bob is and lists his Spaces, together.
  await app.waitUntil(() => bobCheck.arrived() === 1 && bobRequests().length === 2, { timeout: 1_000 });
  expect(bobRequests()).toEqual(["GET /v1/me", "GET /v1/spaces"]);
  await app.back();
  await app.waitFor("id:workspace-dialog", { state: "gone", timeout: 1_000 });
  expect(app.shows("id:staged:pending.txt")).toBe(true);
  // Move past Bob's answer (and well short of the 10 s limit): the late
  // success must not switch, rename, or drop the staged file.
  await app.clock!.runFor(6_000);
  expect(app.shows("id:staged:pending.txt")).toBe(true);
  expect(app.shows("text:alice")).toBe(true);
  expect(app.storage()["metro.savedServers"]).toBe(before);
  // The stale success went no further: Bob's token was used for that one check only.
  expect(bobRequests()).toEqual(["GET /v1/me", "GET /v1/spaces"]);
  expect(app.texts().some((t) => t.includes("Late rename"))).toBe(false);
});

test("choosing the startup server persists its id without contacting it or leaving the current workspace", async () => {
  const server = { id: "other", name: "Other server", baseUrl: "https://other.example.org" };
  const app = await open("../../App.tsx", {
    modules: launched(),
    network: world().network as any,
    storage: { "metro.savedServers": JSON.stringify([server]) },
  });
  await app.press("id:show-server-form");
  await app.type("id:access-token", TOKENS.alice!);
  await app.press("id:connect");
  await app.press("id:account-menu");
  await app.press("id:startup-server");
  await app.press("id:startup-server:other");
  await app.waitFor("id:workspace-dialog", { state: "gone" });
  expect(app.storage()["metro.startupServer"]).toBe(server.id);
  expect(app.requests().some((r) => r.url.startsWith(server.baseUrl))).toBe(false);
  expect(app.shows("id:space:acme")).toBe(true);
});
