// Attaching files to messages, headless against the fake server and its
// Library: picking only stages, Send uploads to exactly the destination's
// library and posts `asset_ids`, files show in the transcript and open in
// the Library's viewer, and failures never upload twice, lose the draft or
// land in another place.

import { expect, open, test } from "nsh/test";
import type { App } from "nsh/test";
import { BASE, TOKENS } from "../../../test/fakeServer";
import { attachmentWorld } from "./test/fixtures";
import { checkPicked, pickAttachments, picking, pickNote } from "./attachmentSend";
import { disconnect, openChat, openPlace } from "../../store";
import { toAttachments } from "../../api/client";
import { clearAllStaged, dropSent, hasPendingAttachments, markUploaded, stage, stagedFor } from "./attachmentDraft";
import { launched } from "../../../test/launched";

type W = ReturnType<typeof attachmentWorld>;
const enc = new TextEncoder();
const NOTE = { name: "notes.txt", type: "text/plain", data: enc.encode("meeting notes") };
const PLAN = { name: "plan.md", type: "text/markdown", data: enc.encode("# Plan") };

async function signIn(w: W, user = "alice") {
  const app = await open("../../../App.tsx", { modules: launched(), network: w.network as any, platform: "linux", width: 1200, height: 800 });
  await app.clear("id:server-url");
  await app.type("id:server-url", BASE);
  await app.type("id:access-token", TOKENS[user]!);
  await app.press("id:connect");
  return app;
}
async function inAcme(w: W) {
  const app = await signIn(w);
  await app.waitFor("id:space:acme");
  await app.press("id:space:acme");
  await app.waitFor("text:hello from bob in acme");
  return app;
}

const uploads = (w: W) => w.l.log.filter((r) => r.method === "POST" && r.path === "/v1/library/assets");
const posts = (w: W) => w.f.posts().filter((p) => p.path.endsWith("/messages"));
const lastFiles = (w: W, key: string) => (w.f.state.events.get(key) ?? []).filter((e: any) => e.attachments).at(-1) as any;

test("picking only stages; Remove and a cancelled pick upload nothing; Send uploads to the Space's Library and posts asset_ids", async () => {
  const w = attachmentWorld();
  const app = await inAcme(w);
  app.onPick(() => null); // cancelled
  await app.press("id:attach");
  expect(app.shows("id:attachments")).toBe(false);

  app.onPick(() => [NOTE, PLAN]);
  await app.press("id:attach");
  await app.waitFor("id:staged:notes.txt");
  expect(app.shows("id:staged:plan.md")).toBe(true);
  expect(app.shows("text:/^Saved to Acme's Library · its members see them · the agent gets text only$/")).toBe(true);
  await app.press("id:staged:plan.md:remove");
  await app.waitFor("id:staged:plan.md", { state: "gone" });
  expect(uploads(w).length).toBe(0); // nothing left the device yet
  expect(app.enabled("id:send")).toBe(true); // a file alone can be sent
  expect(app.enabled("id:ask-agent")).toBe(false); // asking needs a typed question

  await app.type("id:composer", "see attached");
  await app.press("id:send");
  await app.waitFor("id:attachments", { state: "gone" });
  expect(uploads(w).length).toBe(1);
  expect(uploads(w)[0]!.url).toBe(`${BASE}/v1/library/assets?scope=space&space_id=acme&name=notes.txt`);
  const id = w.l.assets[0]!.id;
  expect(posts(w).at(-1)!.body).toEqual({ text: "see attached", invoke_agent: false, asset_ids: [id], infer_text_mentions: false, defer_decisions: true });
  expect(w.f.state.log.some((l) => l.path.includes("/library/index"))).toBe(false); // never indexed
  const ev = lastFiles(w, "acme/lobby");
  await app.waitFor(`id:file:${ev.id}:${id}`);
  expect(app.shows(`id:file:${ev.id}:${id} >> text:notes.txt`)).toBe(true);
  expect(app.value("id:composer")).toBe("");
});

