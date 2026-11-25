import { expect, open, test } from "nsh/test";
import type { App } from "nsh/test";
import { BASE, TOKENS } from "../../../test/fakeServer";
import { launched } from "../../../test/launched";
import { briefSpec, DRAFT, pagesFixture, PERSONAL, TEAM } from "./test/fixtureServer";
import { stamp } from "./test/sample";

type Fixture = ReturnType<typeof pagesFixture>;
async function signIn(app: App, user: string) {
  await app.clear("id:server-url");
  await app.type("id:server-url", BASE);
  await app.type("id:access-token", TOKENS[user]!);
  await app.press("id:connect");
  await app.waitFor("id:nav-pages");
}
async function start(f: Fixture, user = "alice", readProbe = false) {
  const app = await open(readProbe ? "../../../PagesReadProbe.fixture.tsx" : "../../../App.tsx", {
    modules: launched(), network: f.network as any, platform: "linux", width: 1280, height: 900, clock: {},
  });
  await signIn(app, user);
  return app;
}
async function jump(app: App, id = TEAM) {
  await app.press("id:nav-pages");
  await app.waitFor(`id:open-page:${id}`);
  await app.press(`id:open-page:${id}`);
  await app.waitFor("id:page-renderer");
}
const text = (app: App) => app.texts().join(" | ");
async function menu(app: App, id: string) {
  await app.press("id:page-menu");
  await app.press(`id:${id}`);
}

async function refreshMembership(app: App, f: Fixture, milliseconds = 10_100) {
  const requests = () => f.f.state.log.filter(r => r.method === "GET" && r.path === "/v1/spaces").length;
  const first = requests();
  await app.clock!.fastForward(milliseconds);
  await app.waitUntil(() => requests() > first);
}

async function chooseTeamScope(app: App) {
  await app.press("id:new-page-scope");
  await app.press("id:new-page-scope:space:acme");
}

async function pickFilesTemplate(app: App) {
  await app.press("id:new-page-templates");
  await app.press("id:page-template");
  await app.press("id:page-template:files");
}

test("Pages navigation, scoped list, pins and Space template creation", async () => {
  const f = pagesFixture();
  const app = await start(f);
  await app.press("id:space:acme");
  await app.press("id:space-pages:acme");
  await app.waitFor(`id:open-page:${TEAM}`);
  expect(app.shows(`id:open-page:${PERSONAL}`)).toBe(false);
  expect(app.shows(`id:pinned-page:${PERSONAL}`)).toBe(true);
  await app.press(`id:open-page:${TEAM}`);
  await app.waitFor("text:Team dashboard");
  await menu(app, "page-pin");
  await app.waitFor(`id:pinned-page:${TEAM}`);
  expect(f.calls(`/${TEAM}/prefs`, "PUT").map(r => r.body)).toEqual([{ pinned: true }]);
  await app.press("id:toggle-navigation");
  await app.press("id:rail-pages");
  await app.waitFor("id:pages-home");
  await app.press("id:toggle-navigation");
  await app.press("id:space:acme");
  await app.press("id:space-pages:acme");
  await app.waitFor("id:pages-home");
  await app.press("id:new-page");
  await app.press("id:new-page-templates");
  await app.press("id:page-template");
  await app.press("id:page-template:files");
  await app.press("id:create-page-template");
  await app.waitFor("text:Recent files");
  expect(f.calls("", "POST").map(r => r.body)).toEqual([{ scope: "space", space_id: "acme", template: "files" }]);
});

test("Routine Space sync preserves the template and explicit room picked in a sheet", async () => {
  const f = pagesFixture();
  f.f.addRoom("acme", { id: "releases", name: "Releases" });
  const app = await start(f);
  await app.press("id:nav-pages");
  await app.press("id:new-page");
  await chooseTeamScope(app);
  await pickFilesTemplate(app);
  await app.press("id:page-template-room");
  await app.press("text:Releases");
  await refreshMembership(app, f);
  await app.press("id:create-page-template");
  await app.waitFor("text:Recent files");
  expect(f.calls("", "POST")[0]?.body).toEqual({ scope: "space", space_id: "acme", template: "files", room: "Releases" });
});

