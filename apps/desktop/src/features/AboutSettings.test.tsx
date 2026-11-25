import { expect, open, test } from "nsh/test";
import type { App } from "nsh/test";
import { BRIDGE, BRIDGE_TOKEN, bridgeEnv, fakeBridge } from "../device/test/fakeBridge";
import { createOutcomeDismissal, officialReleaseUrl } from "../updates";
import { BASE, header, TOKENS, world } from "../../test/fakeServer";
import { launched } from "../../test/launched";

const CURRENT = "0.5.0";
const OTHER = "http://127.0.0.1:8788";
const NEXT = "0.5.1";
const AFTER = "0.6.0";
const RELEASE = `https://github.com/wnzn/metro/releases/tag/v${NEXT}`;
const INERT = "[Steal](https://untrusted.invalid) <script>bad()</script> ![image](https://untrusted.invalid/image)";
const OUTCOME_DISMISSAL_KEY = "metro.update.dismissedOutcome";
const latest = (version = NEXT) => ({
  version, signed_at: 123, commit: "a".repeat(40), release_url: `https://github.com/wnzn/metro/releases/tag/v${version}`,
  notes: INERT, notes_truncated: true,
});
const lastAt = 1_800_000_000_000;
const available = () => ({ latest: latest(), result: "available", last_checked_at: lastAt - 1 });
type Req = { method: string; url: string; headers: Record<string, string>; body?: string | null };
const ok = (body: unknown) => ({ status: 200, body });
const err = (status: number, code: string) => ({ status, body: { error: { code, message: "private detail never displayed" } } });

function scenario(overrides: Record<string, unknown> = {}) {
  const f = world();
  const bridge = fakeBridge(f);
  const calls: { step: string; body: unknown; auth: boolean; json: boolean }[] = [];
  let view = {
    current: { version: CURRENT, channel: "release", install: "linux-installer" },
    can_check: true, can_update_in_app: true, auto_check: true, last_checked_at: null,
    result: "never", phase: "idle", busy: { serving_connections: 0, local_workspace: false },
    ...overrides,
  } as Record<string, any>;
  const fixture = {
    f, calls, readDelay: 0, readError: null as null | { status: number; body: unknown },
    check: null as null | ((body: any) => unknown),
    download: null as null | ((body: any) => unknown),
    apply: null as null | ((body: any) => unknown),
    update(patch: Record<string, unknown>) { view = { ...view, ...patch }; },
    steps: () => calls.filter(call => call.step !== "read").map(call => call.step),
    count: (step: string) => calls.filter(call => call.step === step).length,
    network: { [`GET ${BASE}/v1/server/update`]: (_req: Req): unknown => err(404, "not_found") } as Record<string, (req: any) => unknown>,
  };
  const handle = (step: string, req: Req) => {
    const body = req.body ? JSON.parse(req.body) : undefined;
    const auth = header(req, "authorization") === `Bearer ${BRIDGE_TOKEN}`;
    const json = header(req, "content-type") === "application/json";
    calls.push({ step, body, auth, json });
    if (!auth || (step !== "read" && !json)) return err(403, "unauthorized");
    switch (step) {
      case "read": return { ...(fixture.readError ?? ok(view)), delay: fixture.readDelay };
      case "started": return ok({ trial: true });
      case "check": return fixture.check?.(body) ?? ok(view);
      case "settings": fixture.update({ auto_check: body.auto_check }); return ok(view);
      case "download":
        if (fixture.download) return fixture.download(body);
        fixture.update({ phase: "downloading", progress: { done: 0, total: 0 } });
        return ok(view);
      case "cancel": return ok(view); // launcher acknowledges before cancellation settles
      case "apply":
        if (fixture.apply) return fixture.apply(body);
        if (!body.confirm_interrupt && (view.busy.serving_connections || view.busy.local_workspace)) return err(409, "would_interrupt");
        fixture.update({ phase: "applying" });
        return ok(view);
      default: return err(404, "not_found");
    }
  };
  for (const step of ["read", "started", "check", "settings", "download", "cancel", "apply"]) {
    const method = step === "read" ? "GET" : step === "settings" ? "PUT" : "POST";
    fixture.network[`${method} ${BRIDGE}/v1/update${step === "read" ? "" : `/${step}`}`] = req => handle(step, req);
  }
  Object.assign(fixture.network, bridge.network, f.network); // The NSH stub matches the first route.
  return fixture;
}