test("Chat: a file-only message goes to my personal Library; the file opens in the Library viewer", async () => {
  const w = attachmentWorld();
  const app = await signIn(w);
  await app.waitFor("id:chat-view");
  app.onPick(() => [NOTE]);
  await app.press("id:attach");
  await app.waitFor("id:staged:notes.txt");
  expect(app.shows("text:/Saved to your Library · only you · the agent gets text only/")).toBe(true);
  await app.press("id:send");
  await app.waitFor("id:attachments", { state: "gone" });
  expect(uploads(w)[0]!.url).toBe(`${BASE}/v1/library/assets?scope=personal&name=notes.txt`);
  const id = w.l.assets[0]!.id;
  expect(posts(w).at(-1)!.body).toEqual({ text: "", invoke_agent: true, asset_ids: [id] });
  const ev = lastFiles(w, "chat:alice");
  await app.waitFor(`id:file:${ev.id}:${id}`);

  await app.press(`id:file:${ev.id}:${id}`);
  await app.waitFor("id:library-viewer");
  await app.waitFor("text:meeting notes"); // read through the authenticated content call
  const read = w.l.log.find((r) => r.path === `/v1/library/assets/${id}/content`)!;
  expect(read.headers["authorization"]).toBe(`Bearer ${TOKENS.alice}`);
  expect(read.url).not.toContain(TOKENS.alice!);
});

test("Chat: Save note with files posts only note intent, even with a literal @agent", async () => {
  const w = attachmentWorld();
  const app = await signIn(w);
  await app.waitFor("id:chat-view");
  app.onPick(() => [NOTE]);
  await app.press("id:attach");
  await app.waitFor("id:staged:notes.txt");
  await app.type("id:composer", "@agent is literal");
  await app.press("id:save-note");
  await app.waitFor("id:attachments", { state: "gone" });
  expect(posts(w).at(-1)!.body).toEqual({
    text: "@agent is literal", save_note: true, asset_ids: [w.l.assets[0]!.id],
  });
  expect(w.f.state.events.get("chat:alice")).toMatchObject([{ kind: "message", audience: "room" }]);
});

for (const noteOnly of [true, false]) {
  const action = noteOnly ? "Save note" : "Send";
  const button = noteOnly ? "id:save-note" : "id:send";
  test(`Chat: a failed upload tells you to retry ${action} and reuses the uploaded file`, async () => {
    const w = attachmentWorld();
    const app = await signIn(w);
    await app.waitFor("id:chat-view");
    app.onPick(() => [NOTE, PLAN]);
    await app.press("id:attach");
    await app.waitFor("id:staged:plan.md");
    await app.type("id:composer", "@agent with two files");
    w.knobs.failUploadOf = PLAN.name;
    const messageAttempts = () => app.requests().filter(r => r.method === "POST" && r.url === `${BASE}/v1/chat/messages`);
    const uploadNames = () => app.requests()
      .filter(r => r.method === "POST" && new URL(r.url).pathname === "/v1/library/assets")
      .map(r => new URL(r.url).searchParams.get("name"));

    await app.press(button);
    await app.waitFor(`id:action-error >> text:/${action} again to retry/`);
    if (noteOnly) expect(app.shows("id:action-error >> text:/Send again|Return/")).toBe(false);
    expect(messageAttempts()).toHaveLength(0);
    expect(uploadNames()).toEqual([NOTE.name, PLAN.name]);
    expect(app.value("id:composer")).toBe("@agent with two files");
    expect(app.shows("id:staged:notes.txt >> text:/in the Library/")).toBe(true);
    expect(app.shows("id:staged:plan.md")).toBe(true);
    expect(w.l.assets).toHaveLength(1);
    const savedId = w.l.assets[0]!.id;

    w.knobs.failUploadOf = null;
    await app.press(button);
    await app.waitFor("id:attachments", { state: "gone" });
    expect(uploadNames()).toEqual([NOTE.name, PLAN.name, PLAN.name]);
    expect(messageAttempts()).toHaveLength(1);
    expect(w.l.assets).toHaveLength(2);
    expect(posts(w)[0]!.body).toEqual({
      text: "@agent with two files",
      ...(noteOnly ? { save_note: true } : { invoke_agent: true }),
      asset_ids: [savedId, w.l.assets.find(a => a.name === PLAN.name)!.id],
    });
    expect(w.f.state.events.get("chat:alice")?.map(e => e.kind))
      .toEqual(noteOnly ? ["message"] : ["message", "agent"]);
    expect(app.value("id:composer")).toBe("");
  });
}

