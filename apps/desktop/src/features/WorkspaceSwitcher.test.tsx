import { expect, open, test } from "nsh/test";
import { BASE, TOKENS, world } from "../../test/fakeServer";

test("Add verifies an explicit token even for the active URL, and renames only after success", async () => {
  const app = await open("../../App.tsx", { network: world().network as any });
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
  const app = await open("../../App.tsx", { network: w.network as any, platform: "linux", width: 1200, height: 820 });
  await app.type("id:access-token", TOKENS.alice!);
  await app.press("id:connect");
  await app.waitFor("id:attach");
  app.onPick(() => [{ name: "pending.txt", type: "text/plain", data: new TextEncoder().encode("pending") }]);
  await app.press("id:attach");
  await app.waitFor("id:staged:pending.txt");
  const before = app.storage()["metro.savedServers"];
  w.state.delays.set("/v1/me", 800);
  await app.press("id:account-menu");
  await app.press("id:add-server");
  await app.type("id:add-server-name", "Late rename");
  await app.type("id:add-server-url", BASE);
  await app.type("id:add-server-token", TOKENS.bob!);
  await app.press("id:add-server-submit");
  await app.press("id:workspace-discard-switch");
  await app.back();
  await app.waitFor("id:workspace-dialog", { state: "gone" });
  await app.wait(900); // even a successful delayed verification cannot commit
  expect(app.shows("id:staged:pending.txt")).toBe(true);
  expect(app.shows("text:alice")).toBe(true);
  expect(app.storage()["metro.savedServers"]).toBe(before);
});

test("choosing the startup server persists its id without contacting it or leaving the current workspace", async () => {
  const server = { id: "other", name: "Other server", baseUrl: "https://other.example.org" };
  const app = await open("../../App.tsx", {
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
