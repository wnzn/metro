// Library (Documents & media) and Search's Documents & media mode against the
// shared fake server plus the library routes (test/fixtures.ts). Real NSH
// engine and the real app entry (the media model choice through
// test/ModelHarness.tsx), no network. Tokens are the fake server's own,
// never real ones. These are headless engine runs: they check what the app
// requests and shows, not native playback.

import { expect, open, test } from "nsh/test";
import type { App } from "nsh/test";
import { BASE, TOKENS } from "../../../test/fakeServer";
import { libraryWorld } from "./test/fixtures";
import { launched } from "../../../test/launched";

type W = ReturnType<typeof libraryWorld>;

async function connect(app: App, user: string) {
  await app.clear("id:server-url");
  await app.type("id:server-url", BASE);
  await app.type("id:access-token", TOKENS[user]!);
  await app.press("id:connect");
}

async function signIn(w: W, user: "alice" | "bob" | "carol" = "alice"): Promise<App> {
  const app = await open("../../../App.tsx", { modules: launched(), network: w.network as any, platform: "linux", width: 1280, height: 900 });
  await connect(app, user);
  await app.waitFor("id:top-search");
  return app;
}

/** The Library page, from the sidebar. */
async function openLibrary(app: App) {
  await app.press("id:nav-library");
  await app.waitFor("id:library-view");
}

/** Return in the empty search box at the top opens Search without searching. */
async function openSearch(app: App) {
  await app.submit("id:top-search");
  await app.waitFor("id:search-view");
}

async function openFileSearch(app: App) {
  await openSearch(app);
  await app.press("id:search-surface:library");
  await app.waitFor("id:library-search");
}

/** Resolves when `cond()` holds (checked every 10 ms); fails after 20 s. */
async function poll(cond: () => boolean, ms = 20_000) {
  const end = Date.now() + ms;
  while (!cond()) {
    if (Date.now() > end) throw new Error("condition not reached: " + cond.toString());
    await new Promise((r) => setTimeout(r, 10));
  }
}

const text = (app: App) => app.texts().join(" | ");
const enc = new TextEncoder();
const lib = (w: W, path: string, method = "GET") => w.l.log.filter((r) => r.path === path && r.method === method);
const contentReads = (w: W) => w.l.log.filter((r) => r.path.endsWith("/content"));

function seed(w: W) {
  const mine = w.addAsset("alice", null, "notes.md", "# Notes\n\nIntro paragraph.\n\nThe rollback plan: revert the tag, rebuild, then run the smoke test.\n\nOutro.");
  const acmeByBob = w.addAsset("bob", "acme", "roadmap.png", "PNGDATA");
  const sideByBob = w.addAsset("bob", "side", "demo.mp4", "MP4DATA");
  const bobsOwn = w.addAsset("bob", null, "bob-private.txt", "bob's private notes");
  w.f.addSpace("secret", "Secret", "carol");
  const carols = w.addAsset("carol", "secret", "carol-secret.pdf", "%PDF-1.7 secret");
  return { mine, acmeByBob, sideByBob, bobsOwn, carols };
}

