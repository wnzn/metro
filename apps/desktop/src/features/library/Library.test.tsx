// Search › Documents & media against the shared fake server plus the
// library routes (test/fixtures.ts). Real NSH engine and the real app entry,
// no network. Tokens are the fake server's own, never real ones.

import { expect, open, test } from "nsh/test";
import type { App } from "nsh/test";
import { BASE, TOKENS } from "../../../test/fakeServer";
import { libraryWorld } from "./test/fixtures";

type W = ReturnType<typeof libraryWorld>;

async function signIn(w: W, user: "alice" | "bob" | "carol" = "alice"): Promise<App> {
  const app = await open("../../../App.tsx", { network: w.network as any, platform: "linux", width: 1280, height: 900 });
  await app.clear("id:server-url");
  await app.type("id:server-url", BASE);
  await app.type("id:access-token", TOKENS[user]!);
  await app.press("id:connect");
  await app.waitFor("id:top-search");
  return app;
}

/** Return in the empty search box at the top opens Search without searching. */
async function openSearch(app: App) {
  await app.submit("id:top-search");
  await app.waitFor("id:search-view");
}

async function openLibrary(app: App) {
  await openSearch(app);
  await app.press("id:search-surface:library");
  await app.waitFor("id:library-view");
}

const text = (app: App) => app.texts().join(" | ");
const enc = new TextEncoder();
const lib = (w: W, path: string, method = "GET") => w.l.log.filter((r) => r.path === path && r.method === method);

function seed(w: W) {
  const mine = w.addAsset("alice", null, "notes.md", "# Notes\n\nIntro paragraph.\n\nThe rollback plan: revert the tag, rebuild, then run the smoke test.\n\nOutro.");
  const acmeByBob = w.addAsset("bob", "acme", "roadmap.png", "PNGDATA");
  const sideByBob = w.addAsset("bob", "side", "demo.mp4", "MP4DATA");
  const bobsOwn = w.addAsset("bob", null, "bob-private.txt", "bob's private notes");
  w.f.addSpace("secret", "Secret", "carol");
  const carols = w.addAsset("carol", "secret", "carol-secret.pdf", "%PDF-1.7 secret");
  return { mine, acmeByBob, sideByBob, bobsOwn, carols };
}

test("message search stays the default; Documents & media is its own surface listing only what I can access", async () => {
  const w = libraryWorld();
  const s = seed(w);
  const app = await signIn(w);
  await openSearch(app);
  expect(lib(w, "/v1/library/assets").length).toBe(0); // opening Search lists nothing yet
  // Conversations first, as before.
  expect(app.shows("id:search-query-echo")).toBe(true);
  expect(app.shows("id:library-view")).toBe(false);
  await app.press("id:search-surface:library");
  await app.waitFor(`id:library-file:${s.mine.id}`);
  expect(app.shows("id:search-query-echo")).toBe(false);
  expect(app.shows("id:top-search")).toBe(true); // the one query field, now for files
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
  // Only listing happened: nothing was indexed or searched.
  expect(lib(w, "/v1/library/index", "POST").length).toBe(0);

  await app.press("id:library-list-scope");
  await app.waitFor("text:Side project");
  expect(app.shows("text:Secret")).toBe(false);
  await app.press("text:Acme >> nth=2");
  await app.waitFor(`id:library-file:${s.acmeByBob.id}`);
  await app.waitUntil(() => !app.shows(`id:library-file:${s.mine.id}`));
  expect(lib(w, "/v1/library/assets").at(-1)!.url).toBe(`${BASE}/v1/library/assets?scope=space&space_id=acme`);

  // Back to Conversations: message search is still there and unchanged.
  await app.press("id:search-surface:conversations");
  await app.waitFor("id:search-query-echo");
  expect(text(app)).toContain("Everything you can read");
});