test("Ask agent with files sends the typed question; the files are saved, the model gets text", async () => {
  const w = attachmentWorld();
  const app = await inAcme(w);
  app.onPick(() => [NOTE]);
  await app.press("id:attach");
  await app.waitFor("id:staged:notes.txt");
  await app.type("id:composer", "summarize the meeting");
  expect(app.shows("label:/Attached files are saved to the Library, not sent to the agent/")).toBe(true);
  await app.press("id:ask-agent");
  await app.waitFor("text:(agent) re: summarize the meeting");
  const body = posts(w).at(-1)!.body;
  expect(body).toMatchObject({ text: "summarize the meeting", invoke_agent: true, asset_ids: [w.l.assets[0]!.id] });
  expect(body.mentions).toBe(undefined); // a file never adds a recipient
});

test("a failed post keeps the draft and files; Send again doesn't upload twice", async () => {
  const w = attachmentWorld();
  const app = await inAcme(w);
  app.onPick(() => [NOTE]);
  await app.press("id:attach");
  await app.waitFor("id:staged:notes.txt");
  await app.type("id:composer", "retry me");
  w.knobs.failPosts = 1;
  await app.press("id:send");
  await app.waitFor("id:action-error");
  expect(app.shows("text:/Check the conversation before sending again/")).toBe(true);
  expect(app.value("id:composer")).toBe("retry me");
  expect(app.shows("id:staged:notes.txt >> text:/in the Library/")).toBe(true);
  expect(uploads(w).length).toBe(1);

  await app.press("id:send");
  await app.waitFor("id:attachments", { state: "gone" });
  expect(uploads(w).length).toBe(1); // reused
  expect(posts(w).at(-1)!.body).toMatchObject({ text: "retry me", asset_ids: [w.l.assets[0]!.id] });
});

test("a failed upload posts nothing and says how many files made it", async () => {
  const w = attachmentWorld();
  const app = await inAcme(w);
  app.onPick(() => [NOTE, PLAN]);
  await app.press("id:attach");
  await app.waitFor("id:staged:plan.md");
  w.knobs.failUploadOf = "plan.md";
  await app.type("id:composer", "two files");
  const before = posts(w).length;
  await app.press("id:send");
  await app.waitFor("id:action-error");
  expect(app.shows("text:/“plan.md” wasn't uploaded: it's over the size limit or the Library's quota. 1 of 2 files is in Acme's Library; nothing was sent/")).toBe(true);
  expect(app.shows("id:action-error >> text:/Send again to retry/")).toBe(true);
  expect(posts(w).length).toBe(before);
  expect(app.value("id:composer")).toBe("two files");
  w.knobs.failUploadOf = null;
  await app.press("id:send");
  await app.waitFor("id:attachments", { state: "gone" });
  expect(uploads(w).filter((u) => u.url.endsWith("name=notes.txt")).length).toBe(1);
  expect(posts(w).at(-1)!.body.asset_ids).toEqual(["notes.txt", "plan.md"].map((name) => w.l.assets.find((a) => a.name === name)!.id));
});

test("Send posts its initial files and leaves files picked during upload in the tray", async () => {
  const w = attachmentWorld();
  const app = await inAcme(w);
  app.onPick(() => [NOTE]);
  await app.press("id:attach");
  await app.waitFor("id:staged:notes.txt");
  w.knobs.uploadDelay = 750;
  await app.press("id:send");
  app.onPick(() => [PLAN]);
  await app.press("id:attach");
  await app.waitFor("id:staged:plan.md");
  await app.waitFor("id:staged:notes.txt", { state: "gone" });
  expect(app.shows("id:staged:plan.md")).toBe(true);
  expect(posts(w).at(-1)!.body.asset_ids).toEqual([w.l.assets[0]!.id]);
});

test("late picker bytes and errors cannot outlive their destination or release a newer picker", async () => {
  const file = { name: "private.txt", size: 1, status: "read" as const, file: new Blob(["x"]) };
  let finish!: (files: typeof file[]) => void;
  let fail!: (error: Error) => void;
  let cancel!: (files: never[]) => void;
  const oldSession = pickAttachments(() => new Promise((resolve) => { finish = resolve; }));
  disconnect();
  const oldPlace = pickAttachments(() => new Promise((_resolve, reject) => { fail = reject; }));
  openPlace("library");
  openChat();
  const current = pickAttachments(() => new Promise((resolve) => { cancel = resolve; }));
  finish([file]);
  fail(new Error("private path"));
  await Promise.all([oldSession, oldPlace]);
  expect(stagedFor("chat").length).toBe(0);
  expect(pickNote()).toBe(null);
  expect(picking()).toBe(true);
  cancel([]);
  await current;
  expect(picking()).toBe(false);
});