async function spaceModelForm(f: Fixture) {
  const app = await start(f);
  await app.press("id:nav-pages");
  await app.press("id:new-page");
  await chooseTeamScope(app);
  await app.type("id:page-prompt", "Explicit model for this dashboard");
  await app.press("id:composer-model");
  await app.press("id:page-model-options");
  await app.press("text:gpt-fixture-1");
  return app;
}

test("Routine Space sync preserves an explicit page-builder model on the wire", async () => {
  const f = pagesFixture();
  const conn = f.f.addConnection("alice", "openai", "Fixture connection", "gpt-fixture-1");
  const app = await spaceModelForm(f);
  await refreshMembership(app, f);
  await app.press("id:page-generate");
  await app.waitFor("id:page-draft-view");
  expect(f.calls("/drafts", "POST")[0]?.body).toEqual({ scope: "space", space_id: "acme",
    prompt: "Explicit model for this dashboard", provider_connection_id: conn.id, model_id: "gpt-fixture-1" });
});

test("A removed explicit page-builder model blocks submission rather than sending with Default", async () => {
  const f = pagesFixture();
  const conn = f.f.addConnection("alice", "openai", "Fixture connection", "gpt-fixture-1");
  const app = await spaceModelForm(f);
  f.f.state.connections.delete(conn.id);
  await refreshMembership(app, f);
  await app.press("id:page-generate");
  await app.waitFor("text:/The selected model or thinking level is unavailable/");
  expect(f.calls("/drafts", "POST")).toHaveLength(0);
  await expect(app.press("id:page-generate")).rejects.toMatchObject({ code: "ERR_NSH_TEST_DISABLED" });
});

test("New page sheet recovers when a delayed create returns after Space access is lost", async () => {
  const f = pagesFixture();
  f.state.createDelay = 8000;
  const app = await start(f);
  await app.press("id:nav-pages");
  await app.press("id:new-page");
  await chooseTeamScope(app);
  await pickFilesTemplate(app);
  await app.clock!.fastForward(9_200);
  await app.press("id:create-page-template");
  await app.waitUntil(() => f.calls("", "POST").length === 1);
  f.f.state.spaces.get("acme")!.members.delete("alice");
  await refreshMembership(app, f, 2200);
  await app.waitFor("text:/You no longer have access to this Space/");
  await app.clock!.fastForward(6200);
  expect(app.shows("id:page-view")).toBe(false);
  await app.press("id:new-page-scope");
  await app.press("id:new-page-scope:personal");
  expect(app.shows("id:new-page-sheet")).toBe(true);
});

test("Recently deleted rows cannot revive on rejoin; a stale restore releases the sheet", async () => {
  const f = pagesFixture();
  f.state.restoreDelay = 8000;
  for (const id of [TEAM, PERSONAL]) f.entries.set(id, { ...f.entries.get(id)!, deleted_at: stamp });
  const app = await start(f);
  await app.press("id:nav-pages");
  await app.press("id:pages-deleted");
  await app.waitFor(`id:undelete-page:${TEAM}`);
  await app.clock!.fastForward(9200);
  await app.press(`id:undelete-page:${TEAM}`);
  await app.waitUntil(() => f.calls(`/${TEAM}/undelete`, "POST").length === 1);
  const members = f.f.state.spaces.get("acme")!.members;
  members.delete("alice");
  await refreshMembership(app, f, 2200);
  await app.waitFor(`id:undelete-page:${TEAM}`, { state: "gone" });
  await app.clock!.fastForward(6200);
  expect(app.enabled(`id:undelete-page:${PERSONAL}`)).toBe(true);
  members.add("alice");
  await refreshMembership(app, f);
  expect(app.shows(`id:undelete-page:${TEAM}`)).toBe(false);
  expect(app.shows("id:page-view")).toBe(false);
});

test("Recently deleted shows the empty state when only other scopes have rows", async () => {
  const f = pagesFixture();
  f.entries.set(PERSONAL, { ...f.entries.get(PERSONAL)!, deleted_at: stamp });
  const app = await start(f);
  await app.press("id:space:acme");
  await app.press("id:space-pages:acme");
  await app.press("id:pages-deleted");
  await app.waitFor("text:No recently deleted pages.");
  expect(app.shows(`id:undelete-page:${PERSONAL}`)).toBe(false);
});

