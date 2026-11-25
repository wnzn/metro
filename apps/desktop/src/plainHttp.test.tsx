// Plain HTTP to a Metro server (BRIEF item 12): loopback and Tailscale are
// used without asking; any other host asks once per exact address before the
// token is sent, and the confirmation is remembered with the saved server.
// Headless, fake tokens only.

import { expect, open, test } from "nsh/test";
import type { App } from "nsh/test";
import vectors from "../../../crates/url-policy/testdata/host_vectors.json";
import { BASE, TOKENS, world } from "../test/fakeServer";
import { launched } from "../test/launched";

const LAUNCHER_NO_PROXY = (vectors as unknown as { launcher_no_proxy_entries: string[] }).launcher_no_proxy_entries.join(",");

type F = ReturnType<typeof world>;
type Req = { method: string; url: string; headers: Record<string, string>; body?: string | null };

const LAN = "http://192.168.1.5:8787";
const TAILNET_IP = "http://100.101.102.103:8787";
const MAGIC_DNS = "http://metro.tail1234.ts.net";

/** Serves `server` at `origin` instead of BASE. */
function at(origin: string, server: F) {
  const network: Record<string, (req: Req) => unknown> = {};
  for (const m of ["GET", "POST", "PUT", "PATCH", "DELETE"]) {
    const inner = (server.network as any)[`${m} ${BASE}/v1/*`];
    network[`${m} ${origin}/v1/*`] = (req: Req) => inner({ ...req, url: req.url.replace(origin, BASE) });
  }
  return network;
}

async function launch(storage: Record<string, string> = {}, env?: Record<string, string>): Promise<App> {
  const s = world();
  const network = { ...(s.network as any), ...at(LAN, s), ...at(TAILNET_IP, s), ...at(MAGIC_DNS, s) };
  // Without `env`, as the launcher starts the app with no proxy configured.
  const modules = env ? { process: { "default.env": env } } as any : launched();
  return open("../App.tsx", { network, platform: "linux", width: 1200, height: 820, storage, modules });
}

async function fill(app: App, url: string, user = "alice") {
  await app.clear("id:server-url");
  await app.type("id:server-url", url);
  await app.type("id:access-token", TOKENS[user]!);
}

const sentTo = (app: App, origin: string) => app.requests().filter((r: any) => r.url.startsWith(origin)).length;
const saved = (app: App): Record<string, unknown>[] => JSON.parse(app.storage()["metro.savedServers"] ?? "[]");
const noTokensStored = (app: App) => {
  const all = JSON.stringify(app.storage());
  for (const t of Object.values(TOKENS)) expect(all).not.toContain(t);
};

test("the server URL hint no longer says HTTPS only", async () => {
  const app = await launch();
  await app.waitFor("id:server-url");
  expect(app.shows("text:HTTPS, or HTTP on this computer, Tailscale or a network you trust.")).toBe(true);
  expect(app.shows("text:Use HTTPS, or HTTP on this computer.")).toBe(false);
});

test("Tailscale addresses and MagicDNS names connect over plain HTTP without asking", async () => {
  for (const url of [TAILNET_IP, MAGIC_DNS]) {
    const app = await launch();
    await fill(app, url);
    await app.press("id:connect");
    await app.waitFor("id:account-menu");
    expect(app.shows("id:plain-http-consent")).toBe(false);
    expect(sentTo(app, url)).toBeGreaterThan(0);
    await app.waitUntil(() => saved(app).length === 1);
    expect(saved(app)[0]).toEqual({ id: saved(app)[0].id, name: saved(app)[0].name, baseUrl: url }); // no consent flag needed
  }
});

test("another plain-HTTP host asks first; Cancel sends nothing, Connect anyway signs in and remembers it with the saved server", async () => {
  const app = await launch();
  await fill(app, LAN);
  await app.press("id:connect");
  await app.waitFor("id:plain-http-consent");
  expect(app.shows("text:This connection isn't encrypted. Anyone on the network between you and 192.168.1.5:8787 could read your sign-in token. Use it on networks you trust, such as Tailscale or a VPN.")).toBe(true);
  expect(app.shows("id:connect")).toBe(false);
  expect(sentTo(app, LAN)).toBe(0);

  await app.press("id:plain-http-consent:cancel");
  await app.waitFor("id:plain-http-consent", { state: "gone" });
  expect(app.shows("id:connect")).toBe(true);
  expect(sentTo(app, LAN)).toBe(0);
  expect(app.shows("id:account-menu")).toBe(false);

  await app.press("id:connect");
  await app.waitFor("id:plain-http-consent");
  expect(sentTo(app, LAN)).toBe(0);
  await app.press("id:plain-http-consent:accept");
  await app.waitFor("id:account-menu");
  expect(sentTo(app, LAN)).toBeGreaterThan(0);
  await app.waitUntil(() => saved(app).length === 1 && saved(app)[0].plainHttp === true);
  expect(saved(app)[0]).toMatchObject({ baseUrl: LAN, plainHttp: true });
  noTokensStored(app);

  // After a restart the saved server is used without asking again.
  const again = await launch({ "metro.savedServers": app.storage()["metro.savedServers"]! });
  const id = saved(app)[0].id as string;
  await again.press(`id:saved-server:${id}`);
  await again.type("id:access-token", TOKENS.alice!);
  await again.press("id:connect");
  await again.waitFor("id:account-menu");
  expect(again.shows("id:plain-http-consent")).toBe(false);
});

