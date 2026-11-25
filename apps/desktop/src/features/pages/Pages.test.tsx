import { expect, open, test } from "nsh/test";
import type { App } from "nsh/test";
import { BASE, TOKENS } from "../../../test/fakeServer";
import { launched } from "../../../test/launched";
import { DRAFT, pagesFixture, PERSONAL, TEAM } from "./test/fixtureServer";

type Fixture = ReturnType<typeof pagesFixture>;
async function signIn(app: App, user: string) {
  await app.clear("id:server-url");
  await app.type("id:server-url", BASE);
  await app.type("id:access-token", TOKENS[user]!);
  await app.press("id:connect");
  await app.waitFor("id:nav-pages");
}
async function start(f: Fixture, user = "alice") {
  const app = await open("../../../App.tsx", { modules: launched(), network: f.network as any, platform: "linux", width: 1280, height: 900, clock: {} });
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
  await app.press("id:new-page");
  await app.press("id:new-page-templates");
  await app.press("id:page-template");
  await app.press("id:page-template:files");
  await app.press("id:create-page-template");
  await app.waitFor("text:Recent files");
  expect(f.calls("", "POST").map(r => r.body)).toEqual([{ scope: "space", space_id: "acme", template: "files" }]);
});

test("Schedule consent sends the exact displayed version and revision, never retries a stale click", async () => {
  const f = pagesFixture();
  const app = await start(f);
  await jump(app);
  await app.press("id:page-action:run");
  await app.waitFor("id:page-action-confirm");
  expect(text(app)).toContain("Send the exact release summary");
  expect(text(app)).toContain("Release reminder");
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
  await app.press("id:page-action:run");
  await app.waitFor("id:page-action-confirm");
  await app.press("id:page-action-run");
  await app.waitFor("text:Scheduled task started.");
  expect(f.state.started).toBe(1);
  expect(f.calls(`/${TEAM}/actions`, "POST").slice(-1)[0]?.body).toEqual({ version: 1, block_key: "run", schedule_revision: 4, confirm: true });
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