type Fixture = ReturnType<typeof scenario>;
const text = (app: App) => app.texts().join(" | ");
async function harness(f: Fixture, showAbout = true, modules = bridgeEnv(), platform: "linux" | "windows" = "linux", storage?: Record<string, string>) {
  const app = await open("../../test/AboutHarness.tsx", { modules, network: f.network as any, width: 1000, height: 840, platform, storage });
  if (showAbout) await app.press("id:h-about");
  return app;
}
async function download(app: App) {
  await app.press("id:update-start");
  await app.press("id:update-start:confirm");
}

test("layout plus a supported bridge starts the health handshake once, without opening About", async () => {
  const f = scenario(available());
  f.readDelay = 700;
  const app = await open("../../App.tsx", { modules: bridgeEnv(), network: f.network as any, platform: "linux" });
  await app.waitFor("id:connect-screen");
  expect(app.shows("id:about-settings")).toBe(false);
  expect(f.count("started")).toBe(0);
  await app.waitUntil(() => f.count("started") === 1);
  expect(f.calls.find(call => call.step === "started")).toEqual({ step: "started", body: {}, auth: true, json: true });
  await app.clear("id:server-url");
  await app.type("id:server-url", BASE);
  await app.type("id:access-token", TOKENS.alice);
  await app.press("id:connect");
  await app.waitFor("id:workspace");
  await app.press("id:account-menu");
  await app.press("id:disconnect");
  await app.waitFor("id:connect-screen");
  expect(f.steps()).toEqual(["started"]);
});

test("reading About before layout can show an offer, but cannot acknowledge trial health", async () => {
  const f = scenario(available());
  const app = await harness(f);
  await app.waitFor("id:h-update-available");
  expect(f.steps()).toEqual([]);
  await app.press("id:h-about");
  await app.press("id:h-render");
  await app.waitFor("text:App rendered");
  await app.waitUntil(() => f.count("started") === 1);
  await app.press("id:h-about");
  await app.waitFor("id:update-start");
  expect(f.steps()).toEqual(["started"]);
});

test("source, preview and unsupported builds never check or send trial health", async () => {
  for (const example of [
    { channel: "source", id: "update-source-build", words: "This is a source build" },
    { channel: "preview", id: "update-unsupported", words: "Preview builds don't check" },
    { channel: "release", id: "update-unsupported", words: "on this platform" },
  ]) {
    const f = scenario({ ...available(), current: { version: CURRENT, channel: example.channel, install: "unsupported" }, can_check: false, can_update_in_app: false });
    const app = await harness(f);
    await app.press("id:h-render");
    await app.waitFor(`id:${example.id}`);
    expect(text(app)).toContain(example.words);
    for (const id of ["update-check", "update-auto", "update-start", "h-update-available"]) expect(app.shows(`id:${id}`)).toBe(false);
    expect(f.steps()).toEqual([]);
  }
});

test("secondary and older launcher responses do not claim support or send health", async () => {
  for (const example of [
    { error: err(503, "update_unavailable"), words: "first Metro window" },
    { error: { status: 404, body: "old launcher" }, words: "doesn't report update status" },
  ]) {
    const f = scenario();
    f.readError = example.error;
    const app = await harness(f);
    await app.press("id:h-render");
    await app.waitFor("id:update-unavailable");
    expect(text(app)).toContain(example.words);
    expect(f.steps()).toEqual([]);
  }
  const f = scenario();
  const app = await harness(f, true, launched());
  await app.waitFor("id:update-unavailable");
  expect(text(app)).toContain("isn't running through the desktop launcher");
  expect(f.calls).toHaveLength(0);
});