test("Schedule consent sends the exact displayed version and revision, never retries a stale click", async () => {
  const f = pagesFixture();
  const app = await start(f);
  await jump(app);
  await app.press("id:page-action:run");
  await app.waitFor("id:page-action-confirm");
  expect(text(app)).toContain("Send the exact release summary");
  expect(text(app)).toContain("Release reminder");
  expect(text(app)).toContain("Acme › Renamed room");
  expect(f.state.started).toBe(0);
  await app.press("id:page-action-cancel");
  await app.waitFor("id:page-action-confirm", { state: "gone" });
  await app.press("id:page-action:run");
  await app.waitFor("id:page-action-confirm");
  f.state.revision = 4;
  await app.press("id:page-action-run");
  await app.waitFor("text:/changed.*no action ran/");
  expect(f.state.started).toBe(0);
  expect(f.calls(`/${TEAM}/actions`, "POST").map(r => r.body)).toEqual([
    { version: 1, block_key: "run", schedule_revision: 3 },
    { version: 1, block_key: "run", schedule_revision: 3 },
    { version: 1, block_key: "run", schedule_revision: 3, confirm: true },
  ]);
  f.state.revision = 5;
  f.state.liveRoom = "Updated destination";
  await app.press("id:page-action:run");
  await app.waitFor("id:page-action-confirm");
  expect(text(app)).toContain("Acme › Updated destination");
  await app.press("id:page-action-run");
  await app.waitFor("text:Scheduled task started.");
  expect(f.state.started).toBe(1);
  expect(f.calls(`/${TEAM}/actions`, "POST").slice(-2).map(r => r.body)).toEqual([
    { version: 1, block_key: "run", schedule_revision: 4 },
    { version: 1, block_key: "run", schedule_revision: 5, confirm: true },
  ]);
});

test("Personal page action stays on the page; a missing room link never falls back", async () => {
  const f = pagesFixture();
  const app = await start(f);
  await jump(app, PERSONAL);
  await app.press("id:page-action:ask");
  await app.waitFor("id:page-action-confirm");
  expect(text(app)).toContain("answer stays on this page");
  await app.press("id:page-action-run");
  await app.waitFor("text:Private page-only answer");
  expect(app.shows("id:page-view")).toBe(true);
  expect(app.shows("id:chat-view")).toBe(false);
  await app.press("id:page-action:room");
  await app.waitFor("text:That room is no longer available.");
  expect(app.shows("id:page-view")).toBe(true);
  expect(app.shows("id:space-view")).toBe(false);
  expect(f.f.posts()).toHaveLength(0);
});

test("A page republished while its confirmation is open does not run the superseded button", async () => {
  const f = pagesFixture();
  const app = await start(f);
  await jump(app);
  await app.press("id:page-action:ask");
  await app.waitFor("id:page-action-confirm");
  expect(text(app)).toContain("Renamed agent (@renamed_agent) in Acme › Renamed room");
  const previous = f.entries.get(TEAM)!;
  f.entries.set(TEAM, { ...previous, head_version: 2, version: { ...previous.version, version: 2 } });
  await app.press("id:page-action-run");
  await app.waitFor("text:/changed.*no action ran/");
  expect(f.calls(`/${TEAM}/actions`, "POST").map(r => r.body)).toEqual([
    { version: 1, block_key: "ask" }, { version: 1, block_key: "ask", confirm: true },
  ]);
  expect(f.state.started).toBe(0);
  expect(text(app)).toContain("Version 2");
});

test("A linked Space page loses access after a verified membership change; Back opens All pages", async () => {
  const f = pagesFixture();
  f.state.linkedPage = TEAM;
  const app = await start(f);
  await jump(app, PERSONAL);
  await app.press("id:page-action:other");
  await app.waitFor("text:Team dashboard");
  await app.waitFor("id:page-action:ask");
  await app.waitUntil(() => f.calls(`/${TEAM}/data`, "POST").length > 0);
  f.f.state.spaces.get("acme")!.members.delete("alice");
  await refreshMembership(app, f);
  await app.waitFor("text:This Space is no longer available.");
  expect(app.shows("id:page-renderer")).toBe(false);
  await app.press("id:page-back");
  await app.waitFor("id:pages-home");
  await app.waitFor(`id:open-page:${PERSONAL}`);
  expect(app.shows(`id:open-page:${TEAM}`)).toBe(false);
});

