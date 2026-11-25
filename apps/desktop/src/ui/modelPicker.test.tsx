import { expect, open, test } from "nsh/test";
import type { App } from "nsh/test";
import { createPickerMemory, pickerScopeKey } from "./pickerMemory";

const FIXTURE = "./modelPicker.fixture.tsx";
const SEARCH = "id:picker-search";
const OPENAI = "id:picker-group:OpenAI";
const TOGGLE = "id:picker-group-toggle:OpenAI";
const ALPHA = "id:picker:alpha";
const DEFAULT = "id:picker:";
const account = pickerScopeKey("https://picker.example", "person:1", "composer")!;
const COLLAPSED = `${account}:"openai"`;

const launch = async (storage?: Record<string, string>) => {
  const app = await open(FIXTURE, { platform: "linux", width: 1000, height: 740, ...(storage && { storage }) });
  await app.press("id:open");
  return app;
};
async function show(app: App) {
  await app.press("id:picker");
  await app.waitFor(SEARCH);
}

test("header toggles only disclosure; search shows matching models without changing what is remembered", async () => {
  const app = await launch();
  await show(app);
  await app.waitFor(`${OPENAI} >> text:OpenAI · 2`);
  await app.waitFor("label:OpenAI, 2 models, expanded");
  expect(app.enabled("id:picker:beta")).toBe(false);
  await app.press(OPENAI); // the legacy group ID is still a press target
  await app.waitFor("label:OpenAI, 2 models, collapsed");
  expect(app.shows(ALPHA)).toBe(false);
  expect(app.shows(DEFAULT)).toBe(true);
  expect(app.shows("text:Choices: 0")).toBe(true);
  await app.waitUntil(() => app.storage()[COLLAPSED] === "collapsed");

  await app.type(SEARCH, "alpha-1");
  await app.waitFor(`${OPENAI} >> text:OpenAI · 1`);
  expect(app.shows(TOGGLE)).toBe(false);
  expect(app.shows(ALPHA)).toBe(true);
  expect(app.shows("id:picker:beta")).toBe(false);
  expect(app.shows("id:picker:claude")).toBe(false);
  expect(app.shows(DEFAULT)).toBe(true);
  await app.clear(SEARCH);
  await app.waitFor("label:OpenAI, 2 models, collapsed");
  expect(app.shows(ALPHA)).toBe(false);

  await app.type(SEARCH, "does not exist");
  await app.waitFor("text:No model matches.");
  expect(app.shows(DEFAULT)).toBe(true);
  await app.keyboard.press("return");
  expect(app.shows(SEARCH)).toBe(true);
  expect(app.shows("text:Choices: 0")).toBe(true);
  await app.press("id:outside");
  await app.waitFor(SEARCH, { state: "gone" });
  await show(app);
  expect(app.value(SEARCH)).toBe("");
  await app.waitFor("label:OpenAI, 2 models, collapsed");
});

test("an explicit pick keeps its provider open without overwriting the collapse preference", async () => {
  const app = await launch({ [COLLAPSED]: "collapsed" });
  await show(app);
  await app.waitFor("label:OpenAI, 2 models, collapsed");
  await app.type(SEARCH, "Alpha");
  await app.press(ALPHA);
  await app.waitFor("id:picker >> text:Alpha");
  expect(app.shows("id:picker-value")).toBe(true);
  await show(app);
  expect(app.shows(TOGGLE)).toBe(false);
  expect(app.shows(ALPHA)).toBe(true);
  await app.press(ALPHA); // reselecting does not reset thinking or write a setting
  expect(app.shows("text:Choices: 1")).toBe(true);
  await show(app);
  await app.press(DEFAULT);
  await app.waitFor("text:Choices: 2");
  await show(app);
  await app.waitFor("label:OpenAI, 2 models, collapsed");
  expect(app.shows(ALPHA)).toBe(false);
  await app.keyboard.press("mod+7"); // when it becomes the only provider it cannot collapse
  await app.waitFor(ALPHA);
  expect(app.shows(TOGGLE)).toBe(false);
  expect(app.storage()[COLLAPSED]).toBe("collapsed");
});

test("picker closes on stale scope and restores only the same account, server and picker kind", async () => {
  const app = await launch({ [COLLAPSED]: "collapsed" });
  await show(app);
  await app.waitFor("label:OpenAI, 2 models, collapsed");
  await app.keyboard.press("mod+6"); // asynchronous display names may update in place
  await app.waitFor("label:OpenAI renamed, 2 models, collapsed");
  await app.type(SEARCH, "alpha");
  await app.keyboard.press("down"); // Default
  await app.keyboard.press("down"); // Alpha
  await app.keyboard.press("mod+5"); // changed Space/catalog context, same preference partition
  await app.waitFor(SEARCH, { state: "gone" });
  await app.keyboard.press("return"); // focus goes back to the trigger, never to the stale row
  await app.waitFor(SEARCH);
  expect(app.shows("text:Choices: 0")).toBe(true);
  await app.waitFor("label:OpenAI renamed, 2 models, collapsed");
  for (const key of ["mod+1", "mod+2", "mod+3"]) {
    await app.keyboard.press(key);
    await app.waitFor(SEARCH, { state: "gone" });
    await app.keyboard.press("return"); // must not pick a stale row
    if (!app.shows(SEARCH)) await show(app);
    await app.waitFor("label:OpenAI renamed, 2 models, expanded");
    expect(app.shows(ALPHA)).toBe(true);
  }
  await app.keyboard.press("mod+4");
  await app.waitFor(SEARCH, { state: "gone" });
  await show(app);
  await app.waitFor("label:OpenAI renamed, 2 models, collapsed");
  await app.keyboard.press("mod+8");
  await app.waitFor(SEARCH, { state: "gone" });
  expect(app.enabled("id:picker")).toBe(false);
  expect(app.shows("text:Choices: 0")).toBe(true);
});