test("portable, legacy Linux, Windows ZIP-installed and Windows MSI copies are manual only", async () => {
  for (const example of [
    { install: "portable", words: "This is a portable copy", reason: "portable" },
    { install: "linux-installer", words: "Run the current Metro Linux installer once", reason: "manual_migration_required" },
    { install: "windows-script", words: "Use the Windows install script", reason: "manual_update_required" },
    { install: "windows-msi", words: "The Windows preview MSI is unsigned", reason: "manual_update_required" },
  ]) {
    // Even a bridge with an incorrect "ready" and can_update_in_app is manual.
    const f = scenario({ ...available(), current: { version: CURRENT, channel: "release", install: example.install }, phase: "ready", reason: example.reason });
    const app = await harness(f);
    await app.press("id:h-render");
    await app.waitFor("id:update-download-page");
    expect(text(app)).toContain(example.words);
    expect(app.shows("id:update-start")).toBe(false);
    expect(app.shows("id:update-restart")).toBe(false);
    expect(f.steps()).toEqual([]);
  }
  const inconsistent = scenario({ ...available(), phase: "ready" });
  const windows = await harness(inconsistent, true, bridgeEnv(), "windows");
  await windows.press("id:h-render");
  await windows.waitFor("id:update-download-page");
  expect(windows.shows("id:update-restart")).toBe(false);
  expect(inconsistent.steps()).toEqual([]);
});

test("check preferences save on the device; a manual check shows offline without a cached offer", async () => {
  const f = scenario(available());
  f.check = () => ok({ ...available(), current: { version: CURRENT, channel: "release", install: "linux-installer" },
    can_check: true, can_update_in_app: true, auto_check: false, result: "error", error: "offline", phase: "idle" });
  const app = await harness(f);
  await app.waitFor("id:update-start");
  await app.press("id:update-auto");
  await app.waitUntil(() => f.count("settings") === 1);
  expect(f.calls.find(c => c.step === "settings")?.body).toEqual({ auto_check: false });
  await app.press("id:update-check");
  await app.waitFor("text:/Couldn't reach GitHub/");
  expect(app.shows("id:update-start")).toBe(false);
  expect(app.shows("id:h-update-available")).toBe(false);
  expect(f.steps()).toEqual(["settings", "check"]);
});

test("notes render as inert text and the tag link must exactly match the official release URL", async () => {
  const f = scenario({ ...available(), latest: { ...latest(), release_url: "https://github.com.untrusted.invalid/wnzn/metro/releases/tag/v0.5.1" } });
  const app = await harness(f);
  await app.waitFor("id:update-notes:toggle");
  await app.press("id:update-notes:toggle");
  await app.waitFor("id:update-notes-text");
  expect(text(app)).toContain(INERT);
  expect(text(app)).toContain("These notes were shortened.");
  expect(app.shows("id:update-release-link")).toBe(false);
  expect(f.steps()).toEqual([]);
  expect(officialReleaseUrl(NEXT, RELEASE)).toBe(RELEASE);
  for (const link of [
    RELEASE.replace("https:", "http:"), RELEASE.replace("github.com", "github.com:443"), `${RELEASE}?next=x`, `${RELEASE}#x`,
    RELEASE.replace("github.com", "github.com@untrusted.invalid"), RELEASE.replace(NEXT, AFTER), `${RELEASE}\n`,
  ]) expect(officialReleaseUrl(NEXT, link)).toBe(undefined);
});

test("managed Linux downloads only after consent and restarts only after verified readiness", async () => {
  const f = scenario(available());
  const app = await harness(f);
  await app.press("id:h-render");
  await app.waitFor("id:update-start");
  await app.press("id:update-start");
  expect(f.count("download")).toBe(0);
  await app.press("id:update-start:cancel");
  await download(app);
  expect(f.calls.find(c => c.step === "download")?.body).toEqual({ version: NEXT });
  await app.waitFor("label:/Downloading Metro 0.5.1/");
  expect(app.shows("id:update-restart")).toBe(false);
  f.update({ progress: { done: 42, total: 100 } });
  await app.waitFor("text:/42%/");
  f.update({ phase: "verifying" });
  await app.waitFor("label:/Checking the signature and checksum/");
  expect(app.shows("id:update-restart")).toBe(false);
  f.update({ phase: "staging" });
  await app.waitFor("label:/Preparing the verified update/");
  expect(app.shows("id:update-cancel")).toBe(false);
  f.update({ phase: "ready" });
  await app.waitFor("id:update-restart");
  await app.press("id:update-later");
  expect(f.count("apply")).toBe(0);
  await app.press("id:update-show-restart");
  await app.press("id:update-restart");
  await app.waitFor("text:/Metro is restarting/");
  expect(f.calls.filter(c => c.step === "apply").map(c => c.body)).toEqual([{ version: NEXT, confirm_interrupt: false }]);
});