test("a saved server without the confirmation asks before its token goes out, also from the account menu", async () => {
  const id = "srv-lan";
  const app = await launch({ "metro.savedServers": JSON.stringify([{ id, name: "LAN", baseUrl: LAN }]) });
  await app.press("id:show-server-form");
  await fill(app, BASE); // loopback: never asks
  await app.press("id:connect");
  await app.waitFor("id:account-menu");
  expect(app.shows("id:plain-http-consent")).toBe(false);

  await app.press("id:account-menu");
  await app.press(`id:workspace-server:${id}`);
  await app.waitFor("id:workspace-token");
  await app.type("id:workspace-token", TOKENS.bob!);
  await app.press("id:workspace-sign-in");
  await app.waitFor("id:workspace-plain-http");
  expect(app.shows("id:workspace-sign-in")).toBe(false);
  expect(sentTo(app, LAN)).toBe(0);
  await app.press("id:workspace-plain-http:cancel");
  await app.waitFor("id:workspace-sign-in");
  expect(sentTo(app, LAN)).toBe(0);

  await app.press("id:workspace-sign-in");
  await app.press("id:workspace-plain-http:accept");
  await app.waitFor("id:workspace-dialog", { state: "gone" });
  expect(sentTo(app, LAN)).toBeGreaterThan(0);
  // Confirming a saved server writes the flag to its record.
  await app.waitUntil(() => saved(app)[0]?.plainHttp === true);
  noTokensStored(app);
});

test("not saving the server keeps the confirmation for this run only", async () => {
  const app = await launch();
  await fill(app, LAN);
  await app.press("id:save-server"); // off
  await app.press("id:connect");
  await app.press("id:plain-http-consent:accept");
  await app.waitFor("id:account-menu");
  expect(saved(app)).toEqual([]);
  const again = await launch({ "metro.serverUrl": LAN });
  await again.waitFor("id:server-url");
  await again.waitUntil(() => again.value("id:server-url") === LAN);
  await again.type("id:access-token", TOKENS.alice!);
  await again.press("id:connect");
  await again.waitFor("id:plain-http-consent");
  expect(sentTo(again, LAN)).toBe(0);
});

test("Add a server asks before signing in to a plain-HTTP host, and Forget drops the confirmation", async () => {
  const app = await launch();
  await fill(app, BASE); // loopback: never asks
  await app.press("id:connect");
  await app.waitFor("id:account-menu");
  expect(app.shows("id:plain-http-consent")).toBe(false);

  await app.press("id:account-menu");
  await app.press("id:add-server");
  await app.waitFor("id:workspace-dialog");
  await app.type("id:add-server-url", LAN);
  await app.type("id:add-server-token", TOKENS.bob!);
  await app.press("id:add-server-submit");
  await app.waitFor("id:add-server-plain-http");
  expect(app.shows("id:add-server-submit")).toBe(false);
  expect(sentTo(app, LAN)).toBe(0);
  await app.press("id:add-server-plain-http:accept");
  await app.waitFor("id:workspace-dialog", { state: "gone" });
  await app.waitUntil(() => saved(app).some((s) => s.baseUrl === LAN && s.plainHttp === true));
  const lan = saved(app).find((s) => s.baseUrl === LAN)!.id as string;

  await app.press("id:account-menu");
  await app.press("id:manage-servers");
  await app.press(`id:forget-server:${lan}`);
  await app.waitUntil(() => !saved(app).some((s) => s.baseUrl === LAN));
  await app.press("id:manage-servers-done");

  // Adding it again asks again.
  await app.press("id:account-menu");
  await app.press("id:add-server");
  await app.waitFor("id:workspace-dialog");
  await app.type("id:add-server-url", LAN);
  await app.type("id:add-server-token", TOKENS.bob!);
  await app.press("id:add-server-submit");
  await app.waitFor("id:add-server-plain-http");
  await app.press("id:add-server-plain-http:cancel");
  await app.waitFor("id:add-server-submit");
  noTokensStored(app);
});

test("behind a proxy, Tailscale asks unless the person's NO_PROXY lists it; loopback stays direct", async () => {
  const proxied = await launch({}, { METRO_PROXY_ROUTE: "proxied", METRO_PROXY_DIRECT_FOR: "corp.example," + LAUNCHER_NO_PROXY });
  await fill(proxied, TAILNET_IP);
  await proxied.press("id:connect");
  await proxied.waitFor("id:plain-http-consent");
  expect(proxied.shows("text:This connection goes through a proxy and isn't encrypted. The proxy, and anyone on the network between you and it, could read your sign-in token for 100.101.102.103:8787. Use it on networks you trust, such as Tailscale or a VPN.")).toBe(true);
  expect(sentTo(proxied, TAILNET_IP)).toBe(0);
  await proxied.press("id:plain-http-consent:cancel");
  await fill(proxied, BASE);
  await proxied.press("id:connect");
  await proxied.waitFor("id:account-menu");
  expect(proxied.shows("id:plain-http-consent")).toBe(false);

  const listed = await launch({}, { METRO_PROXY_ROUTE: "proxied", METRO_PROXY_DIRECT_FOR: "100.101.102.103," + LAUNCHER_NO_PROXY });
  await fill(listed, TAILNET_IP);
  await listed.press("id:connect");
  await listed.waitFor("id:account-menu");
  expect(listed.shows("id:plain-http-consent")).toBe(false);
});

test("without the launcher's route (unknown), even loopback and Tailscale ask first", async () => {
  const app = await launch({}, {});
  await fill(app, BASE);
  await app.press("id:connect");
  await app.waitFor("id:plain-http-consent");
  expect(app.shows("text:This connection may go through a proxy and isn't encrypted. The proxy, and anyone on the network between you and it, could read your sign-in token for 127.0.0.1:8787. Use it on networks you trust, such as Tailscale or a VPN.")).toBe(true);
  expect(sentTo(app, BASE)).toBe(0);
  await app.press("id:plain-http-consent:accept");
  await app.waitFor("id:account-menu");
});