// This scenario also runs against Linux GTK in Xvfb, without a server.
test("keyboard navigates disclosure and choice in a modal and restores trigger focus on close", async () => {
  const app = await launch();
  await show(app);
  await app.keyboard.press("down"); // Default
  await app.keyboard.press("down"); // OpenAI header
  await app.keyboard.press("left");
  await app.waitFor("label:OpenAI, 2 models, collapsed");
  await app.keyboard.press("space");
  await app.waitFor("label:OpenAI, 2 models, expanded");
  await app.keyboard.press("return");
  await app.waitFor("label:OpenAI, 2 models, collapsed");
  await app.keyboard.press("right");
  await app.waitFor("label:OpenAI, 2 models, expanded");
  expect(app.shows("text:Choices: 0")).toBe(true);
  await app.keyboard.press("down");
  await app.keyboard.press("return");
  await app.waitFor("id:picker >> text:Alpha");
  await app.waitFor(SEARCH, { state: "gone" });
  await app.keyboard.press("return"); // focus returned to the trigger
  await app.waitFor(SEARCH);
  await app.keyboard.press("escape"); // closes only the inner popup
  await app.waitFor(SEARCH, { state: "gone" });
  expect(app.shows("id:picker")).toBe(true);
  await app.keyboard.press("tab");
  await app.keyboard.press("return");
  await app.waitFor("text:Next control: 1");
  await app.keyboard.press("shift+tab");
  await app.keyboard.press("return");
  await app.waitFor(SEARCH);
  // Native Tab can reach the real buttons as well as arrows. After Default
  // and Alpha, it reaches the other group's header. Collapsing it puts that
  // header last, so the next Tab leaves the popup and dismisses it.
  for (let i = 0; i < 3; i++) await app.keyboard.press("tab");
  await app.keyboard.press("space");
  await app.waitFor("label:Anthropic, 1 model, collapsed");
  await app.keyboard.press("tab");
  await app.waitFor(SEARCH, { state: "gone" });
  expect(app.shows("text:Choices: 1")).toBe(true);
});

test("picker fits and scrolls a long catalog inside a narrow modal", async () => {
  const app = await launch();
  await app.setViewport(720, 480);
  await app.keyboard.press("mod+0");
  await show(app);
  await app.waitFor("label:OpenAI, 42 models, expanded");
  await app.keyboard.press("down");
  await app.keyboard.press("end"); // reveal a row outside the initial scroll viewport
  await app.press("id:picker:claude");
  await app.waitFor("id:picker >> text:Claude");
  await app.waitFor("text:Choices: 1");
});

test("a late preference read cannot undo an edit or affect another provider or scope", async () => {
  const pending = new Map<string, (value: string | null) => void>();
  const writes: [string, string | null][] = [];
  const memory = createPickerMemory({
    getItem: (key) => new Promise((resolve) => pending.set(key, resolve)),
    setItem: async (key, value) => { writes.push([key, value]); },
    removeItem: async (key) => { writes.push([key, null]); },
  });
  const other = pickerScopeKey("https://picker.example", "person:2", "composer")!;
  const original = memory(account, "openai");
  const peer = memory(account, "anthropic");
  const switched = memory(other, "openai");
  const anonymous = memory(null, "openai");
  original.setCollapsed(true);
  original.setCollapsed(false);
  pending.get(COLLAPSED)!("collapsed"); // old state arrives after both explicit edits
  pending.get(`${account}:"anthropic"`)!("collapsed");
  pending.get(`${other}:"openai"`)!(null);
  anonymous.setCollapsed(true);
  await Promise.resolve();
  expect(original.collapsed()).toBe(false);
  expect(peer.collapsed()).toBe(true);
  expect(switched.collapsed()).toBe(false);
  expect(anonymous.collapsed()).toBe(true);
  expect(writes).toEqual([[COLLAPSED, "collapsed"], [COLLAPSED, null]]);
  expect(pending.size).toBe(3);
  expect(memory(account, "openai")).toBe(original); // same scope, no duplicate read
  expect(pickerScopeKey("a:b", "c", "default")).not.toBe(pickerScopeKey("a", "b:c", "default"));
  expect(pickerScopeKey("https://picker.example", null, "default")).toBe(null);
});

test("unavailable storage still lets the picker collapse and expand in this session", async () => {
  const unavailable = async () => { throw new Error("Unwritable"); };
  const memory = createPickerMemory({ getItem: unavailable, setItem: unavailable, removeItem: unavailable });
  const group = memory(account, "openai");
  await Promise.resolve();
  group.setCollapsed(true);
  expect(group.collapsed()).toBe(true);
  group.setCollapsed(false);
  expect(group.collapsed()).toBe(false);
});
