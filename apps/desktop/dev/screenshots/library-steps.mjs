// Prints the `nsh test App.tsx` steps of the Documents & media capture on
// stdout (see capture-library.sh). The token is read from TOKEN_FILE (mode
// 600) and JSON-quoted into a `type` step; every screenshot is taken after
// it has gone from the screen.
//
//   URL=… IDS=<json name->asset id> MEDIA=<connection id> SHOTS=/shots TOKEN_FILE=… node library-steps.mjs alice|alice-open|bob
import { readFileSync } from "node:fs";

const q = JSON.stringify;
const { URL, IDS, MEDIA, SHOTS = "/shots", TOKEN_FILE } = process.env;
if (!URL || !IDS || !TOKEN_FILE) throw new Error("URL, IDS and TOKEN_FILE are required");
const ids = JSON.parse(IDS);
const token = readFileSync(TOKEN_FILE, "utf8").trim();
const shot = (name) => `await app.screenshot(${q(`${SHOTS}/${name}`)})`;
const T = { timeout: 30000 };
const wait = (sel, t = T) => `await app.waitFor(${q(sel)}, ${q(t)})`;

const signIn = [
  `await app.setViewport(1200, 1040)`,
  `await app.clear("id:server-url")`,
  `await app.type("id:server-url", ${q(URL)})`,
  `await app.type("id:access-token", ${q(token)})`,
  `await app.press("id:connect")`,
  wait("id:workspace"),
  `await app.submit("id:top-search")`,
  wait("id:search-view"),
  `await app.press("id:search-surface:library")`,
  wait("id:library-view"),
];

const search = (query) => [
  `await app.clear("id:top-search")`,
  `await app.type("id:top-search", ${q(query)})`,
  `await app.press("id:library-search-submit")`,
  wait("id:library-results"),
];

function alice() {
  if (!MEDIA) throw new Error("MEDIA is required for alice");
  return [
    ...signIn,
    wait(`id:library-file:${ids["shipping-contract.pdf"]}`),
    // Choose the media model in the app, then index each file on request.
    `await app.press("id:library-model-picker")`,
    `await app.press("text:/Capture EG2 fixture/")`,
    wait("id:library-model-dataflow"),
    `await app.press("id:library-model-save")`,
    wait("text:Saved: Capture EG2 fixture."),
    ...Object.values(ids).flatMap((id) => [
      `await app.press(${q(`id:library-index:${id}`)})`,
      wait(`text:/^Indexed \\d+ parts? in your own index/`),
      wait(`id:library-index-state:${id}`),
    ]),
    `await app.waitUntil(() => app.texts().filter((t) => t.startsWith("Indexed ")).length >= ${Object.keys(ids).length}, ${q(T)})`,
    `await app.scroll("id:library-view", 120)`,
    `await app.wait(400)`,
    shot("library-assets.png"),

    // A page of a PDF, found by its own text.
    ...search("which page has the cargo manifest?"),
    `await app.scroll("id:library-view", 1000)`,
    `await app.wait(400)`,
    shot("library-search.png"),
    `await app.press("text:Open at page 2")`,
    wait("id:library-viewer-document"),
    `await app.wait(300)`,
    shot("library-open-page.png"),
  ];
}

/** A second session (stdin steps get 30 s each): the model is saved and the files indexed. */
function aliceOpen() {
  return [
    ...signIn,
    wait("text:Saved: Capture EG2 fixture."),
    // A time range of a recording, found by what it sounds like.
    ...search("the thunder in the standup recording"),
    `await app.press("text:/^Play from 0:05$/ >> nth=0")`,
    wait("id:library-viewer-player"),
    `await app.wait(1500)`,
    `await app.press("id:library-play-match")`,
    wait("id:library-player-position"),
    `await app.wait(700)`,
    shot("library-open-time.png"),
    `await app.press("id:library-viewer-back")`,
    wait("id:library-results"),

    // An image, found by its pixels.
    ...search("a crimson whiteboard photo"),
    `await app.press("text:Open image >> nth=0")`,
    wait("id:library-viewer-image"),
    `await app.wait(800)`,
    shot("library-open-image.png"),
  ];
}

function bob() {
  const notes = ids["harbor-notes.txt"];
  return [
    ...signIn,
    wait(`id:library-file:${notes}`),
    wait(`id:library-file:${ids["bob-todo.txt"]}`),
    `await app.press(${q(`id:library-open:${notes}`)})`,
    wait("id:library-viewer-text"),
    `await app.wait(300)`,
    // capture-library.sh removes Bob from the Space once this file exists.
    shot("library-revocation-before.png"),
    `await app.waitUntil(() => !app.shows("id:library-viewer"), { timeout: 45000 })`,
    wait("id:library-view"),
    `await app.waitUntil(() => !app.shows(${q(`id:library-file:${notes}`)}), { timeout: 15000 })`,
    `await app.wait(300)`,
    shot("library-revocation-after.png"),
  ];
}

const runs = { alice, "alice-open": aliceOpen, bob };
const who = process.argv[2];
if (!(who in runs)) throw new Error("usage: library-steps.mjs alice|alice-open|bob");
process.stdout.write(runs[who]().join("\n") + "\n");
