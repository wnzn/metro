// Cross-lane navigation on the real app entry: the sidebar's Library, and the
// media model chosen in Settings › Model providers, separate from the
// Library itself. Headless, against the shared fake server plus the library
// routes (fake tokens only, no network).

import { expect, open, test } from "nsh/test";
import type { App } from "nsh/test";
import { BASE, TOKENS } from "./test/fakeServer";
import { libraryWorld } from "./src/features/library/test/fixtures";

type W = ReturnType<typeof libraryWorld>;

async function signIn(w: W, user: "alice" | "bob" = "alice"): Promise<App> {
  const app = await open("./App.tsx", { network: w.network as any, platform: "linux", width: 1280, height: 900 });
  await app.clear("id:server-url");
  await app.type("id:server-url", BASE);
  await app.type("id:access-token", TOKENS[user]!);
  await app.press("id:connect");
  await app.waitFor("id:nav-library");
  return app;
}

const text = (app: App) => app.texts().join(" | ");
const lib = (w: W, path: string, method = "GET") => w.l.log.filter((r) => r.path === path && r.method === method);
/** Anything that would send a file or a query to a model. */
const modelWork = (w: W) => lib(w, "/v1/library/index", "POST").length + lib(w, "/v1/library/search", "POST").length;

test("Library opens from the sidebar and lists only what I can access, without indexing or searching", async () => {
  const w = libraryWorld();
  const mine = w.addAsset("alice", null, "notes.md", "# Notes\n\nThe rollback plan.");
  const acme = w.addAsset("bob", "acme", "roadmap.png", "PNGDATA");
  w.addAsset("bob", null, "bob-private.txt", "bob's private notes");
  const app = await signIn(w);
  await app.press("id:nav-library");
  await app.waitFor(`id:library-file:${mine.id}`);
  expect(app.shows(`id:library-file:${acme.id}`)).toBe(true);
  expect(text(app)).not.toContain("bob-private.txt");
  // The Library is files; the media model is chosen in Settings, not here.
  expect(app.shows("id:library-model-picker")).toBe(false);
  expect(modelWork(w)).toBe(0);
});

test("the media model is chosen in Settings › Model providers: its data flow is disclosed, and saving indexes nothing", async () => {
  const w = libraryWorld();
  const mine = w.addAsset("alice", null, "notes.md", "# Notes\n\nThe rollback plan.");
  w.f.addConnection("alice", "custom-embeddings", "Text embeddings", "nomic-embed");
  const own = w.addMediaConnection("alice", "device-embeddinggemma-2", "My EG2");
  w.addMediaConnection("bob", "embeddinggemma-2", "Bob's EG2 server", { visibility: "spaces", space_ids: ["acme"] });
  const app = await signIn(w);
  await app.press("id:nav-library");
  await app.waitFor(`id:library-file:${mine.id}`);

  await app.press("id:nav-settings");
  await app.press("id:nav-settings:providers");
  await app.waitFor("id:library-model");
  // The page also lists saved connections: count what the picker adds.
  const textOnly = app.count("text:/Text embeddings/");
  await app.press("id:library-model-picker");
  await app.waitFor("text:/Bob's EG2 server/");
  // A text-only embeddings connection never stands in for media embedding.
  expect(app.count("text:/Text embeddings/")).toBe(textOnly);
  await app.press("text:/Bob's EG2 server/");
  await app.waitFor("id:library-model-dataflow");
  expect(text(app)).toContain("Shared by bob: it runs on bob's endpoint, so whoever runs that endpoint can see the files and queries sent, and bob may pay for it.");
  expect(lib(w, "/v1/library/settings", "PUT").length).toBe(0); // nothing saved until Save

  await app.press("id:library-model-picker");
  await app.press("text:/My EG2/");
  await app.press("id:library-model-save");
  await app.waitUntil(() => lib(w, "/v1/library/settings", "PUT").length === 1);
  expect(lib(w, "/v1/library/settings", "PUT")[0]!.body).toBe(JSON.stringify({ embedding_id: own }));
  expect(modelWork(w)).toBe(0);

  // Back to the same Library page; still nothing sent to the model until Index.
  await app.press("id:settings-close");
  await app.waitFor("id:settings-dialog", { state: "gone" });
  await app.waitFor(`id:library-file:${mine.id}`);
  expect(modelWork(w)).toBe(0);
});
