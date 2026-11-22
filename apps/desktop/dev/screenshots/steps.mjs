// Prints the `nsh test App.tsx` steps of one capture scenario on stdout.
// Inputs come from the environment; a token is read from TOKEN_FILE (mode 600)
// and JSON-quoted into a `type` step, so it never appears in argv, logs or a
// screenshot (screenshots are taken before it is typed or after it is gone).
//
//   URL=… SPACE=<space id> SHOTS=/shots TOKEN_FILE=… [EVENT_ID=…] [AGENT=…] [SITE=…] [FIXTURE_URL=…]
//     node steps.mjs <bob|alice|alice-activity|alice-chat|alice-agent|alice-settings|invalid>
//
// Settings shots never show a token: a token just issued stays masked (the
// steps never press Show), and API keys are never typed (the fixture
// endpoint is keyless).
import { readFileSync } from "node:fs";

const q = JSON.stringify;
const { URL, SPACE, SHOTS = "/shots", TOKEN_FILE, EVENT_ID, AGENT, THREAD_ID, SITE, FIXTURE_URL } = process.env;
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
    // The Demo's call and listing arrive as one collapsed "Runner activity" group (dark theme by default).
    `await app.waitFor("text:Runner activity", { timeout: 30000 })`,
    `await app.wait(1000)`,
    shot("room-runner.png"),
    `await app.press("id:manage-space")`,
    `await app.waitFor("id:member:bob", { timeout: 10000 })`,
    `await app.wait(300)`,
    shot("space-members.png"),
  ],
  // One real agent turn (Rig + the runner's workspace.list): first live and
  // collapsed while the model holds its answer, then finished and expanded.
  "alice-activity": () => [
    ...open,
    ...connect(),
    ...enterSpace(),
    `await app.type("id:composer", "@")`,
    `await app.waitFor("id:pick:agent", { timeout: 10000 })`,
    `await app.press("id:pick:agent")`,
    `await app.type("id:composer", "what is in the workspace?")`,
    `await app.press("id:send")`,
    `await app.waitFor("text:Metro is thinking…", { timeout: 20000 })`,
    // Live, with the call and the runner's listing inside: the Demo's group is the other "2 steps".
    `await app.waitUntil(() => app.count("text:2 steps") >= 2 && app.shows("text:Metro is thinking…"), { timeout: 20000 })`,
    `await app.wait(800)`,
    shot("agent-thinking.png"),
    `await app.waitFor("text:/The workspace has/", { timeout: 40000 })`,
    `await app.waitFor("label:Show details: Metro\u2019s activity, completed, 2 steps", { timeout: 10000 })`,
    `if (app.shows("text:Metro is thinking…")) throw new Error("a finished turn still says it is thinking")`,
    `await app.press("label:Show details: Metro\u2019s activity, completed, 2 steps")`,
    `await app.waitFor("label:Hide details: Metro\u2019s activity, completed, 2 steps", { timeout: 5000 })`,
    `await app.wait(800)`,
    shot("agent-activity-expanded.png"),
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
  // Settings with no secret on screen: runners, model providers (with the
  // allowlisted keyless fixture), device onboarding (token masked), appearance,
  // and the Website Space served by the organization runner.
  "alice-settings": () => [
    ...open,
    ...connect(),
    `await app.press("id:nav-settings")`,
    `await app.waitFor("id:runners-settings", { timeout: 10000 })`,
    `await app.waitFor("text:Shared build runner", { timeout: 10000 })`,
    `await app.waitFor("text:Alice workstation", { timeout: 10000 })`,
    `await app.wait(500)`,
    shot("settings-runners.png"),
    `await app.press("id:nav-settings:providers")`,
    `await app.waitFor("id:provider-picker", { timeout: 10000 })`,
    ...(FIXTURE_URL
      ? [
`await app.press("id:provider-picker")`,
          `await app.press("id:provider-picker:custom")`,
          `await app.waitFor("id:provider-ways:custom", { timeout: 5000 })`,
          `await app.press("id:provider:custom")`,
          `await app.waitFor("id:server-form", { timeout: 5000 })`,
          `await app.clear("id:provider-name")`,
          `await app.type("id:provider-name", "Local fixture (keyless)")`,
          `await app.press(${q(`id:provider-url:${FIXTURE_URL}`)})`,
          `await app.press("id:provider-save-fetch")`,
          `await app.waitFor("text:Local fixture (keyless)", { timeout: 10000 })`,
          `await app.waitFor("text:metro-scripted-test-model", { timeout: 15000 })`,
          `await app.press("text:metro-scripted-test-model")`,
          `await app.waitFor("text:/now uses metro-scripted-test-model/", { timeout: 10000 })`,
          `if (app.shows("text:Make default")) { await app.press("text:Make default"); await app.waitFor("text:/is now your default/", { timeout: 10000 }) }`,
          `await app.wait(500)`,
          shot("settings-providers.png"),
`await app.press("id:provider-picker")`,
          `await app.press("id:provider-picker:custom")`,
          `await app.waitFor("id:provider-ways:custom", { timeout: 5000 })`,
          `await app.press("id:provider:device-openai-compatible")`,
          `await app.waitFor("id:device-form", { timeout: 5000 })`,
          `await app.clear("id:provider-name")`,
          `await app.type("id:provider-name", "Home GPU")`,
          `await app.press("id:provider-create-device")`,
          `await app.waitFor("id:token-reveal", { timeout: 10000 })`,
          `await app.press("id:reveal-test-connection")`,
          `await app.waitFor("text:/didn't answer/", { timeout: 20000 })`,
          `await app.waitFor("text:Paired · offline", { timeout: 10000 })`,
          `await app.scroll("id:providers-settings", -5000)`, // back to the (masked) token and the first steps
          `await app.wait(500)`,
          shot("settings-device-onboarding.png"),
          `await app.press("id:hide-runner-token")`,
        ]
      : []),
    `await app.press("id:nav-settings:appearance")`,
    `await app.waitFor("id:appearance-settings", { timeout: 10000 })`,
    `await app.wait(500)`,
    shot("settings-appearance.png"),
    `await app.press("id:settings-close")`,
    ...(SITE
      ? [
          `await app.press(${q(`id:space:${SITE}`)})`,
          `await app.waitFor("id:space-view", { timeout: 15000 })`,
          `await app.press("id:manage-space")`,
          `await app.waitFor("id:attached-runner", { timeout: 10000 })`,
          `await app.wait(500)`,
          shot("settings-space-runner.png"),
        ]
      : []),
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