for (const clear of [true, false]) {
  test(`Delayed model validation never submits an edit after ${clear ? "clearing" : "changing"} its block`, async () => {
    const f = pagesFixture();
    const conn = f.f.addConnection("alice", "openai", "Fixture connection", "gpt-fixture-1");
    const app = await start(f);
    await jump(app);
    await app.press("id:page-select-block");
    await app.press("id:page-edit-block:stat");
    await app.type("id:page-prompt", "Revise the selected block");
    await app.press("id:composer-model");
    await app.press("id:page-model-options");
    await app.press("text:gpt-fixture-1");
    const validations = () => f.log.filter(r => r.path === "/v1/models/available").length;
    const before = validations();
    f.state.modelDelay = 5000;
    await app.press("id:page-generate");
    await app.waitUntil(() => validations() > before);
    await app.press(clear ? "text:Edit whole page" : "id:page-edit-block:intro");
    await app.clock!.fastForward(5100);
    await app.waitFor("text:/The edit target changed/");
    expect(f.calls("/drafts", "POST")).toHaveLength(0);
    if (clear) await app.press("id:page-edit-block:intro");
    f.state.modelDelay = 0;
    await app.press("id:page-generate");
    await app.waitFor("id:page-draft-view");
    expect(f.calls("/drafts", "POST").map(r => r.body)).toEqual([{ page_id: TEAM, block_key: "intro",
      prompt: "Revise the selected block", provider_connection_id: conn.id, model_id: "gpt-fixture-1" }]);
  });
}

for (const loss of ["edit", "read", "membership"] as const) {
  test(`Apply denied: ${loss === "edit" ? "retain preview, diff and Discard for a still-member editor" : `hide draft after ${loss} loss`}`, async () => {
    const f = pagesFixture();
    f.f.state.spaces.get("acme")!.members.add("bob");
    f.entries.set(TEAM, { ...f.entries.get(TEAM)!, members_edit: true });
    const app = await start(f, "bob");
    await jump(app);
    await app.press("id:page-edit");
    await app.type("id:page-prompt", "Update the overview");
    await app.press("id:page-generate");
    await app.waitFor("id:page-renderer");
    await app.waitFor("text:Proposed dashboard");
    await app.press("id:page-draft-tab:changes");
    await app.waitFor("id:page-source-diff");
    f.entries.set(TEAM, { ...f.entries.get(TEAM)!, members_edit: false });
    f.state.denyDraftRead = loss === "read";
    if (loss === "membership") await app.clock!.fastForward(9_200);
    f.state.draftDelay = 5000;
    const before = f.calls("/drafts/pgd_1").length;
    await app.press("id:page-apply");
    await app.waitUntil(() => f.calls("/drafts/pgd_1").length > before);
    await app.waitFor("text:/Checking whether you can still review/");
    expect(app.shows("id:page-source-diff")).toBe(false);
    if (loss === "membership") {
      f.f.state.spaces.get("acme")!.members.delete("bob");
      await refreshMembership(app, f, 2200);
      await app.waitFor("id:page-draft-view", { state: "gone" });
    }
    await app.clock!.fastForward(5100);
    if (loss !== "edit") {
      if (loss === "read") await app.waitFor("text:/This draft is no longer available/");
      expect(app.shows("text:Proposed dashboard")).toBe(false);
      expect(app.shows("id:page-source-diff")).toBe(false);
      expect(app.shows("id:page-apply")).toBe(false);
    } else {
      await app.waitFor("text:/no longer have permission to apply/");
      await app.waitFor("id:page-source-diff");
      await expect(app.press("id:page-apply")).rejects.toMatchObject({ code: "ERR_NSH_TEST_DISABLED" });
      await app.press("id:page-draft-tab:preview");
      await app.waitFor("id:page-renderer");
      await app.press("id:page-discard");
      await app.press("id:page-discard-now");
      await app.waitFor("id:page-view");
      expect(f.calls("/drafts/pgd_1", "DELETE")).toHaveLength(1);
    }
    expect(f.calls("/drafts/pgd_1/apply", "POST")).toHaveLength(1);
    expect(f.state.applied).toBe(0);
  });
}

