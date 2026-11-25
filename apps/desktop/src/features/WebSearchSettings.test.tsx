import { expect, open, test } from "nsh/test";
import type { App } from "nsh/test";
import { BASE, header, world } from "../../test/fakeServer";
import { launched } from "../../test/launched";
import { BRIDGE, BRIDGE_TOKEN, bridgeEnv, fakeBridge } from "../device/test/fakeBridge";
import { WEB_PROVIDERS } from "../api/webSearch";
import { parseLocalWebSearch } from "../device/webSearch";

const OTHER = BASE.replace("8787", "8788");
const PATH = "/v1/web-search";
const SYNTHETIC = "fixture-only-value-not-a-real-key";
const REFLECTED = "fixture-only-private-diagnostic";
type Req = { method: string; url: string; headers: Record<string, string>; body?: string | null };
type Row = ReturnType<typeof rows>[number];
const rows = () => Object.keys(WEB_PROVIDERS).map((id) => ({
  id, enabled: true, keyless: id !== "tinyfish", key_saved: false, key_status: null as string | null,
  cooldown: null as null | { until: string; mode: string; outcome: string },
}));
function device() {
  return {
    enabled: true, notice_acknowledged: false, providers: rows(),
    device: { paired: true, worker: "online", detail: null as string | null },
    keys_error: null as string | null, settings_error: null as string | null, endpoints_error: null,
  };
}

function fixture() {
  const f = world();
  const b = fakeBridge(f);
  const alice = device();
  const bob = device();
  bob.enabled = false;
  bob.device.worker = "stopped";
  const other = device();
  other.providers.reverse();
  const states = new Map([[`${BASE} alice`, alice], [`${BASE} bob`, bob], [`${OTHER} alice`, other]]);
  const calls: { method: string; path: string; binding: string; providers?: string[]; keyAccepted?: boolean }[] = [];
  const chatWrites: { host: string; enabled: boolean }[] = [];
  const prefs = new Map<string, boolean>();
  const replies = { unsupported: false, oldServer: false, testRefused: false, rejectKey: false, refusePrefs: false, refuseOrder: false, writeDelay: 0 };
  const ok = (body: unknown, delay = 0) => ({ status: 200, body, delay });
  const err = (status: number, code: string) => ({ status, body: { error: { code, message: REFLECTED } } });
  function web(req: Req) {
    if (header(req, "authorization") !== `Bearer ${BRIDGE_TOKEN}`) return err(401, "unauthorized");
    const s = b.state.session;
    const binding = `${s?.server_url} ${s?.account_id}`;
    const state = states.get(binding);
    if (!state) return err(409, "no_session");
    const path = new URL(req.url).pathname;
    const record: typeof calls[number] = { method: req.method, path, binding };
    calls.push(record);
    if (replies.unsupported) return { status: 404, body: "" };
    if (path === PATH) {
      if (req.method === "GET") return ok(state);
      const body = JSON.parse(req.body!);
      if (body.providers) {
        record.providers = body.providers.map((r: Row) => r.id);
        if (replies.refuseOrder) return err(422, "invalid");
        state.providers = body.providers.map((r: Row) => ({ ...state.providers.find((p) => r.id === p.id), enabled: r.enabled }));
      }
      if (body.enabled !== undefined) {
        state.enabled = body.enabled;
        state.device.worker = body.enabled ? "online" : "stopped";
        if (state.settings_error?.startsWith("Your web search settings")) {
          state.settings_error = null;
          state.providers = rows();
        }
      }
      if (body.notice_acknowledged) state.notice_acknowledged = true;
      return ok(state, replies.writeDelay);
    }
    if (path === `${PATH}/keys/reset`) {
      state.keys_error = null;
      state.providers.forEach((row) => { row.key_saved = false; row.key_status = null; });
      return ok(state);
    }
    const [, id, action] = /\/providers\/([^/]+)\/([^/]+)$/.exec(path) ?? [];
    const row = state.providers.find((r) => r.id === id);
    if (!row) return err(404, "not_found");
    if (action === "key") {
      if (req.method === "PUT") {
        // Check routing and body without retaining or logging even the test key.
        record.keyAccepted = JSON.parse(req.body!).api_key === SYNTHETIC;
        if (replies.rejectKey) return err(422, "invalid");
      }
      row.key_saved = req.method === "PUT";
      row.key_status = row.key_saved ? "ok" : null;
      return ok({ ...row, api_key: REFLECTED }, replies.writeDelay);
    }
    if (action === "test") return replies.testRefused ? err(409, "not_sent") : ok({ ...row, test: { outcome: "ok" } });
    if (action === "clear-cooldown") {
      if (state.settings_error?.startsWith("Saved provider pauses")) {
        state.settings_error = null;
        state.enabled = true;
      }
      row.cooldown = null;
      row.key_status = row.key_saved ? "ok" : null;
      return ok(row);
    }
    return err(404, "not_found");
  }
  const network = { ...f.network, ...b.network };
  for (const method of ["GET", "PUT", "POST", "DELETE"]) {
    network[`${method} ${BRIDGE}/v1/*`] = (req: Req) => new URL(req.url).pathname.startsWith(PATH)
      ? web(req) : b.network[`${method} ${BRIDGE}/v1/*`]!(req);
    for (const host of [BASE, OTHER]) {
      network[`${method} ${host}/v1/*`] = (req: Req) => {
        if (new URL(req.url).pathname !== `${PATH}/prefs`) return f.network[`${method} ${BASE}/v1/*`]!({ ...req, url: req.url.replace(host, BASE) });
        if (replies.oldServer) return err(404, "not_found");
        const id = `${host} ${header(req, "authorization")}`;
        if (method === "PUT") {
          const enabled = JSON.parse(req.body!).personal_chat;
          chatWrites.push({ host, enabled });
          if (replies.refusePrefs) return err(403, "forbidden");
          prefs.set(id, enabled);
        }
        return ok({ personal_chat: prefs.get(id) ?? false });
      };
    }
  }
  return { network, calls, chatWrites, replies, alice, bob, other, b };
}