test("Library is its own page listing only what I can access; Search's file mode lists nothing and typing sends nothing", async () => {
  const w = libraryWorld();
  const s = seed(w);
  const app = await signIn(w);
  expect(lib(w, "/v1/library/assets").length).toBe(0); // nothing listed before the Library opens
  await openLibrary(app);
  await app.waitFor(`id:library-file:${s.mine.id}`);
  expect(app.shows(`id:library-file:${s.acmeByBob.id}`)).toBe(true);
  expect(app.shows(`id:library-file:${s.sideByBob.id}`)).toBe(true);
  // Nobody else's personal files, no Space I'm not in.
  expect(text(app)).not.toContain("bob-private.txt");
  expect(text(app)).not.toContain("carol-secret.pdf");
  expect(text(app)).toContain("Uploaded by you · Markdown · ");
  expect(text(app)).toContain("Uploaded by bob · PNG · 7 B");
  expect(text(app)).toContain("Personal");
  expect(text(app)).toContain("Acme");
  expect(lib(w, "/v1/library/assets")[0]!.url).toBe(`${BASE}/v1/library/assets?scope=all`);
  // No page intro, no media model form here; with no media model there's no Index line either.
  expect(app.shows("id:library-model-picker")).toBe(false);
  expect(app.shows("id:library-model-status")).toBe(false);
  expect(lib(w, "/v1/library/index", "POST").length).toBe(0);

  // A first Index with no media model leads straight to the choice in Settings › Model choices: nothing is sent.
  await app.press(`id:library-actions:${s.mine.id}`);
  await app.waitFor("text:Choose a media model to index");
  await app.press(`id:library-index:${s.mine.id}`);
  await app.waitFor("id:routing-settings >> text:Model choices");
  await app.waitFor("id:library-model");
  await app.waitFor("id:library-model-asked");
  expect(app.shows("id:library-model-picker")).toBe(true);
  expect(lib(w, "/v1/library/index", "POST").length).toBe(0);
  expect(lib(w, "/v1/library/settings", "PUT").length).toBe(0);
  await app.back(); // Escape closes Settings, back on the Library
  await app.waitFor("id:library-view");

  await app.press("id:library-list-scope");
  await app.waitFor("text:Side project");
  expect(app.shows("text:Secret")).toBe(false);
  await app.press("text:Acme >> nth=2");
  await app.waitFor(`id:library-file:${s.acmeByBob.id}`);
  await app.waitUntil(() => !app.shows(`id:library-file:${s.mine.id}`));
  expect(lib(w, "/v1/library/assets").at(-1)!.url).toBe(`${BASE}/v1/library/assets?scope=space&space_id=acme`);

  // Typing in the box at the top edits the files query only: it opens nothing and sends nothing.
  const before = w.l.log.length;
  await app.type("id:top-search", "rollback");
  expect(app.shows("id:library-view")).toBe(true);
  expect(w.l.log.length).toBe(before);
  await app.clear("id:top-search");

  // Search opens on messages; its Documents & media mode has no upload or file list.
  await app.press("id:nav-chat");
  await openSearch(app);
  expect(app.shows("id:search-form")).toBe(true);
  expect(app.shows("id:library-search")).toBe(false);
  const lists = lib(w, "/v1/library/assets").length;
  await app.press("id:search-surface:library");
  await app.waitFor("id:library-search-start");
  expect(app.shows("id:library-choose")).toBe(false);
  expect(app.shows(`id:library-file:${s.mine.id}`)).toBe(false);
  expect(lib(w, "/v1/library/assets").length).toBe(lists);
  expect(lib(w, "/v1/library/search", "POST").length).toBe(0);
  // From there, the Library is one press away.
  await app.press("id:search-open-library");
  await app.waitFor("id:library-view");
});

test("a revoked Documents & media model is shown as unavailable until explicitly cleared and saved", async () => {
  const w = libraryWorld();
  w.addMediaConnection("alice", "device-embeddinggemma-2", "Another media model");
  const revoked = w.addMediaConnection("bob", "embeddinggemma-2", "Previously shared");
  w.l.settings.set("alice", revoked);
  const app = await signIn(w);
  await app.press("id:nav-settings");
  await app.press("id:nav-settings:routing");
  await app.waitFor("id:library-model-missing");
  expect(app.shows("text:The saved selection is unavailable; no other model takes its place.")).toBe(true);
  expect(lib(w, "/v1/library/settings", "PUT")).toHaveLength(0);
  await app.press("id:library-model-clear");
  expect(w.l.settings.get("alice")).toBe(revoked);
  await app.press("id:library-model-save");
  await app.waitFor("id:library-model-notice");
  const writes = lib(w, "/v1/library/settings", "PUT");
  expect(writes).toHaveLength(1);
  expect(writes[0]!.body).toBe(JSON.stringify({ embedding_id: null }));
});