test("apply rechecks busy work at click time; Cancel and explicit Restart anyway are distinct", async () => {
  const f = scenario({ ...available(), phase: "ready" });
  const app = await harness(f);
  await app.waitFor("id:update-restart");
  f.update({ busy: { serving_connections: 2, local_workspace: true } });
  await app.press("id:update-restart");
  await app.waitFor("id:update-interrupt:cancel");
  await app.waitFor("text:/2 device connections and the local workspace/");
  await app.press("id:update-interrupt:cancel");
  expect(f.count("apply")).toBe(1);
  await app.press("id:update-restart");
  await app.press("id:update-interrupt:confirm");
  await app.waitFor("text:/Metro is restarting/");
  expect(f.calls.filter(c => c.step === "apply").map(c => c.body)).toEqual([
    { version: NEXT, confirm_interrupt: false }, { version: NEXT, confirm_interrupt: false }, { version: NEXT, confirm_interrupt: true },
  ]);
});

test("cancelling waits for the launcher to settle without claiming a completed install", async () => {
  const f = scenario(available());
  const app = await harness(f);
  await app.press("id:h-render");
  await download(app);
  await app.press("id:update-cancel");
  await app.waitFor("label:/Cancelling/");
  expect(f.calls.find(c => c.step === "cancel")?.body).toEqual({});
  expect(app.shows("id:update-restart")).toBe(false);
  f.update({ phase: "idle", progress: null });
  await app.waitFor("text:/Download cancelled. No update was installed/");
  expect(f.count("apply")).toBe(0);
});

test("verification and installer exclusion failures don't enable restart, and checking gives a fresh offer", async () => {
  const f = scenario({ ...available(), phase: "failed", failure: { code: "not_verified", message: "private detail never displayed" } });
  const app = await harness(f);
  await app.press("id:h-render");
  await app.waitFor("text:/signature or checksum didn't pass verification/");
  expect(app.shows("id:update-start")).toBe(false);
  expect(app.shows("id:update-restart")).toBe(false);
  expect(text(app)).not.toContain("private detail");
  f.update({ failure: { code: "installer_busy" } });
  await app.waitFor("text:/Another installation or update/");
  f.update({ phase: "idle", anomaly: "older_release_served" });
  await app.waitFor("text:/older release than Metro already verified/");
  expect(app.shows("id:update-start")).toBe(false);
  f.check = () => {
    f.update({ phase: "idle", anomaly: undefined, latest: latest(AFTER), last_checked_at: lastAt + 1 });
    return undefined;
  };
  await app.press("id:update-check");
  await app.waitFor(`text:Update to ${AFTER}…`);
  expect(f.count("download")).toBe(0);
});

test("rollback and backup exclusion show safe facts and do not offer the old stage", async () => {
  for (const last_update of [
    { from: CURRENT, to: NEXT, at: lastAt, outcome: "rolled_back", detail: "private path" },
    { from: CURRENT, to: NEXT, at: lastAt, outcome: "install_failed", failure_code: "backup_failed" },
  ]) {
    const f = scenario({ ...available(), phase: "ready", last_update });
    const app = await harness(f);
    await app.waitFor("id:update-outcome");
    expect(text(app)).toContain(last_update.outcome === "rolled_back" ? `Metro went back to ${CURRENT}` : "Metro couldn't back up the local workspace.");
    expect(text(app)).not.toContain("private path");
    expect(app.shows("id:update-restart")).toBe(false);
    expect(app.shows("id:update-start")).toBe(false);
    await app.press("id:update-dismiss-outcome");
    await app.waitFor("id:update-outcome", { state: "gone" });
    expect(app.shows("id:update-restart")).toBe(false);
  }
});