test("Scheduled refresh is offered only for a room-and-agent Space brief, not a block-linked answer", async () => {
  const f = pagesFixture();
  f.replaceTeam(briefSpec(false));
  f.entries.set(PERSONAL, { ...f.entries.get(PERSONAL)!, spec: briefSpec(true) });
  const app = await start(f);
  await jump(app, PERSONAL);
  expect(app.shows("id:page-schedule:answer")).toBe(false);
  await jump(app);
  await app.waitFor("text:Everything looks good.");
  await app.press("id:page-schedule:answer");
  await app.waitFor("id:page-open-schedule-form");
  expect(text(app)).toContain("including scheduled answers");
  await app.press("id:page-open-schedule-form");
  await app.waitFor("id:schedule-prompt");
  expect(app.value("id:schedule-prompt")).toBe("Summarize today");
  expect(text(app)).toContain("To: the agent in Acme · #lobby");
  expect(f.f.posts().filter(r => r.path === "/v1/schedules")).toHaveLength(0);
  f.replaceTeam(briefSpec(false, true));
  await jump(app);
  expect(app.shows("id:page-schedule:answer")).toBe(false);
});

test("Prompt creation polls a private draft, previews disabled actions and applies once", async () => {
  const f = pagesFixture();
  f.state.generating = 2;
  const app = await start(f);
  await app.press("id:nav-pages");
  await app.press("id:new-page");
  await app.type("id:page-prompt", "Build a release dashboard");
  await app.press("id:page-generate");
  await app.waitFor("id:page-draft-generating");
  await app.waitFor("text:Proposed dashboard");
  await app.waitFor("id:page-action:ask");
  await expect(app.press("id:page-action:ask")).rejects.toMatchObject({ code: "ERR_NSH_TEST_DISABLED" });
  expect(f.calls("/drafts", "POST")[0]?.body).toEqual({ scope: "personal", prompt: "Build a release dashboard" });
  expect(f.state.started).toBe(0);
  await app.press("id:page-draft-tab:changes");
  await app.waitFor("id:page-source-diff");
  await app.press("id:page-draft-tab:source");
  await app.waitFor("id:page-code");
  await app.press("id:page-apply");
  await app.waitFor("id:page-view");
  await app.waitFor("text:Proposed dashboard");
  expect(f.state.applied).toBe(1);
});

test("Selected-block edit requires review of outside changes; stale conflict re-diffs and never applies", async () => {
  const f = pagesFixture();
  f.state.outside = true;
  f.state.stale = true;
  const app = await start(f);
  await jump(app);
  await app.press("id:page-select-block");
  await app.press("id:page-edit-block:stat");
  await app.waitFor("text:Editing block: stat");
  await app.type("id:page-prompt", "Retitle the metric");
  await app.press("id:page-generate");
  await app.waitFor("text:Changes outside the selected block");
  expect(f.calls("/drafts", "POST")[0]?.body).toEqual({ page_id: TEAM, block_key: "stat", prompt: "Retitle the metric" });
  await app.press("id:page-apply");
  expect(f.calls("/drafts/pgd_1/apply", "POST")).toHaveLength(0);
  await app.press("id:page-apply-outside");
  await app.waitFor("text:Comparing draft with current version 2");
  await app.waitFor("id:page-source-diff");
  expect(f.calls("/drafts/pgd_1/apply", "POST")[0]?.body).toEqual({ confirm_outside: true });
  await expect(app.press("id:page-apply")).rejects.toMatchObject({ code: "ERR_NSH_TEST_DISABLED" });
  expect(f.state.applied).toBe(0);
  await app.press("id:page-discard");
  await app.press("id:page-discard-now");
  await app.waitFor("id:page-view");
  expect(f.calls("/drafts/pgd_1", "DELETE")).toHaveLength(1);
});

