// Prints the `nsh test App.tsx` steps of the Search and Scheduled tasks
// capture on stdout (see capture-discovery.sh). The token is read from
// TOKEN_FILE (mode 600) and JSON-quoted into a `type` step; every screenshot
// is taken after it has gone from the screen.
//
//   URL=… SHOTS=/shots TOKEN_FILE=… node discovery-steps.mjs alice
import { readFileSync } from "node:fs";

const q = JSON.stringify;
const { URL, SHOTS = "/shots", TOKEN_FILE } = process.env;
if (!URL || !TOKEN_FILE) throw new Error("URL and TOKEN_FILE are required");
const token = readFileSync(TOKEN_FILE, "utf8").trim();
const shot = (name) => `await app.screenshot(${q(`${SHOTS}/${name}`)})`;
const T = { timeout: 30000 };
const wait = (sel) => `await app.waitFor(${q(sel)}, ${q(T)})`;

export const QUERY = "deploy checklist smoke tests";

const steps = [
  `await app.setViewport(1200, 1000)`,
  `await app.clear("id:server-url")`,
  `await app.type("id:server-url", ${q(URL)})`,
  `await app.type("id:access-token", ${q(token)})`,
  `await app.press("id:connect")`,
  wait("id:workspace"),

  // Search › Conversations: Return in the search box at the top sends the query.
  `await app.clear("id:top-search")`,
  `await app.type("id:top-search", ${q(QUERY)})`,
  `await app.submit("id:top-search")`,
  wait("id:search-ranking"),
  `await app.wait(800)`,
  shot("semantic-search.png"),

  // Scheduled tasks, once the one-time task has fired.
  `await app.clear("id:top-search")`,
  `await app.press("id:nav-schedules")`,
  wait("id:schedule-list"),
  wait("text:/No more firings/"),
  `await app.wait(800)`,
  shot("scheduled-tasks.png"),
];

if (process.argv[2] !== "alice") throw new Error("usage: discovery-steps.mjs alice");
process.stdout.write(steps.join("\n") + "\n");