test("a refused download does not hide an independent newer offer returned by the next GET", async () => {
  const f = scenario(available());
  f.download = () => {
    f.update({ latest: latest(AFTER), last_checked_at: lastAt + 1 });
    return err(409, "not_latest");
  };
  const app = await harness(f);
  await app.press("id:h-render");
  await app.waitFor("id:update-start");
  await download(app);
  await app.waitFor(`text:Update to ${AFTER}…`);
  expect(app.shows("id:h-update-available")).toBe(true);
  expect(app.shows("id:update-action-error")).toBe(false);
  expect(f.calls.filter(c => c.step === "download").map(c => c.body)).toEqual([{ version: NEXT }]);
  expect(f.count("check")).toBe(0);
  expect(f.count("apply")).toBe(0);
});

test("a busy refusal reads fresh status before an old poll returns, and that poll cannot retire a newer read", async () => {
  const f = scenario({ ...available(), phase: "ready", stage_revision: "1".repeat(64) });
  const app = await open("../../test/AboutHarness.tsx", { modules: bridgeEnv(), network: f.network as any,
    clock: {}, width: 1000, height: 840, platform: "linux" });
  await app.press("id:h-about");
  await app.waitFor("id:update-restart");

  const initialReads = f.count("read");
  f.update({ phase: "idle" });
  f.readDelay = 2_500;
  await app.press("id:h-poll-updater");
  await app.waitUntil(() => f.count("read") === initialReads + 1, { timeout: 400 });
  f.readDelay = 0;
  f.update({ phase: "ready", busy: { serving_connections: 2, local_workspace: true } });
  await app.press("id:update-restart");
  await app.waitFor("text:/2 device connections and the local workspace/", { timeout: 400 });
  expect(app.shows("id:update-interrupt:confirm")).toBe(true);
  expect(f.count("read")).toBe(initialReads + 2);

  // A third read remains pending when the old idle snapshot is delivered.
  f.readDelay = 4_200;
  await app.press("id:h-poll-updater");
  await app.waitUntil(() => f.count("read") === initialReads + 3, { timeout: 400 });
  await app.clock!.runFor(2_600);
  expect(app.shows("id:update-interrupt:confirm")).toBe(true);
  await app.press("id:h-poll-updater");
  expect(f.count("read")).toBe(initialReads + 3);
  await app.press("id:update-interrupt:confirm");
  await app.waitFor("text:/Metro is restarting/");
  expect(f.calls.filter(c => c.step === "apply").map(c => c.body)).toEqual([
    { version: NEXT, confirm_interrupt: false }, { version: NEXT, confirm_interrupt: true },
  ]);
});

test("a ready refusal blocks the same stage on refresh, not a distinct stage", async () => {
  const f = scenario({ ...available(), phase: "ready", stage_revision: "1".repeat(64) });
  f.apply = () => err(409, "not_ready");
  const app = await harness(f);
  await app.press("id:h-render");
  await app.waitFor("id:update-restart");
  const reads = f.count("read");
  await app.press("id:update-restart");
  await app.waitFor("id:update-action-error");
  await app.waitUntil(() => f.count("read") > reads);
  expect(app.shows("id:update-restart")).toBe(false);
  expect(app.shows("id:h-update-available")).toBe(false);
  f.update({ stage_revision: "2".repeat(64) });
  await app.waitFor("id:update-restart");
  expect(app.shows("id:h-update-available")).toBe(true);
  expect(f.calls.filter(c => c.step === "apply").map(c => c.body)).toEqual([{ version: NEXT, confirm_interrupt: false }]);
  expect(f.count("check")).toBe(0);
});

test("a verified check cannot re-offer a stage with a recorded verification failure", async () => {
  const f = scenario({ ...available(), phase: "idle",
    last_update: { from: CURRENT, to: NEXT, at: lastAt, outcome: "install_failed", failure_code: "stage_verification_failed",
      detail: "Untrusted prose: this was actually safe." } });
  const app = await harness(f);
  await app.press("id:h-render");
  await app.waitFor("id:update-recovery-release");
  expect(app.shows("id:update-start")).toBe(false);
  expect(text(app)).not.toContain("Untrusted prose");
  f.check = () => {
    f.update({ phase: "ready", last_checked_at: lastAt + 1, stage_revision: "1".repeat(64) });
    return undefined;
  };
  await app.press("id:update-check");
  await app.waitUntil(() => f.count("check") === 1);
  await app.waitFor("text:Check again");
  expect(app.shows("id:update-restart")).toBe(false);
  expect(app.shows("id:h-update-available")).toBe(false);
  await app.press("id:update-dismiss-outcome");
  await app.waitFor("id:update-outcome", { state: "gone" });
  expect(app.shows("id:update-recovery-release")).toBe(true);
  f.update({ phase: "idle", latest: latest(AFTER), last_checked_at: lastAt + 2 });
  await app.waitFor(`text:Update to ${AFTER}…`);
  expect(app.shows("id:h-update-available")).toBe(true);
  expect(f.count("apply")).toBe(0);
  expect(f.count("download")).toBe(0);
});