async function start(w: ReturnType<typeof fixture>, bridge = true) {
  const app = await open("./WebSearchSettings.fixture.tsx", {
    modules: (bridge ? bridgeEnv() : launched()) as any, network: w.network as any,
    width: 1280, height: 1000, platform: "linux", clock: {}, reducedMotion: true,
  });
  await app.press("id:fixture-alice");
  await app.waitFor("id:websearch-settings");
  return app;
}
function text(app: App) { return app.texts().join(" | "); }
async function item(app: App, name: string, id = "exa") {
  await app.press(`id:websearch-menu:${id}`);
  await app.press(`id:websearch-${name}:${id}`);
}

test("the complete provider list reorders with accessible controls and status; a refused write leaves the real order", async () => {
  const w = fixture();
  const app = await start(w);
  await app.waitFor("id:websearch-up:exa");
  expect(app.enabled("id:websearch-up:exa")).toBe(false);
  expect(app.enabled("id:websearch-down:tinyfish")).toBe(false);
  expect(text(app)).toContain("Needs a free key");
  expect(text(app)).toContain("Free (no key)");
  await app.press("id:websearch-enabled:exa");
  await app.waitFor("text:Exa is off.");
  await app.press("label:Move Exa down");
  await app.waitFor("text:Exa moved to position 2 of 5.");
  expect(w.calls.filter((c) => c.method === "PUT").at(-1)?.providers).toEqual(["parallel", "exa", "tavily", "firecrawl", "tinyfish"]);
  expect(w.alice.providers.find((p) => p.id === "exa")?.enabled).toBe(false);
  w.replies.refuseOrder = true;
  await app.press("id:websearch-up:exa");
  await app.waitFor("text:/couldn't confirm the web search change/");
  expect(app.enabled("id:websearch-up:exa")).toBe(true);
  expect(text(app)).not.toContain(REFLECTED);
  w.alice.providers.forEach((row) => { row.enabled = false; });
  await app.press("id:websearch-refresh");
  await app.waitFor("id:websearch-all-off");
});

test("master notice and Chat consent are separate, with no optimistic or cross-server write", async () => {
  const w = fixture();
  const app = await start(w);
  await app.waitFor("id:websearch-notice");
  await app.waitFor("id:websearch-chat-opt-in");
  expect(text(app)).toContain("third-party providers");
  expect(text(app)).toContain("Exact queries from Space runs");
  expect(text(app)).toContain("may charge that account");
  await app.press("id:websearch-ack");
  await app.waitFor("id:websearch-notice", { state: "gone" });
  await app.press("id:websearch-master");
  await app.waitFor("text:Web search is off on this computer.");
  w.replies.refusePrefs = true;
  await app.press("id:websearch-chat-opt-in");
  await app.waitFor("id:websearch-chat-error");
  expect(text(app)).not.toContain(REFLECTED);
  w.replies.refusePrefs = false;
  await app.press("id:websearch-chat-reload");
  await app.waitFor("id:websearch-chat-opt-in");
  await app.press("id:websearch-chat-opt-in");
  await app.waitUntil(() => w.chatWrites.length === 2);
  expect(w.chatWrites).toEqual([{ host: BASE, enabled: true }, { host: BASE, enabled: true }]);
  expect(w.alice.enabled).toBe(false);
  await app.press("id:websearch-privacy");
  expect(text(app)).toContain("Privacy opened");
});