test("Only the proposal author can open a thread’s review; a missing draft has no Apply action", async () => {
  const f = pagesFixture();
  const event = f.propose("alice");
  const gone = f.propose("alice", true);
  async function thread(app: App) {
    await app.press("id:space:acme");
    await app.press("id:room:thread-a");
    await app.waitFor(`id:page-draft-card:${event.id}`);
  }
  const app = await start(f, "bob");
  await thread(app);
  expect(app.shows(`id:review-page-draft:${event.id}`)).toBe(false);
  expect(f.calls(`/drafts/${DRAFT}`)).toHaveLength(0);
  await app.press("id:account-menu");
  await app.press("id:disconnect");
  await signIn(app, "alice");
  await thread(app);
  await app.press(`id:review-page-draft:${event.id}`);
  await app.waitFor("id:page-draft-view");
  await app.waitFor("id:page-apply");
  expect([...new Set(f.calls(`/drafts/${DRAFT}`).map(r => r.user))]).toEqual(["alice"]);
  await app.press("id:page-draft-back");
  await app.waitFor(`id:page-draft-card:${event.id}`);
  await app.press(`id:review-page-draft:${gone.id}`);
  await app.waitFor("text:/This draft is no longer available/");
  expect(app.shows("id:page-apply")).toBe(false);
});

test("Live data stops polling in the background, under Settings, and on another route", async () => {
  const f = pagesFixture();
  const app = await start(f);
  await jump(app);
  await app.waitFor("text:Everything looks good.");
  const reads = () => f.calls(`/${TEAM}/data`, "POST").length;
  let before = reads();
  await app.setLifecycle("background");
  await app.clock!.fastForward(35_000);
  expect(reads()).toBe(before);
  await app.setLifecycle("active");
  await app.waitUntil(() => reads() > before);
  before = reads();
  await app.press("id:nav-settings");
  await app.waitFor("id:settings-dialog");
  await app.clock!.fastForward(35_000);
  expect(reads()).toBe(before);
  await app.press("id:settings-close");
  await app.waitUntil(() => reads() > before);
  before = reads();
  await app.clock!.fastForward(31_000);
  await app.waitUntil(() => reads() > before);
  await app.press("id:nav-chat");
  before = reads();
  await app.clock!.fastForward(35_000);
  expect(reads()).toBe(before);
});

const readAbort = (app: App, method: string, path: string) => Number(app.storage()[`pages-abort:${method}:/v1/pages${path}`] ?? 0);
const readStart = (app: App, method: string, path: string) => Number(app.storage()[`pages-read:${method}:/v1/pages${path}`] ?? 0);
async function waitCount(app: App, count: () => number, expected: number, label: string) {
  try { await app.waitUntil(() => count() >= expected); }
  catch { throw new Error(`Expected ${expected} ${label}, saw ${count()}`); }
  if (count() !== expected) throw new Error(`Expected exactly ${expected} ${label}, saw ${count()}`);
}

test("Page reads abort on hide and route exit; resuming supersedes the delayed result", async () => {
  const f = pagesFixture();
  f.state.delayPage = TEAM;
  f.state.delay = 8000;
  const app = await start(f, "alice", true);
  await app.press("id:nav-pages");
  await app.press(`id:open-page:${TEAM}`);
  await waitCount(app, () => readStart(app, "GET", `/${TEAM}`), 1, "page head reads");
  await app.setLifecycle("background");
  await waitCount(app, () => readAbort(app, "GET", `/${TEAM}`), 1, "page head aborts");
  expect(f.calls(`/${TEAM}/data`, "POST")).toHaveLength(0);
  f.state.delay = 0;
  f.state.dataDelay = 8000;
  await app.setLifecycle("active");
  await waitCount(app, () => readStart(app, "POST", `/${TEAM}/data`), 1, "page data reads");
  await app.press("id:nav-chat");
  await waitCount(app, () => readAbort(app, "POST", `/${TEAM}/data`), 1, "page data aborts");
  await app.clock!.fastForward(8100);
  expect(app.shows("id:page-view")).toBe(false);
  expect(readStart(app, "POST", `/${TEAM}/data`)).toBe(1);
});

