import { expect, open, test } from "nsh/test";
import type { App } from "nsh/test";
import { BASE, header, world } from "../../test/fakeServer";
import { launched } from "../../test/launched";
import { BRIDGE, BRIDGE_TOKEN, bridgeEnv, fakeBridge } from "../device/test/fakeBridge";
import { parseLocalWebSearch } from "../device/webSearch";

const OTHER = BASE.replace("8787", "8788");
const PATH = "/v1/web-search";
const SYNTHETIC = "fixture-only-value-not-a-real-key";
const REFLECTED = "fixture-only-private-diagnostic";
type Req = { method: string; url: string; headers: Record<string, string>; body?: string | null };
type Row = ReturnType<typeof rows>[number];
const defaults = ["tavily", "exa", "parallel", "firecrawl", "tinyfish"];
const rows = () => defaults.map((id) => ({
  id, enabled: true, keyless: id !== "tinyfish", key_saved: false, key_status: null as string | null,
  cooldown: null as null | { until: string; mode: string; outcome: string },
}));
function device() {
  return {
    enabled: true, notice_acknowledged: false, providers: rows(),
    device: { paired: true, worker: "online", detail: null as string | null },
    keys_error: null as string | null, settings_error: null as string | null, endpoints_error: null as string | null,
  };
}

function fixture() {
  const f = world();
  const b = fakeBridge(f);
  const alice = device();
  // Alice has explicitly moved Exa to the top, not the connector default.
  alice.providers = [alice.providers[1]!, alice.providers[0]!, ...alice.providers.slice(2)];
  const bob = device();
  bob.enabled = false;
  bob.device.worker = "stopped";
  const other = device();
  other.providers.reverse();
  const states = new Map([[`${BASE} alice`, alice], [`${BASE} bob`, bob], [`${OTHER} alice`, other]]);
  const calls: { method: string; path: string; binding: string; providers?: string[]; keyAccepted?: boolean; generation?: number; prefsBody?: unknown }[] = [];
  const lifecycle: { method: string; previous: number | null; next: number | null }[] = [];
  const intentEvents: { action: string; id: string }[] = [];
  let nextIntent = 0;
  let activeIntent: string | null = null;
  let generation = 0;
  let workerGeneration: number | null = null;
  const chatWrites: { host: string; enabled: boolean }[] = [];
  const prefs = new Map<string, boolean>();
  const replies = { unsupported: false, oldServer: false, testRefused: false, staleTest: false, rejectKey: false, refusePrefs: false,
    refuseOrder: false, writeDelay: 0, testDelay: 0, bindDelay: 0, unbindDelay: 0, failBind: false, failUnbind: false, noGeneration: false,
    bindIntents: false };
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
      const view = () => state.settings_error ? { ...state, enabled: false, providers: rows() } : state;
      if (req.method === "GET") return ok(view());
      const body = JSON.parse(req.body!);
      record.prefsBody = body;
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
      return ok(view(), replies.writeDelay);
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
    if (action === "test") {
      record.generation = JSON.parse(req.body!).session_generation;
      if (record.generation !== generation) return err(409, "no_session");
      if (replies.staleTest) return err(409, "test_not_applied");
      return replies.testRefused ? err(409, "not_sent") : ok({ ...row, test: { outcome: "ok" } }, replies.testDelay);
    }
    if (action === "clear-cooldown") {
      if (state.settings_error?.startsWith("Saved provider pauses")) {
        state.settings_error = null;
      }
      row.cooldown = null;
      row.key_status = row.key_saved ? "ok" : null;
      return ok(row);
    }
    return err(404, "not_found");
  }
  const network = { ...f.network, ...b.network };
  for (const method of ["GET", "PUT", "POST", "DELETE"]) {
    network[`${method} ${BRIDGE}/v1/*`] = (req: Req) => {
      const path = new URL(req.url).pathname;
      if (path.startsWith(PATH)) return web(req);
      if (path === "/v1/session/intent" && replies.bindIntents) {
        if (header(req, "authorization") !== `Bearer ${BRIDGE_TOKEN}`) return err(401, "unauthorized");
        if (method === "POST") {
          activeIntent = (++nextIntent).toString(16).padStart(64, "0");
          intentEvents.push({ action: "reserve", id: activeIntent });
          return ok({ intent: activeIntent });
        }
        if (method === "DELETE") {
          const id = JSON.parse(req.body!).intent;
          intentEvents.push({ action: "cancel", id });
          if (id === activeIntent) {
            activeIntent = null;
            workerGeneration = null;
          }
          return { status: 204, body: "" };
        }
      }
      if (path === "/v1/session" && method === "POST" && replies.failBind) return { error: "fixture offline" };
      if (path === "/v1/session" && method === "DELETE" && replies.failUnbind) return { error: "fixture offline" };
      if (path === "/v1/session" && method === "POST" && replies.bindIntents) {
        const id = JSON.parse(req.body!).intent;
        if (!activeIntent || id !== activeIntent) return err(409, "bind_cancelled");
        intentEvents.push({ action: "bind", id });
      }
      const answer = b.network[`${method} ${BRIDGE}/v1/*`]!(req) as {
        status: number; body: { session?: { generation?: number } | null };
      };
      const previous = workerGeneration;
      if (path === "/v1/session" && answer.status === 200) {
        if (method === "POST") workerGeneration = ++generation;
        if (method === "DELETE") workerGeneration = null;
        lifecycle.push({ method, previous, next: workerGeneration });
      }
      const session = answer.body.session;
      return {
        ...answer,
        body: session && !replies.noGeneration ? { ...answer.body, session: { ...session, generation } } : answer.body,
        delay: path !== "/v1/session" ? 0 : method === "POST" ? replies.bindDelay : replies.unbindDelay,
      };
    };
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
  return { network, calls, chatWrites, replies, alice, bob, other, lifecycle, intentEvents, worker: () => workerGeneration };
}

