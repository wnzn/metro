// Quick-search file previews against the fake library server, through the
// real store and transport (test/QuickHarness.tsx mounts rows as the shell
// does). The test network carries strings only, so the media fixtures are
// real files whose every byte is ASCII: a 16-bit WAV whose first half is
// loud and second half silent, a silent WAV, Markdown text. Full-range
// decoding (sines, every PCM layout, deflated DOCX) is in
// quickPreviewData.test.ts. These are headless engine runs: they check what
// is read and drawn, not that a native player shows a video frame.

import { expect, open, test } from "nsh/test";
import type { App } from "nsh/test";
import { BASE, TOKENS } from "../../../test/fakeServer";
import { libraryWorld } from "./test/fixtures";

type W = ReturnType<typeof libraryWorld>;

/** Signs in. The Connect press is returned, not awaited: the driver's press waits for in-flight reads, which a test may hold. */
async function signIn(w: W, user = "alice"): Promise<{ app: App; connecting: Promise<void> }> {
  const app = await open("./test/QuickHarness.tsx", { network: w.network as any, platform: "linux", width: 1100, height: 900 });
  await app.clear("id:server-url");
  await app.type("id:server-url", BASE);
  await app.type("id:access-token", TOKENS[user]!);
  const connecting = app.press("id:connect");
  await poll(() => app.shows("id:h-toggle"));
  return { app, connecting };
}

const sleep = (ms: number) => new Promise((r) => setTimeout(r, ms));

async function poll(cond: () => boolean, ms = 20_000) {
  const end = Date.now() + ms;
  while (!cond()) {
    if (Date.now() > end) throw new Error("condition not reached: " + cond.toString());
    await new Promise((r) => setTimeout(r, 10));
  }
}

/** A mono 16-bit, 4096 Hz WAV of `frames` samples, each 0x7F00 (loud) or 0 (silent); every byte is under 0x80. */
function asciiWav(loud: (i: number) => boolean, frames: number): string {
  const le32 = (n: number) => String.fromCharCode(n & 0xff, (n >> 8) & 0xff, (n >> 16) & 0xff, (n >> 24) & 0xff);
  const le16 = (n: number) => String.fromCharCode(n & 0xff, (n >> 8) & 0xff);
  const data = Array.from({ length: frames }, (_, i) => (loud(i) ? "\u0000\u007f" : "\u0000\u0000")).join("");
  const out = "RIFF" + le32(36 + data.length) + "WAVE" + "fmt " + le32(16) + le16(1) + le16(1) + le32(4096) + le32(8192) + le16(2) + le16(16) + "data" + le32(data.length) + data;
  for (let i = 0; i < out.length; i++) if (out.charCodeAt(i) > 0x7f) throw new Error(`byte ${i} isn't ASCII`);
  return out;
}

/** An MP3 whose ID3v2.3 tag holds Latin-1 title, artist and album frames, then a stand-in for audio frames; ASCII only. */
function asciiId3(title: string, artist: string, album: string): string {
  const frame = (id: string, text: string) => id + "\u0000\u0000\u0000" + String.fromCharCode(text.length + 1) + "\u0000\u0000" + "\u0000" + text;
  const body = frame("TIT2", title) + frame("TPE1", artist) + frame("TALB", album);
  const n = body.length;
  return "ID3\u0003\u0000\u0000" + String.fromCharCode((n >> 21) & 0x7f, (n >> 14) & 0x7f, (n >> 7) & 0x7f, n & 0x7f) + body + "audio frames";
}

const reads = (w: W, id?: string) => w.l.log.filter((r) => r.path.endsWith("/content") && (!id || r.path === `/v1/library/assets/${id}/content`));

function seed(w: W) {
  const notes = w.addAsset("alice", null, "notes.md", "# Rollback plan\n\nRevert the **tag**, rebuild, then run the [smoke test](http://x).\n");
  const half = w.addAsset("alice", null, "standup.wav", asciiWav((i) => i < 400, 800));
  const quiet = w.addAsset("alice", null, "quiet.wav", asciiWav(() => false, 800));
  const photo = w.addAsset("alice", null, "photo.png", "PNGDATA");
  const clip = w.addAsset("alice", null, "clip.mp4", "MP4DATA");
  const pdf = w.addAsset("alice", null, "contract.pdf", "%PDF-1.7 fake");
  const song = w.addAsset("alice", null, "song.mp3", asciiId3("Harbor Song", "Ana Reyes", "Dockside"));
  const untagged = w.addAsset("alice", null, "raw.mp3", "\u0000\u0000 no tag here");
  const word = w.addAsset("alice", null, "memo.docx", "PK not a real docx");
  const huge = w.addAsset("alice", null, "huge.png", "PNGBIG");
  huge.size_bytes = 5 * 1024 * 1024;
  const acme = w.addAsset("bob", "acme", "roadmap.png", "PNGACME");
  return { notes, half, quiet, photo, clip, pdf, song, untagged, word, huge, acme };
}