test("a dismissed outcome stays dismissed on relaunch, but a new attempt with the same version and result appears", async () => {
  const last = { from: CURRENT, to: NEXT, at: lastAt, outcome: "rolled_back", detail: "private path" };
  const f = scenario({ ...available(), last_update: last });
  const app = await harness(f);
  await app.waitFor("id:update-outcome");
  await app.press("id:update-dismiss-outcome");
  const dismissed = `${CURRENT}/${NEXT}/${lastAt}/rolled_back`;
  await app.waitUntil(() => app.storage()[OUTCOME_DISMISSAL_KEY] === dismissed);
  expect(JSON.stringify(app.storage())).not.toContain("private path");

  const next = await harness(f, true, bridgeEnv(), "linux", app.storage());
  await next.waitFor("id:update-status");
  expect(next.shows("id:update-outcome")).toBe(false);
  await next.press("id:h-render");
  f.update({ last_update: { ...last, at: lastAt + 1 } });
  await next.waitFor("id:update-outcome");
  expect(next.storage()[OUTCOME_DISMISSAL_KEY]).toBe(dismissed);
});

test("a held dismissal read hides only the outcome until storage says whether it was dismissed", async () => {
  const last = { from: CURRENT, to: NEXT, at: lastAt, outcome: "rolled_back" };
  const dismissed = `${CURRENT}/${NEXT}/${lastAt}/rolled_back`;
  const f = scenario({ ...available(), latest: latest(AFTER), last_update: last });
  const app = await open("../../test/AboutDismissalHarness.tsx", { modules: bridgeEnv(), network: f.network as any,
    storage: { [OUTCOME_DISMISSAL_KEY]: dismissed }, width: 1000, height: 840, platform: "linux" });
  await app.waitFor("id:h-storage-held");
  await app.waitFor(`text:Update to ${AFTER}…`);
  expect(app.shows("id:about-privacy")).toBe(true);
  expect(app.shows("id:update-check")).toBe(true);
  expect(app.shows("id:update-outcome")).toBe(false);
  await app.press("id:h-finish-storage");
  await app.waitFor("id:h-storage-settled");
  expect(app.shows("id:update-outcome")).toBe(false);

  f.update({ last_update: { ...last, at: lastAt + 1 } });
  await app.press("id:h-poll-updater");
  await app.waitFor("id:update-outcome");
  expect(app.storage()[OUTCOME_DISMISSAL_KEY]).toBe(dismissed);
});

test("an unreadable dismissal record releases the outcome notice without affecting About", async () => {
  const f = scenario({ ...available(), latest: latest(AFTER),
    last_update: { from: CURRENT, to: NEXT, at: lastAt, outcome: "rolled_back" } });
  const app = await open("../../test/AboutDismissalHarness.tsx", { modules: bridgeEnv(), network: f.network as any,
    width: 1000, height: 840, platform: "linux" });
  await app.waitFor("id:h-storage-held");
  await app.waitFor("id:update-start");
  expect(app.shows("id:update-outcome")).toBe(false);
  await app.press("id:h-fail-storage");
  await app.waitFor("id:update-outcome");
  expect(app.shows("id:update-start")).toBe(true);
  expect(text(app)).not.toContain("Fixture storage unavailable");
});