test("uploads come from the system picker as raw bytes, to the chosen Space, within the server's limit; nothing goes to a model", async () => {
  const w = libraryWorld();
  w.l.maxFileBytes = 1024;
  const app = await signIn(w);
  await openLibrary(app);
  await app.waitFor("id:library-files-empty");
  app.onPick((pick) => {
    expect(pick.kind).toBe("files");
    expect(pick.multiple).toBe(true);
    expect(pick.maxSize).toBe(1024);
    expect(pick.filters[0]!.extensions).toContain("pdf");
    expect(pick.filters[0]!.extensions).toContain("webm");
    return [
      { name: "plan.md", type: "text/markdown", data: enc.encode("# Plan\n\nShip it.") },
      { name: "huge.pdf", type: "application/pdf", data: new Uint8Array(4096) },
      { name: "tool.exe", type: "application/octet-stream", data: enc.encode("MZ") },
    ];
  });
  // Upload ▾ says where each destination puts the files, and who sees them; choosing one opens the picker.
  await app.press("id:library-choose");
  await app.waitFor("text:/up to 1.0 KB each/");
  expect(text(app)).toContain("Only you see these files.");
  expect(text(app)).toContain("Adds to Acme's library · its members can see them.");
  expect(app.picks().length).toBe(0);
  await app.press("id:library-choose:space:acme");
  await app.waitFor("id:library-upload-outcomes");
  expect(app.picks().length).toBe(1);
  await app.waitFor("text:/plan.md: Uploaded to Acme/");
  expect(text(app)).toContain("Nothing was sent to a model.");
  expect(text(app)).toContain("huge.pdf: Not uploaded: it's 4.0 KB; files are limited to 1.0 KB.");
  expect(text(app)).toContain("tool.exe: Not uploaded: this type isn't supported.");

  const posts = lib(w, "/v1/library/assets", "POST");
  expect(posts.length).toBe(1);
  expect(posts[0]!.url).toBe(`${BASE}/v1/library/assets?scope=space&space_id=acme&name=plan.md`);
  expect(posts[0]!.headers["content-type"]).toBe("text/markdown");
  expect(posts[0]!.headers["authorization"]).toBe(`Bearer ${TOKENS.alice}`);
  expect(posts[0]!.body).toBe("# Plan\n\nShip it.");
  // The credential never rides in a URL.
  expect(w.l.log.some((r) => r.url.includes(TOKENS.alice!))).toBe(false);
  expect(w.l.assets[0]!.space_id).toBe("acme");
  await app.waitFor(`id:library-file:${w.l.assets[0]!.id}`);
  expect(lib(w, "/v1/library/index", "POST").length).toBe(0);
  expect(lib(w, "/v1/library/search", "POST").length).toBe(0);
});