test("uploads come from the system picker as raw bytes, to the chosen Space, within the server's limit; nothing goes to a model", async () => {
  const w = libraryWorld();
  w.l.maxFileBytes = 1024;
  const app = await signIn(w);
  await openLibrary(app);
  await app.waitFor("id:library-files-empty");
  await app.waitFor("text:/up to 1.0 KB each/");
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
  await app.press("id:library-upload-target");
  await app.press("text:Acme >> nth=1");
  await app.waitFor("text:/Every current member of Acme will see these files/");
  await app.press("id:library-choose");
  await app.waitFor("id:library-upload-outcomes");
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

test("only media-capable connections can be chosen; the data flow is disclosed; Index sends one file only on request", async () => {
  const w = libraryWorld();
  const s = seed(w);
  w.f.addConnection("alice", "custom-embeddings", "Text embeddings", "nomic-embed");
  const own = w.addMediaConnection("alice", "device-embeddinggemma-2", "My EG2");
  w.addMediaConnection("bob", "embeddinggemma-2", "Bob's EG2 server", { visibility: "spaces", space_ids: ["acme"] });
  const app = await signIn(w);
  await openLibrary(app);
  await app.waitFor(`id:library-file:${s.mine.id}`);
  expect(app.enabled(`id:library-index:${s.mine.id}`)).toBe(false);
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

  await app.press("id:library-model-picker");
  await app.press("text:/My EG2/");
  await app.waitFor("text:/It runs in your own metro-connector on your device/");
  await app.press("id:library-model-save");
  await app.waitFor("text:Saved: My EG2.");
  expect(lib(w, "/v1/library/settings", "PUT")[0]!.body).toBe(JSON.stringify({ embedding_id: own }));
  expect(lib(w, "/v1/library/index", "POST").length).toBe(0);

  await app.press(`id:library-index:${s.mine.id}`);
  await app.waitFor(`id:library-index-state:${s.mine.id}`);
  await app.waitFor("text:Indexed 3 parts in your own index.");
  const index = lib(w, "/v1/library/index", "POST");
  expect(index.length).toBe(1);
  expect(JSON.parse(index[0]!.body!)).toEqual({ asset_id: s.mine.id });
});

test("delete asks first, and is offered only to the uploader or the Space owner", async () => {
  const w = libraryWorld();
  const s = seed(w);
  const app = await signIn(w);
  await openLibrary(app);
  await app.waitFor(`id:library-file:${s.mine.id}`);
  // Alice owns Acme (Bob's file there), but is only a member of Side.
  expect(app.shows(`id:library-delete:${s.mine.id}`)).toBe(true);
  expect(app.shows(`id:library-delete:${s.acmeByBob.id}`)).toBe(true);
  expect(app.shows(`id:library-delete:${s.sideByBob.id}`)).toBe(false);

  await app.press(`id:library-delete:${s.acmeByBob.id}`);
  await app.waitFor(`id:library-confirm-delete:${s.acmeByBob.id}`);
  expect(text(app)).toContain("It's removed for every member of Acme, with its index.");
  await app.press(`id:library-delete-cancel:${s.acmeByBob.id}`);
  expect(app.shows(`id:library-confirm-delete:${s.acmeByBob.id}`)).toBe(false);
  expect(lib(w, `/v1/library/assets/${s.acmeByBob.id}`, "DELETE").length).toBe(0);

  await app.press(`id:library-delete:${s.acmeByBob.id}`);
  await app.press(`id:library-delete-confirm:${s.acmeByBob.id}`);
  await app.waitFor("text:Deleted roadmap.png and its index.");
  expect(app.shows(`id:library-file:${s.acmeByBob.id}`)).toBe(false);
  expect(lib(w, `/v1/library/assets/${s.acmeByBob.id}`, "DELETE").length).toBe(1);
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
  await app.press("id:library-search-submit");
  await app.waitFor("id:library-results");
}

test("search finds pages, sections, images and times; labels never claim a rerank that didn't happen; open fetches with auth and jumps to the match", async () => {
  const w = libraryWorld();
  const s = seed(w);
  const { pdf } = seedHits(w, s);
  w.l.settings.set("alice", w.addMediaConnection("alice", "embeddinggemma-2", "EG2"));
  const app = await signIn(w);
  await openLibrary(app);
  await searchFiles(app, "rollback plan");
  expect(JSON.parse(lib(w, "/v1/library/search", "POST")[0]!.body!)).toEqual({ query: "rollback plan", scope: "all", limit: 10 });
  expect(text(app)).toContain("5 matches for “rollback plan” · Everything you can access");
  expect(text(app)).not.toContain("carol's secret");
  expect(text(app)).toContain("Documents reranked by System One");
  expect(text(app)).toContain("System One relevance 0.93");
  expect(text(app)).toContain("similarity 0.71");
  expect(text(app)).toContain("similarity 0.66");
  expect(text(app)).not.toContain("System One relevance 0.66");
  expect(text(app)).toContain("Page 3");
  expect(text(app)).toContain("0:12–0:23");
  expect(text(app)).toContain("Play from 0:12");
  expect(text(app)).toContain("No text excerpt: matched by its image content.");

  // A text section, marked.
  await app.press("text:Open at section");
  await app.waitFor("id:library-viewer-text");
  await app.waitFor("text:/The rollback plan/");
  expect(text(app)).toContain("The matching section is marked");
  const reads = lib(w, `/v1/library/assets/${s.mine.id}/content`);
  expect(reads.length).toBe(1);
  expect(reads[0]!.headers["authorization"]).toBe(`Bearer ${TOKENS.alice}`);
  expect(reads[0]!.url).toBe(`${BASE}/v1/library/assets/${s.mine.id}/content`);
  await app.press("id:library-viewer-back");
  await app.waitFor("id:library-results");

  // A video at its time.
  await app.press("text:Play from 0:12");
  await app.waitFor("id:library-viewer-player");
  expect(text(app)).toContain("The player starts at the match (0:12–0:23).");
  expect(app.shows("id:library-play-match")).toBe(true);
  await app.press("id:library-viewer-back");

  // A PDF page: the indexed text of that page, and an honest way to open the original.
  await app.press("text:Open at page 3");
  await app.waitFor("id:library-viewer-document");
  expect(text(app)).toContain("Indexed text of page 3");
  expect(text(app)).toContain("then go to page 3");
  expect(lib(w, `/v1/library/assets/${pdf.id}/content`).length).toBe(1);
  expect(w.l.log.some((r) => r.url.includes(TOKENS.alice!))).toBe(false);
  // Opening never indexes or sends a file anywhere.
  expect(lib(w, "/v1/library/index", "POST").length).toBe(0);
});

test("similarity-only answers say so; without a media model the search explains what's missing", async () => {
  const w = libraryWorld();
  const s = seed(w);
  seedHits(w, s);
  w.l.ranking = "embeddings";
  const app = await signIn(w);
  await openLibrary(app);
  await app.type("id:top-search", "x");
  await app.press("id:library-search-submit");
  await app.waitFor("id:library-search-error");
  expect(text(app)).toContain("File search needs a media model: choose a media embedding model in Settings to search your files.");
  w.l.settings.set("alice", w.addMediaConnection("alice", "embeddinggemma-2", "EG2"));
  await searchFiles(app, "rollback");
  expect(text(app)).toContain("Similarity order");
  expect(text(app)).toContain("System One didn't rerank these.");
  expect(text(app)).not.toContain("reranked by System One");
});

test("losing a Space drops its files, results and an open preview; signing out discards a late file read", async () => {
  const w = libraryWorld();
  const s = seed(w);
  seedHits(w, s);
  w.l.settings.set("alice", w.addMediaConnection("alice", "embeddinggemma-2", "EG2"));
  const app = await signIn(w);
  await openLibrary(app);
  await searchFiles(app, "anything");
  await app.press("text:Play from 0:12"); // demo.mp4 in Side
  await app.waitFor("id:library-viewer-player");

  // Alice is removed from Side while the preview is open.
  w.f.state.spaces.get("side")!.members.delete("alice");
  await app.waitUntil(() => !app.shows("id:library-viewer"), { timeout: 15_000 });
  await app.waitFor("id:library-results");
  expect(text(app)).not.toContain("demo.mp4");
  expect(app.shows(`id:library-file:${s.sideByBob.id}`)).toBe(false);

  // A slow read that answers after sign-out shows nothing to the next user.
  w.l.contentDelay = 600;
  await app.press("text:Open at section");
  await app.waitFor("id:library-viewer-loading");
  await app.press("id:disconnect");
  await app.waitFor("id:connect");
  await app.type("id:access-token", TOKENS.bob!);
  await app.press("id:connect");
  await app.waitFor("id:top-search");
  expect(app.value("id:top-search")).toBe(""); // nothing of Alice's query
  await openSearch(app);
  // Bob starts on Conversations with nothing of Alice's.
  expect(app.shows("id:library-view")).toBe(false);
  await app.press("id:search-surface:library");
  await app.waitFor("id:library-view");
  await app.wait(800);
  expect(app.shows("id:library-viewer")).toBe(false);
  expect(text(app)).not.toContain("The rollback plan");
  expect(text(app)).not.toContain("notes.md");
});
