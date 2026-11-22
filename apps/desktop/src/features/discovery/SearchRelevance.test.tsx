// Message search folds results with no measurable similarity under one
// "Less relevant" disclosure, and only in an embeddings-ordered answer, so a
// System One score is never relabelled. Same harness and fake server as
// Discovery.test.tsx; no network.

import { expect, open, test } from "nsh/test";
import type { App } from "nsh/test";
import { BASE, TOKENS } from "../../../test/fakeServer";
import { discoveryWorld } from "./test/fixtures";

type W = ReturnType<typeof discoveryWorld>;

async function signIn(w: W): Promise<App> {
  const app = await open("./test/Harness.tsx", { network: w.network as any, platform: "linux", width: 1280, height: 900 });
  await app.clear("id:server-url");
  await app.type("id:server-url", BASE);
  await app.type("id:access-token", TOKENS.alice!);
  await app.press("id:connect");
  await app.waitFor("id:h-search");
  await app.press("id:h-search");
  await app.waitFor("id:search-view");
  return app;
}

async function search(app: App, query: string) {
  await app.clear("id:top-search");
  await app.type("id:top-search", query);
  await app.submit("id:top-search");
  await app.waitFor("id:search-results");
}

const text = (app: App) => app.texts().join(" | ");

function seed(w: W) {
  w.d.corpus.push(
    { owner: "alice", space_id: null, room_id: null, event_id: 7, actor: "alice", text: "deploy checklist lives in the wiki", score: 0.8 },
    { space_id: "acme", room_id: "lobby", event_id: 101, actor: "bob", text: "lunch order for friday", score: 0 },
    { space_id: "acme", room_id: "lobby", event_id: 102, actor: "bob", text: "weather was fine", score: 0 },
  );
}

test("zero-similarity messages fold under Less relevant, closed, with separate counts", async () => {
  const w = discoveryWorld();
  seed(w);
  w.d.ranking = "embeddings";
  const app = await signIn(w);
  await search(app, "deploy");
  expect(text(app)).toContain("3 results for “deploy”"); // everything readable is counted
  expect(text(app)).toContain("Less relevant (2)");
  expect(app.shows("id:search-result:chat:chat:7")).toBe(true);
  expect(app.shows("id:search-result:acme:lobby:101")).toBe(false);
  expect(app.shows("id:search-hidden")).toBe(false); // folded is not "no longer a member"
  expect(text(app)).not.toContain("similarity 0");

  await app.press("id:search-less-relevant:toggle");
  await app.waitFor("id:search-result:acme:lobby:101");
  expect(app.shows("id:search-result:acme:lobby:102")).toBe(true);
  await app.press("id:search-less-relevant:toggle");
  await app.waitUntil(() => !app.shows("id:search-result:acme:lobby:101"));

  // A new search starts folded again.
  await app.press("id:search-less-relevant:toggle");
  await app.waitFor("id:search-result:acme:lobby:101");
  await search(app, "deploy again");
  expect(app.shows("id:search-result:acme:lobby:101")).toBe(false);
});

test("a System One-ordered answer folds nothing: a score of 0 there is System One's, not missing similarity", async () => {
  const w = discoveryWorld();
  seed(w);
  w.d.ranking = "classifier";
  const app = await signIn(w);
  await search(app, "deploy");
  expect(app.shows("id:search-less-relevant")).toBe(false);
  expect(app.shows("id:search-result:acme:lobby:101")).toBe(true);
  expect(text(app)).toContain("Reranked by System One");
});