test("staged files stay with their room; another room doesn't see or send them; sign-out drops them", async () => {
  const w = attachmentWorld();
  w.f.addRoom("acme", { id: "ops", name: "ops" });
  const app = await inAcme(w);
  app.onPick(() => [NOTE]);
  await app.press("id:attach");
  await app.waitFor("id:staged:notes.txt");
  await app.press("id:room:ops");
  await app.waitFor("text:# ops");
  expect(app.shows("id:attachments")).toBe(false);
  expect(app.enabled("id:send")).toBe(false); // nothing of this room to send
  await app.press("id:room:lobby");
  await app.waitFor("id:staged:notes.txt");
  expect(uploads(w).length).toBe(0);
});

test("hasPendingAttachments: true while any place has staged files, false once they're sent or dropped", () => {
  clearAllStaged();
  expect(hasPendingAttachments()).toBe(false);
  const file = { key: "k1", name: "a.txt", mime: "text/plain", kind: "text" as const, size: 1, file: new Blob(["x"]), assetId: null };
  stage("acme\u0001lobby", [file]);
  expect(hasPendingAttachments()).toBe(true);
  markUploaded("acme\u0001lobby", "k1", "asset-1"); // uploaded, not yet posted: still pending
  expect(hasPendingAttachments()).toBe(true);
  dropSent("acme\u0001lobby", ["k1"]);
  expect(hasPendingAttachments()).toBe(false);
});

test("a deleted file shows as a neutral 'File removed' with nothing to open; the others still open", async () => {
  const w = attachmentWorld();
  const kept = w.addAsset("bob", "acme", "kept.txt", "still here");
  const ev = w.f.post("acme/lobby", {
    kind: "message",
    actor: "bob",
    actor_id: "bob",
    text: "",
    attachments: [{ removed: true }, { asset_id: kept.id, name: "kept.txt", mime_type: "text/plain", kind: "text", size_bytes: 10 }],
  } as any);
  const app = await inAcme(w);
  await app.waitFor(`id:file-removed:${ev.id}:0`);
  expect(app.shows(`id:file-removed:${ev.id}:0 >> text:File removed`)).toBe(true);
  await expect(app.press(`id:file-removed:${ev.id}:0`)).rejects.toMatchObject({ code: "ERR_NSH_TEST_NOT_FOUND" }); // nothing pressable
  await app.press(`id:file:${ev.id}:${kept.id}`);
  await app.waitFor("text:still here");
});

test("attachments parse to the contract: files with kind, tombstones, nothing else read", () => {
  const parsed = toAttachments([
    { asset_id: "a1", name: "a.txt", mime_type: "text/plain", kind: "text", size_bytes: 3, space_id: "evil", url: "https://x" },
    { asset_id: "a1", name: "dup.txt" },
    { removed: true, name: "secret.txt" },
    { name: "no-id.txt" },
    "junk",
  ]);
  expect(parsed).toEqual([{ removed: false, assetId: "a1", name: "a.txt", mimeType: "text/plain", kind: "text", sizeBytes: 3 }, { removed: true }]);
  expect(toAttachments(undefined)).toEqual([]);
});

test("picks are checked before staging: type, size, empty, duplicates and the 10-file limit", () => {
  const blob = new Blob(["x"]);
  const p = (name: string, size = 1, status: "read" | "tooLarge" = "read") => ({ name, size, status, file: status === "read" ? blob : null });
  const r = checkPicked([p("a.txt"), p("a.txt"), p("tool.exe"), p("big.pdf", 2048, "tooLarge"), p("empty.md", 0)], [], 1024);
  expect(r.files.map((f) => f.name)).toEqual(["a.txt"]);
  expect(r.problems.length).toBe(3);
  const ten = Array.from({ length: 10 }, (_, i) => ({ key: `k${i}`, name: `f${i}.txt`, mime: "text/plain", kind: "text" as const, size: 1, file: blob, assetId: null }));
  expect(checkPicked([p("b.txt")], ten, 1024).problems[0]).toContain("at most 10 files");
});

void (null as unknown as App);