test("A manual full refresh supersedes data-only polling and ignores its older answer", async () => {
  const f = pagesFixture();
  const app = await start(f, "alice", true);
  await jump(app);
  await app.waitFor("text:Everything looks good.");
  const reads = () => readStart(app, "POST", `/${TEAM}/data`);
  const first = reads();
  f.state.dataDelay = 8000;
  f.state.answer = "Obsolete delayed value";
  await app.clock!.fastForward(31_000);
  await waitCount(app, reads, first + 1, "live data reads");
  f.state.dataDelay = 0;
  f.state.answer = "Manually refreshed value";
  await app.press("id:page-action:refresh");
  await waitCount(app, () => readAbort(app, "POST", `/${TEAM}/data`), 1, "superseded data aborts");
  await app.waitFor("text:Manually refreshed value");
  await app.clock!.fastForward(8100);
  expect(reads()).toBe(first + 2);
  expect(text(app)).not.toContain("Obsolete delayed value");
});

test("Draft polling coalesces delayed reads and aborts on hide; changing tabs cancels its preview", async () => {
  const f = pagesFixture();
  const event = f.propose("alice");
  f.state.draftDelay = 8000;
  const app = await start(f, "alice", true);
  await app.press("id:space:acme");
  await app.press("id:room:thread-a");
  await app.press(`id:review-page-draft:${event.id}`);
  const reads = () => readStart(app, "GET", `/drafts/${DRAFT}`);
  await waitCount(app, reads, 1, "draft reads");
  await app.clock!.runFor(4500);
  expect(reads()).toBe(1);
  await app.setLifecycle("background");
  await waitCount(app, () => readAbort(app, "GET", `/drafts/${DRAFT}`), 1, "draft aborts");
  f.state.draftDelay = 0;
  f.state.previewDelay = 8000;
  f.state.answer = "Obsolete draft data";
  await app.setLifecycle("active");
  const previews = () => readStart(app, "POST", `/drafts/${DRAFT}/data`);
  await waitCount(app, previews, 1, "preview reads");
  expect(reads()).toBe(2);
  await refreshMembership(app, f, 5400);
  expect(previews()).toBe(1); // routine Space sync is not supersession
  await app.press("id:page-draft-tab:source");
  await waitCount(app, () => readAbort(app, "POST", `/drafts/${DRAFT}/data`), 1, "preview aborts");
  f.state.previewDelay = 0;
  f.state.answer = "Fresh draft preview";
  await app.press("id:page-draft-tab:preview");
  await app.waitFor("text:Fresh draft preview");
  await app.clock!.fastForward(8100);
  expect(previews()).toBe(2);
  expect(text(app)).not.toContain("Obsolete draft data");
});

test("A completed action does not start a data refresh while the window is hidden", async () => {
  const f = pagesFixture();
  f.state.actionDelay = 3000;
  const app = await start(f);
  await jump(app, PERSONAL);
  await app.press("id:page-action:ask");
  await app.waitFor("id:page-action-confirm");
  await app.press("id:page-action-run");
  await app.waitUntil(() => f.state.started === 1);
  await app.setLifecycle("background");
  const reads = () => f.calls(`/${PERSONAL}/data`, "POST").length;
  const before = reads();
  await app.clock!.fastForward(4000);
  expect(reads()).toBe(before);
  await app.setLifecycle("active");
  await app.waitFor("text:Private page-only answer");
  expect(reads()).toBeGreaterThan(before);
});

test("A late Page definition from an old session never becomes visible to the next account", async () => {
  const f = pagesFixture();
  f.state.delayPage = PERSONAL;
  f.state.delay = 3000;
  const app = await start(f);
  await app.press("id:nav-pages");
  await app.press(`id:open-page:${PERSONAL}`);
  await app.waitUntil(() => f.calls(`/${PERSONAL}`).length > 0);
  await app.press("id:account-menu");
  await app.press("id:disconnect");
  await signIn(app, "bob");
  await app.press("id:nav-pages");
  await app.waitFor("id:open-page:pg_bob");
  await app.clock!.fastForward(4000);
  expect(text(app)).not.toContain("Alice’s dashboard");
  expect(app.shows(`id:pinned-page:${PERSONAL}`)).toBe(false);
  expect(f.calls(`/${PERSONAL}/data`, "POST")).toHaveLength(0);
});