test("the media model settings offer only media-capable connections and disclose the data flow; Index sends one file only on request", async () => {
  const w = libraryWorld();
  const s = seed(w);
  w.f.addConnection("alice", "custom-embeddings", "Text embeddings", "nomic-embed");
  const own = w.addMediaConnection("alice", "device-embeddinggemma-2", "My EG2");
  w.addMediaConnection("bob", "embeddinggemma-2", "Bob's EG2 server", { visibility: "spaces", space_ids: ["acme"] });
  const app = await open("./test/ModelHarness.tsx", { modules: launched(), network: w.network as any, platform: "linux", width: 1280, height: 900 });
  await connect(app, "alice");
  await app.waitFor("id:library-model");
  await app.waitFor(`id:library-file:${s.mine.id}`);
  // Index is in the row's More menu; without a saved model it leads to the choice and sends nothing.
  expect(app.shows(`id:library-index:${s.mine.id}`)).toBe(false);
  await app.press(`id:library-actions:${s.mine.id}`);
  await app.waitFor(`id:library-index:${s.mine.id}`);
  expect(text(app)).toContain("Choose a media model to index");
  await app.press(`id:library-index:${s.mine.id}`);
  await app.waitFor("id:library-model-asked");
  expect(lib(w, "/v1/library/index", "POST").length).toBe(0);
  expect(text(app)).toContain("Nothing is chosen, so nothing can be indexed or searched.");

  await app.press("id:library-model-picker");
  await app.waitFor("text:/Bob's EG2 server/");
  expect(app.shows("text:/Text embeddings/")).toBe(false);
  await app.press("text:/Bob's EG2 server/");
  await app.waitFor("id:library-model-dataflow");
  expect(text(app)).toContain("Index sends the whole original file to it, which extracts, renders or splits it there.");
  expect(text(app)).toContain("Uploading and opening files send it nothing.");
  expect(text(app)).toContain("never whole files");
  expect(text(app)).toContain("Shared by bob: it runs on bob's endpoint, so whoever runs that endpoint can see the files and queries sent, and bob may pay for it.");
  // Picking isn't saving: nothing is used yet.
  expect(lib(w, "/v1/library/settings", "PUT").length).toBe(0);

  await app.press("id:library-model-picker");
  await app.press("text:/My EG2/");
  await app.waitFor("text:/It runs in your own metro-connector on your device/");
  await app.press("id:library-model-save");
  await app.waitFor("text:Saved: My EG2.");
  expect(lib(w, "/v1/library/settings", "PUT")[0]!.body).toBe(JSON.stringify({ embedding_id: own }));
  expect(lib(w, "/v1/library/index", "POST").length).toBe(0);
  await app.waitFor("text:/Index sends the whole file to My EG2; whoever runs it can see it/");
  expect(app.shows("id:library-model-asked")).toBe(false);

  await app.press(`id:library-actions:${s.mine.id}`);
  await app.press(`id:library-index:${s.mine.id}`);
  await app.waitFor(`id:library-index-state:${s.mine.id}`);
  await app.waitFor("text:Indexed 3 parts in your own index.");
  const index = lib(w, "/v1/library/index", "POST");
  expect(index.length).toBe(1);
  expect(JSON.parse(index[0]!.body!)).toEqual({ asset_id: s.mine.id });
});

test("Delete sits in each row's More menu, asks first, and is offered only to the uploader or the Space owner", async () => {
  const w = libraryWorld();
  const s = seed(w);
  const app = await signIn(w);
  await openLibrary(app);
  await app.waitFor(`id:library-file:${s.mine.id}`);
  // Each row: one visible Open, everything else in More.
  expect(app.shows(`id:library-open:${s.mine.id}`)).toBe(true);
  expect(app.shows(`id:library-index:${s.mine.id}`)).toBe(false);
  expect(app.shows(`id:library-delete:${s.acmeByBob.id}`)).toBe(false); // folded away until the menu opens
  // Alice owns Acme (Bob's file there), but is only a member of Side: its menu has Index, not Delete.
  await app.press(`id:library-actions:${s.sideByBob.id}`);
  await app.waitFor(`id:library-index:${s.sideByBob.id}`);
  expect(app.shows(`id:library-delete:${s.sideByBob.id}`)).toBe(false);
  await app.back();
  await app.waitFor(`id:library-index:${s.sideByBob.id}`, { state: "gone" });

  await app.press(`id:library-actions:${s.acmeByBob.id}`);
  await app.waitFor(`id:library-delete:${s.acmeByBob.id}`);
  await app.press(`id:library-delete:${s.acmeByBob.id}`);
  await app.waitFor(`id:library-confirm-delete:${s.acmeByBob.id}`);
  expect(text(app)).toContain("It's removed for every member of Acme, with its index.");
  expect(lib(w, `/v1/library/assets/${s.acmeByBob.id}`, "DELETE").length).toBe(0); // choosing Delete only asks
  await app.press(`id:library-delete-cancel:${s.acmeByBob.id}`);
  expect(app.shows(`id:library-confirm-delete:${s.acmeByBob.id}`)).toBe(false);
  expect(lib(w, `/v1/library/assets/${s.acmeByBob.id}`, "DELETE").length).toBe(0);

  await app.press(`id:library-actions:${s.acmeByBob.id}`);
  await app.press(`id:library-delete:${s.acmeByBob.id}`);
  await app.press(`id:library-delete-confirm:${s.acmeByBob.id}`);
  await app.waitFor("text:Deleted roadmap.png and its index.");
  expect(app.shows(`id:library-file:${s.acmeByBob.id}`)).toBe(false);
  expect(lib(w, `/v1/library/assets/${s.acmeByBob.id}`, "DELETE").length).toBe(1);
});

