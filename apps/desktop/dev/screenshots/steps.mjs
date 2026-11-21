// Prints the `nsh test App.tsx` steps of one capture scenario on stdout.
// Inputs come from the environment; a token is read from TOKEN_FILE (mode 600)
// and JSON-quoted into a `type` step, so it never appears in argv, logs or a
// screenshot (screenshots are taken before it is typed or after it is gone).
//
//   URL=… SPACE=<space id> SHOTS=/shots TOKEN_FILE=… [EVENT_ID=…] [AGENT=…] node steps.mjs <bob|alice|alice-chat|alice-agent|invalid>
import { readFileSync } from "node:fs";

const q = JSON.stringify;
const { URL, SPACE, SHOTS = "/shots", TOKEN_FILE, EVENT_ID, AGENT, THREAD_ID } = process.env;
if (!URL) throw new Error("URL is required");
const token = () => {
  if (!TOKEN_FILE) throw new Error("TOKEN_FILE is required");
  return readFileSync(TOKEN_FILE, "utf8").trim();
};

export const BOB_SAYS = "@alice the runner is paired with Acme now. Can you run the Demo?";
export const ALICE_SAYS = "Thanks Bob! Running the deterministic Demo.";

const open = [`await app.setViewport(1200, 780)`, `await app.clear("id:server-url")`, `await app.type("id:server-url", ${q(URL)})`];
const connect = () => [
  `await app.type("id:access-token", ${q(token())})`,
  `await app.press("id:connect")`,
  `await app.waitFor("id:workspace", { timeout: 15000 })`,
];
const enterSpace = () => [`await app.waitFor(${q(`id:space:${SPACE}`)}, { timeout: 15000 })`, `await app.press(${q(`id:space:${SPACE}`)})`, `await app.waitFor("id:space-view", { timeout: 15000 })`];
const shot = (name) => `await app.screenshot(${q(`${SHOTS}/${name}`)})`;

const scenarios = {
  // Bob mentions Alice with the picker (a person: no agent work starts).
  bob: () => [
    ...open,
    ...connect(),
    ...enterSpace(),
    `await app.type("id:composer", "@")`,
    `await app.waitFor("id:pick:alice", { timeout: 10000 })`,
    `await app.press("id:pick:alice")`,
    `await app.type("id:composer", ${q(BOB_SAYS.replace(/^@alice /, ""))})`,
    `await app.press("id:send")`,
    `await app.waitFor("text:/the runner is paired with Acme/", { timeout: 10000 })`,
  ],
  // Alice: the connect screen before any secret, then the populated room and the owner's view.
  alice: () => [
    ...open,
    `await app.wait(300)`,
    shot("connection.png"),
    ...connect(),
    ...enterSpace(),
    `await app.waitFor("text:/the runner is paired with Acme/", { timeout: 15000 })`,
    ...(EVENT_ID
      ? [
          `await app.press(${q(`id:react:${EVENT_ID}`)})`,
          `await app.press(${q(`id:pick-reaction:${EVENT_ID}:👍`)})`,
          `await app.waitFor(${q(`id:reaction:${EVENT_ID}:👍`)}, { timeout: 10000 })`,
        ]
      : []),
    `await app.type("id:composer", ${q(ALICE_SAYS)})`,
    `await app.press("id:send")`,
    `await app.waitFor(${q("text:" + ALICE_SAYS)}, { timeout: 10000 })`,
    ...(AGENT
      ? [
          `await app.type("id:composer", "@")`,
          `await app.press("id:pick:agent")`,
          `await app.type("id:composer", "please acknowledge")`,
          `await app.press("id:send")`,
          `await app.waitFor("text:👀", { timeout: 20000 })`,
        ]
      : []),
    `await app.press("id:run-demo")`,
    `await app.waitFor("text:/README\\\\.md/", { timeout: 30000 })`,
    `await app.wait(1000)`,
    shot("room-runner.png"),
    `await app.press("id:manage-space")`,
    `await app.waitFor("id:member:bob", { timeout: 10000 })`,
    `await app.wait(300)`,
    shot("space-members.png"),
  ],
  // Only with a configured model: ask the Space's agent, then look at the Chat's run controls.
  // Delegate to a child thread, then look at the Chat's run controls while it runs.
  "alice-chat": () => [
    ...open,
    ...connect(),
    ...enterSpace(),
    `await app.type("id:composer", "@")`,
    `await app.waitFor("id:pick:agent", { timeout: 10000 })`,
    `await app.press("id:pick:agent")`,
    `await app.type("id:composer", "please delegate a workspace audit to a thread")`,
    `await app.press("id:send")`,
    `await app.waitFor("text:workspace-audit", { timeout: 20000 })`,
    `await app.press("id:nav-chat")`,
    `await app.waitFor("id:runs-aside", { timeout: 10000 })`,
    `await app.type("id:composer", "I'll follow the workspace audit from the runs panel. Please acknowledge.")`,
    `await app.press("id:ask-agent")`,
    `await app.waitFor("text:👀", { timeout: 20000 })`,
    `await app.waitFor("text:Acme › #workspace-audit", { timeout: 20000 })`,
    `await app.wait(800)`,
    shot("chat-runs.png"),
  ],
  // After the child run finished: the thread, nested under the lobby, with its tool call, result and answer.
  "alice-agent": () => [
    ...open,
    ...connect(),
    ...enterSpace(),
    `await app.waitFor(${q(`id:room:${THREAD_ID}`)}, { timeout: 15000 })`,
    `await app.press(${q(`id:room:${THREAD_ID}`)})`,
    `await app.waitFor("text:/The workspace has/", { timeout: 25000 })`,
    `await app.wait(800)`,
    shot("thread.png"),
  ],
  invalid: () => [
    ...open,
    `await app.type("id:access-token", "not-a-valid-token")`,
    `await app.press("id:connect")`,
    `await app.waitFor("id:connect-error", { timeout: 10000 })`,
    `await app.wait(300)`,
    shot("connection-error.png"),
  ],
};

const name = process.argv[2];
if (!scenarios[name]) throw new Error(`unknown scenario ${name}; one of ${Object.keys(scenarios).join(", ")}`);
if (name.startsWith("alice-") && !AGENT) throw new Error(`${name} needs a configured model (AGENT set)`);
console.log(scenarios[name]().join("\n"));