test("each file shows what it holds, or says why not; reads are authenticated, ranged for text and WAV, and skipped for PDF, MP3 and oversized files", async () => {
  const w = libraryWorld();
  const s = seed(w);
  const { app } = await signIn(w);
  await app.waitFor(`id:quick-preview-text:${s.notes.id}`);
  await app.waitFor(`id:quick-preview-wave:${s.half.id}`);
  await app.waitFor(`id:quick-preview-wave:${s.quiet.id}`);
  await app.waitFor(`id:quick-preview-image:${s.photo.id}`);
  await app.waitFor(`id:quick-preview-video:${s.clip.id}`);
  await app.waitFor(`id:quick-preview-none:${s.word.id}`);
  const text = app.texts().join(" | ");

  // Text: the file's own words, Markdown marks dropped.
  expect(text).toContain("Rollback plan Revert the tag, rebuild, then run the smoke test.");
  // WAV: real peaks. Bars with signal and bars of pure silence are drawn differently
  // (heights aren't exposed to the test driver; exact peaks are checked in quickPreviewData.test.ts).
  const bar = (id: string, i: number) => app.viewStyle(`id:quick-preview-wave:${id}:bar:${i}`)?.backgroundColor;
  const sounding = bar(s.half.id, 0);
  const flat = bar(s.half.id, 39);
  expect(sounding).not.toBe(flat);
  for (let i = 0; i < 20; i++) expect(bar(s.half.id, i)).toBe(sounding);
  for (let i = 20; i < 40; i++) expect(bar(s.half.id, i)).toBe(flat);
  for (let i = 0; i < 40; i++) expect(bar(s.quiet.id, i)).toBe(flat);
  expect(text).toContain("0:00 · silent");
  // Honest fallbacks, never a made-up preview: the type's glyph, with the reason in its accessible name (no paragraph in the list).
  const reason = (id: string, why: string) => app.shows(`id:quick-preview-none:${id}`) && app.shows(`label:No preview of ${nameOf(id)}. ${why}`);
  const nameOf = (id: string) => Object.values(s).find((a) => a.id === id)!.name;
  expect(reason(s.pdf.id, "PDF: no preview here. Open it to read.")).toBe(true);
  expect(text).not.toContain("PDF: no preview here. Open it to read.");
  // MP3: its own tags (metadata, not a picture of its sound), or an honest line without them.
  await app.waitFor(`id:quick-preview-tags:${s.song.id}`);
  expect(app.texts()).toContain("“Harbor Song” · Ana Reyes · Dockside");
  await app.waitFor(`id:quick-preview-none:${s.untagged.id}`);
  expect(reason(s.untagged.id, "MP3 audio without a title or artist tag. Open it to play.")).toBe(true);
  expect(reason(s.word.id, "Word file: no readable text found.")).toBe(true);
  expect(reason(s.huge.id, "PNG image of 5.0 MB: too large for a quick preview.")).toBe(true);

  // What was read: one authenticated read per previewable file; text and WAV a prefix only.
  for (const id of [s.pdf.id, s.huge.id]) expect(reads(w, id)).toHaveLength(0);
  expect(reads(w, s.song.id)[0]!.headers["range"]).toBe("bytes=0-65535");
  expect(reads(w, s.notes.id)[0]!.headers["range"]).toBe("bytes=0-8191");
  expect(reads(w, s.half.id)[0]!.headers["range"]).toBe("bytes=0-1048575");
  expect(reads(w, s.photo.id)[0]!.headers["range"]).toBeUndefined();
  for (const id of [s.notes.id, s.half.id, s.quiet.id, s.photo.id, s.clip.id, s.song.id, s.untagged.id, s.word.id, s.acme.id]) {
    expect(reads(w, id)).toHaveLength(1);
    expect(reads(w, id)[0]!.headers["authorization"]).toBe(`Bearer ${TOKENS.alice}`);
  }
  expect(w.l.log.some((r) => r.url.includes(TOKENS.alice!))).toBe(false);
  // Previews never index or search anything.
  expect(w.l.log.some((r) => r.path === "/v1/library/index" || r.path === "/v1/library/search")).toBe(false);
});

