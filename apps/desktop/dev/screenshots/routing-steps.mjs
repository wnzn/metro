// Prints the `nsh test App.tsx` steps of the Model routing and sharing
// capture on stdout (see capture-routing.sh). The token is read from
// TOKEN_FILE (mode 600) and JSON-quoted into a `type` step; every screenshot
// is taken after it has gone from the screen.
//
//   URL=… SPACE=<space id> SHARED=<connection id> SHOTS=/shots TOKEN_FILE=… node routing-steps.mjs alice
import { readFileSync } from "node:fs";

const q = JSON.stringify;
const { URL, SPACE, SHARED, SHOTS = "/shots", TOKEN_FILE } = process.env;
if (!URL || !SPACE || !SHARED || !TOKEN_FILE) throw new Error("URL, SPACE, SHARED and TOKEN_FILE are required");
const token = readFileSync(TOKEN_FILE, "utf8").trim();
const shot = (name) => `await app.screenshot(${q(`${SHOTS}/${name}`)})`;

export const ASK = "Please analyze the causal chain for the incident review.";

const steps = [
  `await app.setViewport(1200, 1000)`,
  `await app.clear("id:server-url")`,
  `await app.type("id:server-url", ${q(URL)})`,
  `await app.type("id:access-token", ${q(token)})`,
  `await app.press("id:connect")`,
  `await app.waitFor("id:workspace", { timeout: 15000 })`,

  // Model routing: the use cases, each with its answering chat model.
  `await app.press("id:nav-settings")`,
  `await app.press("id:nav-settings:routing")`,
  `await app.waitFor("id:case-label:1", { timeout: 15000 })`,
  `await app.waitFor("text:/Bob's GPU box · fixture\\/shared-local/", { timeout: 15000 })`,
  `await app.wait(300)`,
  shot("model-routing.png"),
  // Where text goes, from what is chosen.
  `await app.scroll("id:routing-settings", 1400)`,
  `await app.waitFor("id:routing-dataflow")`,
  `await app.wait(300)`,
  shot("model-routing-dataflow.png"),

  // Sharing: a local model shared with one Space, and what that means.
  `await app.press("id:nav-settings:providers")`,
  `await app.waitFor(${q(`id:connection-sharing:${SHARED}`)}, { timeout: 15000 })`,
  `await app.press(${q(`id:connection-sharing:${SHARED}`)})`,
  `await app.waitFor(${q(`id:sharing-warning:${SHARED}`)}, { timeout: 15000 })`,
  `await app.wait(300)`,
  shot("model-sharing.png"),
  `await app.press("id:settings-close")`,

  // A real turn: an unaddressed message, judged by System One under Alice's
  // opt-in, routed to a use case answered by Bob's shared model.
  `await app.press(${q(`id:space:${SPACE}`)})`,
  `await app.waitFor("id:space-view", { timeout: 15000 })`,
  `await app.type("id:composer", ${q(ASK)})`,
  `await app.press("id:send")`,
  `await app.waitFor("text:/reply from shared-local/", { timeout: 30000 })`,
  `await app.waitFor("text:/detected by System One/", { timeout: 15000 })`,
  `await app.waitFor("text:Incident review", { timeout: 15000 })`,
  `await app.press("text:/’s activity$/")`,
  `await app.waitFor("text:/answered: System One matched the use case/", { timeout: 15000 })`,
  `await app.wait(300)`,
  shot("route-attribution.png"),
];

if (process.argv[2] !== "alice") throw new Error("usage: routing-steps.mjs alice");
process.stdout.write(steps.join("\n") + "\n");