test("a file opened from the Library reads its bytes with auth, and leaving the Library closes it", async () => {
  const w = libraryWorld();
  const s = seed(w);
  const app = await signIn(w);
  await openLibrary(app);
  await app.press(`id:library-open:${s.mine.id}`);
  await app.waitFor("id:library-viewer-text");
  expect(text(app)).toContain("‹ Back to files");
  const reads = lib(w, `/v1/library/assets/${s.mine.id}/content`);
  expect(reads.length).toBe(1);
  expect(reads[0]!.headers["authorization"]).toBe(`Bearer ${TOKENS.alice}`);
  await app.press("id:nav-chat");
  await app.press("id:nav-library");
  await app.waitFor("id:library-view");
  expect(app.shows("id:library-viewer")).toBe(false);
});

function seedHits(w: W, s: ReturnType<typeof seed>) {
  const pdf = w.addAsset("alice", "acme", "contract.pdf", "%PDF-1.7 fake");
  const wav = w.addAsset("alice", null, "standup.wav", "RIFFfakeWAVE");
  const start = "# Notes\n\nIntro paragraph.\n\n".length;
  w.l.hits.push(
    { asset_id: s.mine.id, locator: { kind: "text", start, end: start + 17 }, excerpt: "The rollback plan", score: 0.93, score_kind: "classifier" },
    { asset_id: pdf.id, locator: { kind: "page", page: 3 }, excerpt: "Either party may terminate early with 30 days notice.", score: 0.88, score_kind: "classifier" },
    { asset_id: s.acmeByBob.id, locator: { kind: "image" }, score: 0.71, score_kind: "cosine" },
    // A "classifier" score without an excerpt is impossible: shown as similarity.
    { asset_id: s.sideByBob.id, locator: { kind: "time", start_ms: 12_000, end_ms: 23_000 }, score: 0.66, score_kind: "classifier" },
    { asset_id: wav.id, locator: { kind: "time", start_ms: 61_000, end_ms: 72_000 }, score: 0.6, score_kind: "cosine" },
    { asset_id: s.carols.id, locator: { kind: "page", page: 1 }, excerpt: "carol's secret", score: 0.99, score_kind: "classifier" },
  );
  return { pdf, wav };
}

async function searchFiles(app: App, query: string) {
  await app.clear("id:top-search");
  await app.type("id:top-search", query);
  await app.submit("id:top-search");
  await app.waitFor("id:library-results");
}