test("keys are masked, wiped on Escape and close, key errors are safe, and remove is confirmed", async () => {
  const w = fixture();
  const app = await start(w);
  await app.waitFor("id:websearch-provider:exa");
  await item(app, "edit");
  await app.type("id:websearch-key:exa", SYNTHETIC);
  expect(text(app)).not.toContain(SYNTHETIC);
  expect(JSON.stringify(app.storage())).not.toContain(SYNTHETIC);
  await app.keyboard.press("mod+a");
  await app.keyboard.press("mod+c");
  expect(app.clipboard() ?? "").not.toContain(SYNTHETIC);
  await app.keyboard.press("escape");
  await app.waitFor("id:websearch-key:exa", { state: "gone" });
  expect(w.calls.some((r) => r.path.endsWith("/key"))).toBe(false);
  await item(app, "edit");
  expect(app.enabled("id:websearch-save:exa")).toBe(false);
  await app.type("id:websearch-key:exa", SYNTHETIC);
  await app.press("id:fixture-toggle");
  await app.press("id:fixture-toggle");
  await app.waitFor("id:websearch-provider:exa");
  await item(app, "edit");
  expect(app.enabled("id:websearch-save:exa")).toBe(false);
  w.replies.rejectKey = true;
  await app.type("id:websearch-key:exa", SYNTHETIC);
  await app.submit("id:websearch-key:exa");
  await app.waitFor("text:/Couldn't save the Exa key/");
  expect(app.shows("id:websearch-key:exa")).toBe(false);
  expect(text(app)).not.toContain(REFLECTED);
  w.replies.rejectKey = false;
  await item(app, "edit");
  await app.type("id:websearch-key:exa", SYNTHETIC);
  await app.press("id:websearch-save:exa");
  await app.waitFor("text:Saved your Exa key on this computer.");
  expect(w.calls.filter((r) => r.path.endsWith("/key")).map((r) => r.keyAccepted && r.binding === `${BASE} alice`)).toEqual([true, true]);
  expect(text(app)).not.toContain(REFLECTED);
  await item(app, "remove");
  await app.waitFor("text:/Searches use its free mode again/");
  await app.press("id:websearch-remove-confirm:exa:cancel");
  expect(w.alice.providers[0]?.key_saved).toBe(true);
  await item(app, "remove");
  await app.press("id:websearch-remove-confirm:exa:confirm");
  await app.waitFor("text:Removed your Exa key.");
  expect(w.alice.providers[0]?.key_saved).toBe(false);
});

test("account and workspace switches wipe drafts and drop an older mutation's notice", async () => {
  const w = fixture();
  const app = await start(w);
  await app.waitFor("id:websearch-provider:exa");
  await item(app, "edit");
  await app.type("id:websearch-key:exa", SYNTHETIC);
  await app.press("id:fixture-bob");
  await app.waitFor("id:websearch-key:exa", { state: "gone" });
  await app.waitFor("id:websearch-provider:exa");
  await item(app, "edit");
  expect(app.enabled("id:websearch-save:exa")).toBe(false);
  expect(w.calls.some((r) => r.path.endsWith("/key"))).toBe(false);
  await app.press("id:fixture-alice");
  await app.waitFor("id:websearch-provider:exa");
  w.replies.writeDelay = 1500;
  await item(app, "edit");
  await app.type("id:websearch-key:exa", SYNTHETIC);
  await app.press("id:websearch-save:exa");
  await app.waitUntil(() => w.calls.some((r) => r.keyAccepted === true), { timeout: 500 });
  await app.press("id:fixture-other");
  await app.waitFor("id:websearch-provider:tinyfish");
  expect(app.enabled("id:websearch-up:tinyfish")).toBe(false);
  expect(w.other.providers.at(-1)?.key_saved).toBe(false);
  expect(text(app)).not.toContain("Saved your Exa key");
  expect(w.calls.filter((r) => r.path.endsWith("/key")).map((r) => r.binding)).toEqual([`${BASE} alice`]);
});