test("reads are held to two in flight; hiding the rows aborts them, queued ones are never read, and a late answer draws nothing", async () => {
  // The test network answers its handlers one at a time, so "two at once" is
  // checked as: with every read held, hiding the rows leaves at most the two
  // already started ever reaching the server.
  const w = libraryWorld();
  const s = seed(w);
  const release = w.holdContent();
  const { app, connecting } = await signIn(w);
  await poll(() => w.l.contentParked >= 1);
  await sleep(300); // not app.wait: the driver's waits wait for the held reads too
  expect(app.shows(`id:quick-preview-loading:${s.notes.id}`)).toBe(true);

  const toggling = app.press("id:h-toggle"); // every row unmounts while reads are held
  await poll(() => !app.shows("id:h-row:row-0"));
  release();
  await Promise.all([toggling, connecting]);
  await app.wait(300);
  expect(reads(w).length).toBeLessThanOrEqual(2); // nothing queued was read after the rows went
  expect(reads(w, s.notes.id)).toHaveLength(0);
  expect(app.shows(`id:quick-preview-text:${s.notes.id}`)).toBe(false);

  // Shown again: read afresh (nothing was kept), and every preview completes.
  await app.press("id:h-toggle");
  await app.waitFor(`id:quick-preview-text:${s.notes.id}`);
  await app.waitFor(`id:quick-preview-wave:${s.half.id}`);
  expect(reads(w, s.notes.id)).toHaveLength(1);
});

test("two rows of one file share one read; pointing a row at another file releases the old one", async () => {
  const w = libraryWorld();
  const s = seed(w);
  const { app } = await signIn(w);
  const first = s.acme.id; // the list's first file (added last)
  await app.waitFor(`id:quick-preview-image:${first}`);
  await app.press("id:h-dup");
  await app.waitFor("id:h-row:row-dup");
  expect(app.count(`id:quick-preview-image:${first}`)).toBe(2);
  expect(reads(w, first)).toHaveLength(1);

  // One of the two rows goes: the other keeps the shared preview, nothing is read again.
  await app.press("id:h-dup");
  await app.waitUntil(() => app.count(`id:quick-preview-image:${first}`) === 1);
  expect(reads(w, first)).toHaveLength(1);
  // The row is pointed at another file (a new query): the old file's preview is released...
  await app.press("id:h-swap");
  await app.waitUntil(() => app.count(`id:quick-preview-image:${first}`) === 0);
  // ...so pointing it back reads the file afresh: nothing was kept.
  await app.press("id:h-swap");
  await app.waitFor(`id:quick-preview-image:${first}`);
  expect(reads(w, first)).toHaveLength(2);
});

test("losing a Space drops its previews at once; sign-out drops everything and a late read never reaches the next person", async () => {
  const w = libraryWorld();
  const s = seed(w);
  const { app } = await signIn(w);
  await app.waitFor(`id:quick-preview-image:${s.acme.id}`);
  w.f.state.spaces.get("acme")!.members.delete("alice");
  await app.waitFor(`id:quick-preview-none:${s.acme.id}`, { timeout: 15_000 });
  expect(app.shows(`id:quick-preview-image:${s.acme.id}`)).toBe(false);
  expect(app.shows(`label:/^No preview of roadmap\\.png\\. You're no longer a member of this file's Space\\./`)).toBe(true); // said to screen readers, not drawn
  expect(app.shows(`id:quick-preview-image:${s.photo.id}`)).toBe(true); // personal files stay

  // Hide, then park reads, show again and sign out while they're parked.
  await app.press("id:h-toggle");
  const release = w.holdContent();
  const showing = app.press("id:h-toggle");
  await poll(() => w.l.contentParked >= 1);
  const signingOut = app.press("id:h-signout");
  await poll(() => app.shows("id:connect"));
  release();
  await Promise.all([showing, signingOut]);
  await app.type("id:access-token", TOKENS.bob!);
  await app.press("id:connect");
  await app.waitFor("id:h-toggle");
  await app.waitUntil(() => w.l.contentParked === 0);
  await app.wait(300);
  const text = app.texts().join(" | ");
  expect(text).not.toContain("Rollback plan");
  expect(text).not.toContain("notes.md");
  expect(app.shows(`id:quick-preview-image:${s.photo.id}`)).toBe(false);
});

test("past the row budget a preview says so instead of reading", async () => {
  const w = libraryWorld();
  const ids = Array.from({ length: 14 }, (_, i) => w.addAsset("alice", null, `n${String(i).padStart(2, "0")}.txt`, `note ${i}`).id);
  const { app } = await signIn(w);
  await app.waitUntil(() => ids.filter((id) => app.shows(`id:quick-preview-text:${id}`)).length === 12);
  await app.wait(200);
  // The two past the budget show their type's glyph, with the reason in its accessible name.
  expect(ids.filter((id) => app.shows(`id:quick-preview-none:${id}`))).toHaveLength(2);
  expect(app.count("label:/Preview skipped: too many files at once\\./")).toBe(2);
  expect(reads(w).length).toBe(12);
});