test("file search: chips filter, Return runs it, results carry no raw scores, only image hits get authenticated thumbnails, and opening jumps to the match", async () => {
  const w = libraryWorld();
  const s = seed(w);
  const { pdf, wav } = seedHits(w, s);
  w.l.settings.set("alice", w.addMediaConnection("alice", "embeddinggemma-2", "EG2"));
  const app = await signIn(w);
  await openFileSearch(app);
  await app.waitFor("id:library-search-disclosure");
  expect(text(app)).toContain("sends your query to EG2");
  // Filters change nothing until the search runs.
  await app.press("id:library-search-kind:document");
  await app.press("id:library-search-kind:any");
  expect(lib(w, "/v1/library/search", "POST").length).toBe(0);

  await searchFiles(app, "rollback plan");
  expect(JSON.parse(lib(w, "/v1/library/search", "POST")[0]!.body!)).toEqual({ query: "rollback plan", scope: "all", limit: 10 });
  expect(text(app)).toContain("5 matches for “rollback plan” · Everything you can access");
  expect(text(app)).not.toContain("carol's secret");
  expect(text(app)).toContain("Documents reranked by System One");
  // No raw numbers on results.
  expect(text(app)).not.toContain("relevance 0.");
  expect(text(app)).not.toContain("similarity 0.");
  expect(text(app)).not.toContain("No text excerpt");
  expect(text(app)).toContain("Page 3");
  expect(text(app)).toContain("0:12–0:23");
  expect(text(app)).toContain("Play from 0:12");

  // Exactly one thumbnail read: the one image hit, with the bearer in a header.
  await app.waitUntil(() => contentReads(w).length === 1);
  const thumb = contentReads(w)[0]!;
  expect(thumb.path).toBe(`/v1/library/assets/${s.acmeByBob.id}/content`);
  expect(thumb.headers["authorization"]).toBe(`Bearer ${TOKENS.alice}`);
  expect(app.shows(`id:library-hit-thumb:${s.acmeByBob.id}:image:0`)).toBe(true);
  expect(w.l.log.some((r) => r.url.includes(TOKENS.alice!))).toBe(false);

  // The whole card opens a text section, marked.
  await app.press(`id:library-hit-open:${s.mine.id}:text:${"# Notes\n\nIntro paragraph.\n\n".length}`);
  await app.waitFor("id:library-viewer-text");
  await app.waitFor("text:/The rollback plan/");
  expect(text(app)).toContain("The matching section is marked");
  expect(text(app)).toContain("‹ Back to results");
  const reads = lib(w, `/v1/library/assets/${s.mine.id}/content`);
  expect(reads.length).toBe(1);
  expect(reads[0]!.headers["authorization"]).toBe(`Bearer ${TOKENS.alice}`);
  await app.press("id:library-viewer-back");
  await app.waitFor("id:library-results");

  // A video keeps the platform's player and its controls.
  await app.press("text:Play from 0:12");
  await app.waitFor("id:library-viewer-player");
  expect(text(app)).toContain("The player starts at the match (0:12–0:23).");
  expect(app.shows("id:library-play-match")).toBe(true);
  expect(app.shows("id:library-play-toggle")).toBe(true);
  await app.press("id:library-viewer-back");

  // Audio: a compact player. No time or match mark is shown before the player itself reported its length.
  await app.press(`id:library-hit-open:${wav.id}:time:61000`);
  await app.waitFor("id:library-viewer-player:timeline");
  expect(app.shows("id:library-viewer-player:toggle")).toBe(true);
  expect(app.enabled("id:library-viewer-player:timeline")).toBe(false);
  expect(app.shows("id:library-viewer-player:match")).toBe(false);
  // The match is asked of the player at load, but no position is shown until the player itself reports one.
  expect(app.shows("id:library-viewer-player:position")).toBe(true);
  expect(app.texts().filter((t) => t === "–:––").length).toBe(2); // position and length both unknown
  // One play control for audio: the player's own button, which starts at the match.
  expect(app.shows("id:library-play-match")).toBe(false);
  expect(app.shows("id:library-play-toggle")).toBe(false);
  expect(app.count("id:library-viewer-player:toggle")).toBe(1);
  await app.press("id:library-viewer-player:toggle"); // plays (asks the host to seek first); still no reported position
  expect(app.texts().filter((t) => t === "–:––").length).toBe(2);
  await app.press("id:library-viewer-back");

  // A PDF page: the indexed text of that page, and an honest way to open the original.
  await app.press("text:Open at page 3");
  await app.waitFor("id:library-viewer-document");
  expect(text(app)).toContain("Indexed text of page 3");
  expect(text(app)).toContain("then go to page 3");
  expect(lib(w, `/v1/library/assets/${pdf.id}/content`).length).toBe(1);
  // Opening never indexes or sends a file anywhere.
  expect(lib(w, "/v1/library/index", "POST").length).toBe(0);
});