test("unreadable preferences, pauses and keys have different explicit recovery actions", async () => {
  const w = fixture();
  w.alice.enabled = false;
  w.alice.settings_error = "Your web search settings on this computer can't be read, so web search is off.";
  w.alice.keys_error = REFLECTED;
  w.alice.providers[0]!.key_saved = true;
  const app = await start(w);
  await app.waitFor("id:websearch-settings-error");
  expect(app.enabled("id:websearch-master")).toBe(false);
  await app.press("id:websearch-reset-settings");
  await app.waitFor("text:Settings reset. Web search is off on this computer.");
  expect(w.alice.enabled).toBe(false);
  await app.press("id:websearch-reset-keys");
  await app.press("id:websearch-reset-keys:confirm");
  await app.waitFor("id:websearch-keys-error", { state: "gone" });
  w.alice.settings_error = "Saved provider pauses on this computer can't be read, so web search is paused.";
  await app.press("id:websearch-refresh");
  await app.waitFor("id:websearch-pauses-error");
  expect(app.shows("id:websearch-reset-settings")).toBe(false);
  await item(app, "clear");
  await app.waitFor("id:websearch-pauses-error", { state: "gone" });
  expect(text(app)).not.toContain(REFLECTED);
  expect(w.calls.filter((r) => r.method !== "GET").map((r) => r.path)).toEqual([PATH, `${PATH}/keys/reset`, `${PATH}/providers/exa/clear-cooldown`]);
});

test("explicit provider tests and cooldown clear use separate routes and never show refusal details", async () => {
  const w = fixture();
  w.alice.providers[0]!.cooldown = { until: "2027-01-02T14:20:00Z", mode: "keyless", outcome: "limited" };
  const app = await start(w);
  await app.waitFor("text:/Limit reached · retry after 2027-01-02 14:20 UTC/");
  expect(text(app)).toContain("Metro web search test");
  w.replies.testRefused = true;
  await item(app, "test");
  await app.waitFor("text:/The test was refused/");
  expect(text(app)).not.toContain(REFLECTED);
  w.replies.testRefused = false;
  await item(app, "test");
  await app.waitFor("text:Exa test: worked.");
  await item(app, "clear");
  await app.waitFor("text:/Exa is no longer paused/");
  expect(w.calls.filter((r) => r.method === "POST").map((r) => r.path)).toEqual([`${PATH}/providers/exa/test`, `${PATH}/providers/exa/test`, `${PATH}/providers/exa/clear-cooldown`]);
});

test("missing and old helper, unsupported server and out-of-service worker never look ready", async () => {
  const no = fixture();
  const absent = await start(no, false);
  await absent.waitFor("id:websearch-unavailable");
  await absent.waitFor("id:websearch-chat-opt-in");
  expect(no.calls).toEqual([]);
  const old = fixture();
  old.replies.unsupported = old.replies.oldServer = true;
  const older = await start(old);
  await older.waitFor("id:websearch-unsupported");
  await older.waitFor("id:websearch-chat-unsupported");
  await older.press("Open About & updates");
  expect(text(older)).toContain("About opened");
  const w = fixture();
  w.alice.device.worker = "offline";
  const app = await start(w);
  await app.waitFor("id:websearch-worker >> text:Offline");
  w.alice.device.worker = "error";
  w.alice.device.detail = "Couldn't set up web search with Metro: This Metro server doesn't offer web search yet";
  await app.press("id:websearch-refresh");
  await app.waitFor("id:websearch-worker >> text:Unsupported server");
});

test("a helper bound to the wrong account is never read or written", async () => {
  const w = fixture();
  const route = `POST ${BRIDGE}/v1/*`;
  const next = w.network[route]!;
  w.network[route] = (req: Req) => {
    const response = next(req) as { body?: { session?: { account_id: string; server_url: string } } };
    if (new URL(req.url).pathname !== "/v1/session" || !response.body?.session) return response;
    return { ...response, body: { ...response.body, session: { ...response.body.session, account_id: "bob" } } };
  };
  const app = await start(w);
  await app.waitFor("text:/isn't serving this account on this server/");
  await app.waitFor("id:websearch-chat-opt-in");
  expect(w.calls).toEqual([]);
  expect(app.shows("id:websearch-master")).toBe(false);
});

test("bridge parser drops diagnostics and credentials, and rejects an incomplete order", () => {
  const wire = device();
  const safe = parseLocalWebSearch({ ...wire, device: { ...wire.device, detail: REFLECTED },
    providers: wire.providers.map((row) => ({ ...row, api_key: SYNTHETIC, name: REFLECTED })) });
  expect(safe?.providers.length).toBe(5);
  expect(JSON.stringify(safe)).not.toContain(REFLECTED);
  expect(JSON.stringify(safe)).not.toContain(SYNTHETIC);
  expect(parseLocalWebSearch({ ...wire, providers: wire.providers.slice(1) })).toBe(null);
});