test("a slow initial dismissal read cannot undo a newer dismissal", async () => {
  const saved = new Map<string, string>();
  let complete!: (value: string) => void;
  const outcomeStore = createOutcomeDismissal({
    getItem: () => new Promise<string>(resolve => { complete = resolve; }),
    setItem: async (key, value) => { saved.set(key, value); },
  });
  const restoring = outcomeStore.restore();
  const dismissed = `${CURRENT}/${NEXT}/${lastAt}/rolled_back`;
  await outcomeStore.dismiss(dismissed);
  complete(`${CURRENT}/${NEXT}/${lastAt - 1}/rolled_back`);
  await restoring;
  expect(outcomeStore.current()).toBe(dismissed);
  expect(saved.get(OUTCOME_DISMISSAL_KEY)).toBe(dismissed);
});

test("newer offer invalidates an already-open restart confirmation", async () => {
  const f = scenario({ ...available(), phase: "ready", busy: { serving_connections: 1, local_workspace: false } });
  const app = await harness(f);
  await app.press("id:h-render");
  await app.waitFor("id:update-restart");
  await app.press("id:update-restart");
  await app.waitFor("id:update-interrupt:confirm");
  f.update({ latest: latest(AFTER), phase: "idle" });
  await app.waitFor(`text:Update to ${AFTER}…`);
  expect(app.shows("id:update-interrupt:confirm")).toBe(false);
  expect(f.calls.filter(c => c.step === "apply").map(c => c.body)).toEqual([{ version: NEXT, confirm_interrupt: false }]);
});

test("server update uses the admin API, hides historical offers and forgets data on account switch", async () => {
  const f = scenario({ can_check: false, can_update_in_app: false });
  const seen: boolean[] = [];
  let historical = true;
  let delay = 0;
  f.network[`GET ${BASE}/v1/server/update`] = req => {
    seen.push(header(req, "authorization") === `Bearer ${TOKENS.alice}`);
    return { ...ok({ current: "0.4.0", available: true, latest: { version: NEXT, historical, release_url: RELEASE } }), delay };
  };
  const app = await harness(f);
  await app.press("id:h-bob");
  await app.waitFor("text:bob");
  expect(seen).toEqual([]);
  expect(app.shows("id:about-server-update")).toBe(false);
  await app.press("id:h-alice");
  await app.waitFor("id:about-server-update");
  expect(seen.length).toBeGreaterThan(0);
  expect(seen.every(Boolean)).toBe(true);
  expect(text(app)).toContain("127.0.0.1:8787 · server 0.4.0");
  expect(app.shows("id:about-server-release")).toBe(false);
  await app.press("id:h-bob");
  await app.waitFor("text:bob");
  expect(app.shows("id:about-server-update")).toBe(false);
  expect(text(app)).toContain("version unknown");
  historical = false;
  await app.press("id:h-alice");
  await app.waitFor("id:about-server-release");
  await app.press("id:h-bob");
  await app.waitFor("text:bob");
  delay = 600;
  const before = seen.length;
  await app.press("id:h-alice");
  await app.waitUntil(() => seen.length > before);
  await app.press("id:h-bob");
  await app.waitFor("text:bob");
  await app.wait(700);
  expect(seen.every(Boolean)).toBe(true);
  expect(app.shows("id:about-server-update")).toBe(false);
  expect(text(app)).not.toContain("server 0.4.0");
});

test("switching servers with the same admin ID never displays the previous server's status", async () => {
  const f = scenario();
  let otherChecks = 0;
  f.network[`GET ${BASE}/v1/server/update`] = () => ok({ current: "0.4.0", available: true,
    latest: { version: NEXT, historical: false, release_url: RELEASE } });
  f.network[`GET ${OTHER}/v1/server/update`] = () => {
    ++otherChecks;
    return err(404, "not_found"); // An older server, even for an admin.
  };
  for (const method of ["GET", "POST", "PUT", "PATCH", "DELETE"]) {
    f.network[`${method} ${OTHER}/v1/*`] = req => f.f.network[`${method} ${BASE}/v1/*`]({ ...req, url: req.url.replace(OTHER, BASE) });
  }
  const app = await harness(f);
  await app.press("id:h-alice");
  await app.waitFor("id:about-server-release");
  await app.press("id:h-other-server");
  await app.waitUntil(() => otherChecks > 0);
  await app.waitFor("text:127.0.0.1:8788 · version unknown");
  expect(app.shows("id:about-server-update")).toBe(false);
  expect(text(app)).not.toContain("server 0.4.0");
});