test("similarity-only answers say so; without a media model Search says what's missing before anything is sent", async () => {
  const w = libraryWorld();
  const s = seed(w);
  seedHits(w, s);
  w.l.ranking = "embeddings";
  const app = await signIn(w);
  await openFileSearch(app);
  await app.waitFor("id:library-search-no-model");
  expect(app.shows("id:library-search-submit")).toBe(false); // one query field: Return in it runs the search
  await app.type("id:top-search", "x");
  await app.submit("id:top-search");
  await app.waitFor("id:library-search-error");
  expect(text(app)).toContain("File search needs a media model: choose a media embedding model in Settings to search your files.");
  expect(app.shows("id:library-search-open-providers")).toBe(true);
  w.l.settings.set("alice", w.addMediaConnection("alice", "embeddinggemma-2", "EG2"));
  await searchFiles(app, "rollback");
  expect(text(app)).toContain("Similarity order");
  expect(text(app)).toContain("System One didn't rerank these.");
  expect(text(app)).not.toContain("reranked by System One");
});

test("losing a Space drops its files, results, thumbnails and an open preview; signing out discards a late file read", async () => {
  const w = libraryWorld();
  const s = seed(w);
  seedHits(w, s);
  w.l.settings.set("alice", w.addMediaConnection("alice", "embeddinggemma-2", "EG2"));
  const app = await signIn(w);
  await openFileSearch(app);
  await searchFiles(app, "anything");
  await app.waitFor(`id:library-hit-thumb:${s.acmeByBob.id}:image:0`);
  await app.press("text:Play from 0:12"); // demo.mp4 in Side
  await app.waitFor("id:library-viewer-player");

  // Alice is removed from Side (the open preview's) and from Acme (the image hit's).
  w.f.state.spaces.get("side")!.members.delete("alice");
  w.f.state.spaces.get("acme")!.members.delete("alice");
  await app.waitUntil(() => !app.shows("id:library-viewer"), { timeout: 15_000 });
  await app.waitFor("id:library-results");
  expect(text(app)).not.toContain("demo.mp4");
  expect(app.shows(`id:library-hit:${s.acmeByBob.id}:image:0`)).toBe(false);
  expect(app.shows(`id:library-hit-thumb:${s.acmeByBob.id}:image:0`)).toBe(false);

  // A slow read that answers after sign-out shows nothing to the next user.
  // The driver's press() waits for in-flight requests, so a parked read would
  // block it. The read is therefore held on a gate and the sequence is driven
  // by conditions, never by elapsed time: once the server has the read parked
  // (so "Opening…" is up), sign out; once the app shows the signed-out screen,
  // release the answer.
  const release = w.holdContent();
  const opening = app.press("text:Open at section");
  await poll(() => w.l.contentParked === 1);
  const sawLoading = app.shows("id:library-viewer-loading");
  // Sign out is in the account menu: open it, then choose Sign out, each
  // press dispatched without waiting on the parked read.
  const menuOpening = app.press("id:account-menu");
  await poll(() => app.shows("id:disconnect"));
  const signingOut = app.press("id:disconnect");
  await poll(() => app.shows("id:connect"));
  expect(w.l.contentParked).toBe(1); // sign-out didn't wait for the read
  expect(w.l.contentReleased).toBe(0);
  release();
  await Promise.all([opening, menuOpening, signingOut]);
  expect(sawLoading).toBe(true);
  await app.type("id:access-token", TOKENS.bob!);
  await app.press("id:connect");
  await app.waitFor("id:top-search");
  expect(app.value("id:top-search")).toBe(""); // nothing of Alice's query
  await openSearch(app);
  // Bob starts on Conversations with nothing of Alice's.
  expect(app.shows("id:library-search")).toBe(false);
  await app.press("id:search-surface:library");
  await app.waitFor("id:library-search");
  expect(app.shows("id:library-viewer")).toBe(false);
  expect(app.shows("id:library-results")).toBe(false);
  await app.waitUntil(() => w.l.contentParked === 0 && w.l.contentReleased === 1);
  await app.wait(300);
  expect(app.shows("id:library-viewer")).toBe(false);
  expect(app.shows("id:library-viewer-loading")).toBe(false);
  expect(text(app)).not.toContain("The rollback plan");
  expect(text(app)).not.toContain("notes.md");
});