test("A late Space page cannot open after switching to another Space", async () => {
  const f = pagesFixture();
  f.state.delayPage = TEAM;
  f.state.delay = 3000;
  const app = await start(f);
  await app.press("id:nav-pages");
  await app.press(`id:open-page:${TEAM}`);
  await app.waitUntil(() => f.calls(`/${TEAM}`).length > 0);
  await app.press("id:space:side");
  await app.waitFor("text:side lobby only");
  await app.clock!.fastForward(4000);
  expect(app.shows("id:page-view")).toBe(false);
  expect(f.calls(`/${TEAM}/data`, "POST")).toHaveLength(0);
});

test("Page TSX source editor checks diagnostics before saving a version", async () => {
  const f = pagesFixture();
  const app = await start(f);
  await jump(app);
  await menu(app, "page-source");
  await app.waitFor("id:page-code");
  await app.keyboard.press("mod+a");
  await app.type("id:page-code", "INVALID");
  await app.waitFor("text:/Unsupported fixture source/");
  await expect(app.press("id:page-save-source")).rejects.toMatchObject({ code: "ERR_NSH_TEST_DISABLED" });
  await app.keyboard.press("mod+a");
  await app.type("id:page-code", "export default (<Page title=\"Updated\" />);");
  await app.waitUntil(() => f.calls("/validate", "POST").some(r => !r.body.source.includes("INVALID")));
  await app.waitFor("id:page-diagnostics", { state: "gone" });
  await app.press("id:page-save-source");
  await app.waitFor("text:/Version 2/");
  expect(f.calls(`/${TEAM}/versions`, "POST").map(r => r.body.base_version)).toEqual([1]);
});

test("Source conflict preserves my changes, diffs the new head, and requires an explicit retry", async () => {
  const f = pagesFixture();
  f.state.staleSource = true;
  const app = await start(f);
  await jump(app);
  await menu(app, "page-source");
  await app.waitFor("id:page-code");
  await app.type("id:page-code", "// Kept my unsaved edit\n");
  const saves = () => f.calls(`/${TEAM}/versions`, "POST");
  await app.waitUntil(() => app.enabled("id:page-save-source"));
  const edited = f.calls("/validate", "POST").at(-1)?.body.source;
  expect(edited).toContain("// Kept my unsaved edit");
  await app.press("id:page-save-source");
  await app.waitFor("text:Published version 2 → your unsaved source");
  await app.waitFor("id:page-source-diff");
  expect(text(app)).toContain("Another editor published this");
  expect(text(app)).toContain("Kept my unsaved edit");
  expect(saves().map(r => r.body)).toEqual([{ base_version: 1, source: edited }]);
  expect(app.enabled("id:page-save-source")).toBe(false);
  await app.press("id:page-use-current-base");
  await app.waitFor("id:page-code");
  expect(saves()).toHaveLength(1);
  expect(text(app)).toContain("Kept my unsaved edit");
  await app.press("id:page-save-source");
  await app.waitFor("text:/^Version 3/");
  expect(saves().map(r => r.body)).toEqual([{ base_version: 1, source: edited }, { base_version: 2, source: edited }]);
});

test("Settings rename, history restore, delete and undelete preserve version checks", async () => {
  const f = pagesFixture();
  const app = await start(f);
  await jump(app);
  await menu(app, "page-settings");
  await app.clear("id:page-setting-title");
  await app.type("id:page-setting-title", "Renamed dashboard");
  await app.press("id:page-members-edit");
  await app.press("id:page-save-settings");
  await app.waitFor("text:Renamed dashboard");
  expect(f.calls(`/${TEAM}`, "PATCH")[0]?.body).toEqual({ title: "Renamed dashboard", base_version: 1, members_edit: true });
  await menu(app, "page-history");
  await app.press("id:page-version:1");
  await app.press("id:page-history-tab:changes");
  await app.waitFor("id:page-source-diff");
  await app.press("id:page-restore-version");
  await app.press("id:page-restore-now");
  await app.waitFor("text:/Version 3/");
  expect(f.calls(`/${TEAM}/versions/1/restore`, "POST")[0]?.body).toEqual({ base_version: 2 });
  await menu(app, "page-delete");
  await app.press("id:page-delete-now");
  await app.waitFor("id:pages-home");
  await app.press("id:pages-deleted");
  await app.press(`id:undelete-page:${TEAM}`);
  await app.waitFor("id:page-view");
  expect(f.calls(`/${TEAM}/undelete`, "POST")).toHaveLength(1);
});