async function start(w: ReturnType<typeof fixture>, bridge = true, size = { width: 1280, height: 1000 }) {
  const app = await open("./WebSearchSettings.fixture.tsx", {
    modules: (bridge ? bridgeEnv() : launched()) as any, network: w.network as any,
    ...size, platform: "linux", clock: {}, reducedMotion: true,
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
  expect(text(app)).toContain("Key required");
  expect(text(app)).toContain("No key");
  await app.press("id:websearch-enabled:exa");
  await app.waitFor("text:Exa is off.");
  await app.press("label:Move Exa down");
  await app.waitFor("text:Exa moved to position 2 of 5.");
  expect(w.calls.filter((c) => c.method === "PUT").at(-1)?.providers).toEqual(defaults);
  expect(w.alice.providers.find((p) => p.id === "exa")?.enabled).toBe(false);
  w.replies.refuseOrder = true;
  await app.press("id:websearch-up:exa");
  await app.waitFor("text:/couldn't confirm the web search change/");
  expect(app.enabled("id:websearch-up:exa")).toBe(true);
  expect(text(app)).not.toContain(REFLECTED);
  w.alice.providers.forEach((row) => { row.enabled = false; });
  await app.press("id:websearch-refresh");
  await app.waitFor("id:websearch-all-off");
  expect(text(app)).toContain("All providers are off on this computer.");
});

test("Web search describes device selection and privacy with no disclosure required at minimum size", async () => {
  const w = fixture();
  w.alice.device.worker = "offline";
  const app = await start(w, true, { width: 720, height: 480 });
  await app.waitFor("id:websearch-worker >> text:Offline");
  expect(text(app)).toContain("This switch and these provider settings apply only to this computer.");
  expect(text(app)).toContain("For runs you start, Metro selects your most recently seen online web device on this server for each permitted search or page read.");
  expect(text(app)).toContain("If none of your web devices is online");
  expect(text(app)).toContain("After setup, it may receive your query or page URLs.");
  expect(text(app)).toContain("Web keys are stored on the device and never uploaded to the Metro server.");
  expect(text(app)).toContain("The Metro server relays requests and limited results.");
  expect(text(app)).toContain("Activity records accepted queries, URLs and options, plus summaries and provider outcomes.");
  expect(text(app)).toContain("Metro doesn't keep or replay that text as tool history");
  expect(text(app)).toContain("Don't give agents private links or URLs that contain credentials.");
  await app.press("id:websearch-learn:toggle");
  await app.waitFor("text:/doesn't check where DNS or provider redirects lead/");
  await app.press("label:Open About & updates › Privacy");
  expect(text(app)).toContain("Privacy opened");
});

test("master notice and Chat consent are separate, with no optimistic or cross-server write", async () => {
  const w = fixture();
  const app = await start(w);
  await app.waitFor("id:websearch-notice");
  await app.waitFor("id:websearch-chat-opt-in");
  expect(text(app)).toContain("third-party providers");
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
  expect(text(app)).toContain("A key goes only to its provider, which may charge your account.");
  expect(text(app)).not.toContain("higher limits");
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
  await app.waitFor("text:/this provider will run without a key/");
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
  w.alice.endpoints_error = REFLECTED;
  w.alice.enabled = true;
  await app.press("id:websearch-refresh");
  await app.waitFor("id:websearch-pauses-error");
  expect(app.shows("id:websearch-reset-settings")).toBe(false);
  await app.press("id:websearch-turn-off-paused");
  await app.waitFor("text:Web search is off on this computer.");
  expect(w.alice.enabled).toBe(false);
  expect(w.alice.device.worker).toBe("stopped");
  expect(w.alice.settings_error).not.toBe(null);
  expect(w.calls.filter((r) => r.method === "PUT").at(-1)?.prefsBody).toEqual({ enabled: false });
  expect(w.calls.some((r) => r.path.endsWith("/clear-cooldown"))).toBe(false);
  w.alice.endpoints_error = null;
  await app.press("id:websearch-refresh");
  await item(app, "clear");
  await app.waitFor("id:websearch-pauses-error", { state: "gone" });
  expect(w.alice.enabled).toBe(false);
  expect(text(app)).not.toContain(REFLECTED);
  expect(w.calls.filter((r) => r.method !== "GET").map((r) => r.path)).toEqual([PATH, `${PATH}/keys/reset`, PATH, `${PATH}/providers/exa/clear-cooldown`]);
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
  w.replies.staleTest = true;
  await item(app, "test");
  await app.waitFor("text:/The provider test result was not saved/");
  expect(text(app)).not.toContain(REFLECTED);
  expect(app.shows("text:Exa test: worked.")).toBe(false);
  w.replies.staleTest = false;
  await item(app, "test");
  await app.waitFor("text:Exa test: worked.");
  await item(app, "clear");
  await app.waitFor("text:/Exa is no longer paused/");
  expect(w.calls.filter((r) => r.method === "POST").map((r) => r.path)).toEqual([
    `${PATH}/providers/exa/test`, `${PATH}/providers/exa/test`, `${PATH}/providers/exa/test`, `${PATH}/providers/exa/clear-cooldown`,
  ]);
});

test("an off provider cannot be tested from Settings", async () => {
  const w = fixture();
  const app = await start(w);
  await app.waitFor("id:websearch-enabled:exa");
  await app.press("id:websearch-enabled:exa");
  await app.waitFor("text:Exa is off.");
  await app.press("id:websearch-menu:exa");
  await app.waitFor("text:Enable provider to test");
  expect(app.enabled("id:websearch-test:exa")).toBe(false);
  expect(w.calls.some((r) => r.path.endsWith("/test"))).toBe(false);
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

test("sign-out cleans a dispatched bind even when its response has not arrived", async () => {
  const w = fixture();
  w.replies.bindDelay = 3000;
  const app = await start(w);
  await app.waitUntil(() => w.worker() === 1, { timeout: 500 });
  expect(app.shows("id:websearch-loading")).toBe(true);
  await app.press("id:fixture-signout");
  expect(w.lifecycle.map((action) => action.method)).toEqual(["POST"]);
  await app.waitUntil(() => w.lifecycle.length === 2);
  expect(w.lifecycle[1]).toEqual({ method: "DELETE", previous: 1, next: null });
  expect(w.worker()).toBe(null);
  expect(w.calls).toEqual([]);
});

test("replacement sign-in to the same account waits for held bind and cleanup, with no old worker", async () => {
  const w = fixture();
  w.replies.bindDelay = 2500;
  w.replies.unbindDelay = 3000;
  const app = await start(w);
  await app.waitUntil(() => w.worker() === 1, { timeout: 500 });
  w.replies.bindDelay = 0; // Only the first POST is held.
  await app.press("id:fixture-alice");
  expect(w.lifecycle.map((action) => action.method)).toEqual(["POST"]);
  await app.waitUntil(() => w.lifecycle.length === 2, { timeout: 4000 });
  expect(w.worker()).toBe(null);
  await app.clock!.runFor(1000);
  expect(w.lifecycle.map((action) => action.method)).toEqual(["POST", "DELETE"]);
  expect(w.calls).toEqual([]);
  await app.waitFor("id:websearch-provider:exa");
  expect(w.lifecycle).toEqual([
    { method: "POST", previous: null, next: 1 },
    { method: "DELETE", previous: 1, next: null },
    { method: "POST", previous: null, next: 2 },
  ]);
});

test("supported helpers receive targeted cancellation before the pending bind returns", async () => {
  const w = fixture();
  w.replies.bindIntents = true;
  w.replies.bindDelay = 3000;
  const app = await start(w);
  await app.waitUntil(() => w.worker() === 1, { timeout: 500 });
  w.replies.bindDelay = 0;
  await app.press("id:fixture-alice");
  await app.waitUntil(() => w.intentEvents.length === 3, { timeout: 500 });
  const first = w.intentEvents[0]!.id;
  expect(w.intentEvents).toEqual([
    { action: "reserve", id: first }, { action: "bind", id: first }, { action: "cancel", id: first },
  ]);
  expect(w.worker()).toBe(null);
  expect(w.lifecycle.map((event) => event.method)).toEqual(["POST"]);
  await app.waitFor("id:websearch-provider:exa");
  expect(w.lifecycle.map((event) => event.method)).toEqual(["POST", "DELETE", "POST"]);
  const second = w.intentEvents[3]!.id;
  expect(second === first).toBe(false);
  expect(w.intentEvents.slice(3)).toEqual([{ action: "reserve", id: second }, { action: "bind", id: second }]);
  expect(w.worker()).toBe(2);
});

test("a failed cleanup must be confirmed before a replacement session can bind", async () => {
  const w = fixture();
  const app = await start(w);
  await app.waitFor("id:websearch-provider:exa");
  w.replies.failUnbind = true;
  await app.press("id:fixture-alice");
  await app.waitFor("text:/isn't serving this account on this server/");
  expect(w.lifecycle.map((action) => action.method)).toEqual(["POST"]);
  w.replies.failUnbind = false;
  await app.press("id:fixture-alice");
  await app.waitFor("id:websearch-provider:exa");
  expect(w.lifecycle.map((action) => action.method)).toEqual(["POST", "DELETE", "POST"]);
  expect(w.worker()).toBe(2);
});

test("a held provider-test reply neither blocks switching nor publishes to the new account", async () => {
  const w = fixture();
  const app = await start(w);
  await app.waitFor("id:websearch-provider:exa");
  w.replies.testDelay = 5000;
  await item(app, "test");
  await app.waitUntil(() => w.calls.some((r) => r.path.endsWith("/test")), { timeout: 500 });
  expect(w.calls.at(-1)?.generation).toBe(1);
  await app.press("id:fixture-bob");
  await app.waitUntil(() => w.worker() === 2, { timeout: 1000 });
  await app.waitFor("id:websearch-provider:exa", { timeout: 1000 });
  expect(w.lifecycle.map((action) => action.method)).toEqual(["POST", "DELETE", "POST"]);
  expect(w.calls.at(-1)?.binding).toBe(`${BASE} bob`);
  const observed = w.calls.length;
  await app.clock!.runFor(5000);
  expect(w.calls.length).toBe(observed);
  expect(text(app)).not.toContain("Exa test:");
  expect(app.shows("id:websearch-status")).toBe(false);
});

test("initial bind failure tells you to sign in again rather than offering a no-op Refresh", async () => {
  const w = fixture();
  w.replies.failBind = true;
  const app = await start(w);
  await app.waitFor("text:/sign out and sign in again/");
  expect(app.shows("id:websearch-refresh")).toBe(false);
  expect(w.calls).toEqual([]);
  w.replies.failBind = false;
  await app.press("id:fixture-signout");
  await app.press("id:fixture-alice");
  await app.waitFor("id:websearch-provider:exa");
});

test("an older bridge without a generation can't dispatch provider tests", async () => {
  const w = fixture();
  w.replies.noGeneration = true;
  const app = await start(w);
  await app.waitFor("text:Update Metro to test providers.");
  await app.press("id:websearch-menu:exa");
  expect(app.enabled("id:websearch-test:exa")).toBe(false);
  expect(w.calls.some((r) => r.path.endsWith("/test"))).toBe(false);
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