test("zero-similarity matches fold under Less relevant; a folded, collapsed or left-behind image is never read for a thumbnail", async () => {
  const w = libraryWorld();
  const s = seed(w);
  const faint = w.addAsset("alice", null, "whiteboard.png", "PNGFAINT");
  w.l.hits.push(
    { asset_id: s.acmeByBob.id, locator: { kind: "image" }, score: 0.71, score_kind: "cosine" },
    { asset_id: faint.id, locator: { kind: "image" }, score: 0, score_kind: "cosine" },
    // A System One score of 0 keeps its provenance: never folded as "no similarity".
    { asset_id: s.mine.id, locator: { kind: "text", start: null, end: null }, excerpt: "Intro paragraph.", score: 0, score_kind: "classifier" },
    { asset_id: s.carols.id, locator: { kind: "page", page: 1 }, excerpt: "carol's secret", score: 0.9, score_kind: "classifier" },
  );
  w.l.settings.set("alice", w.addMediaConnection("alice", "embeddinggemma-2", "EG2"));
  const app = await signIn(w);
  await openFileSearch(app);
  await searchFiles(app, "whiteboard");
  const faintKey = `${faint.id}:image:0`;
  const reads = (id: string) => lib(w, `/v1/library/assets/${id}/content`).length;

  // Readable and shown counts are separate; the folded one isn't called lost.
  expect(text(app)).toContain("3 matches for “whiteboard”");
  expect(text(app)).toContain("Less relevant (1)");
  expect(text(app)).not.toContain("no longer in"); // carol's file is not mine to read: no lost-membership notice either
  expect(app.shows(`id:library-hit:${faintKey}`)).toBe(false);
  expect(app.shows(`id:library-hit:${s.mine.id}:text:0`)).toBe(true); // classifier 0 stays in the list
  await app.waitUntil(() => reads(s.acmeByBob.id) === 1);
  await app.wait(200);
  expect(reads(faint.id)).toBe(0); // folded: no bytes requested

  // Open the fold: now it's drawn, and only now read, with auth.
  await app.press("id:library-less-relevant:toggle");
  await app.waitFor(`id:library-hit:${faintKey}`);
  await app.waitUntil(() => reads(faint.id) === 1);
  expect(lib(w, `/v1/library/assets/${faint.id}/content`)[0]!.headers["authorization"]).toBe(`Bearer ${TOKENS.alice}`);
  await app.waitFor(`id:library-hit-thumb:${faintKey}`);

  // Collapse: the thumbnail goes; opening again reads again (nothing was kept).
  await app.press("id:library-less-relevant:toggle");
  await app.waitUntil(() => !app.shows(`id:library-hit:${faintKey}`));
  await app.press("id:library-less-relevant:toggle");
  await app.waitUntil(() => reads(faint.id) === 2);

  // Switching to Conversations drops every thumbnail; coming back reads again.
  const before = reads(s.acmeByBob.id);
  await app.press("id:search-surface:conversations");
  await app.wait(200);
  expect(reads(s.acmeByBob.id)).toBe(before);
  await app.press("id:search-surface:library");
  await app.waitUntil(() => reads(s.acmeByBob.id) === before + 1);

  // Leaving Search altogether drops them too, and reads nothing while away.
  await app.press("id:nav-chat");
  const away = contentReads(w).length;
  await app.wait(200);
  expect(contentReads(w).length).toBe(away);

  // A new search closes the fold again.
  await openFileSearch(app);
  await searchFiles(app, "whiteboard again");
  expect(app.shows(`id:library-hit:${faintKey}`)).toBe(false);
  expect(text(app)).toContain("Less relevant (1)");
});
