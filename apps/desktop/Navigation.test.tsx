// Cross-lane navigation on the real app entry: the sidebar's Library, kept
// apart from the media model choice. Headless, against the shared fake server
// plus the library routes (fake tokens only, no network).

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
