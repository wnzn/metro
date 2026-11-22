// Each agent turn shows as one collapsed group; people's messages, the agent's
// final reply and errors stay visible. Real NSH engine, fake server.

import { expect, open, test } from "nsh/test";
import type { App } from "nsh/test";
import { BASE, TOKENS, world } from "../../../test/fakeServer";

type F = ReturnType<typeof world>;
type Act = { id: string; name: string; source: "agent" | "demo"; phase?: string };

async function signedIntoAcme(f: F, user: "alice" | "bob" = "alice") {
  const app = await open("../../../App.tsx", { network: f.network as any, platform: "linux", width: 1200, height: 800 });
  await app.clear("id:server-url");
  await app.type("id:server-url", BASE);
  await app.type("id:access-token", TOKENS[user]!);
  await app.press("id:connect");
  await app.waitFor("id:space:acme");
  await app.press("id:space:acme");
  await app.waitFor("text:hello from bob in acme");
  return app;
}

const turn = (id: string, name = "Metro"): Act => ({ id, name, source: "agent" });
const phase = (a: Act, p: string): Act => ({ ...a, phase: p });
const post = (f: F, ev: { kind: string; actor: string; text: string; activity?: Act; mentions?: unknown[]; audience?: string }, room = "acme/lobby") => f.post(room, ev as any);

function startTurn(f: F, a: Act) {
  const ask = post(f, { kind: "message", actor: "alice", text: "@agent what is in the workspace?", mentions: [{ kind: "agent" }], audience: "agent" });
  post(f, { kind: "activity", actor: "agent", text: "", activity: phase(a, "started") });
  const call = post(f, { kind: "tool_call", actor: "agent", text: 'workspace.list {"path":"."}', activity: a });
  return { ask, call };
}
function finishTurn(f: F, a: Act) {
  const result = post(f, { kind: "tool_result", actor: "runner", text: "workspace.list ok\nREADME.md\nsrc/", activity: a });
  const summary = post(f, { kind: "reasoning_summary", actor: "agent", text: "Listed the root before answering.", activity: a });
  post(f, { kind: "activity", actor: "agent", text: "", activity: phase(a, "completed") });
  const reply = post(f, { kind: "agent", actor: "agent", text: "The workspace has a README and a src folder.", activity: a });
  return { result, summary, reply };
}

const shown = (app: App, s: string) => app.shows(s);

test("a live turn is one collapsed 'thinking' group that becomes the agent's activity", async () => {
  const f = world();
  const a = turn("run-1");
  const { ask, call } = startTurn(f, a);
  const app = await signedIntoAcme(f);
  await app.waitFor("text:Metro is thinking…");
  expect(shown(app, "id:activity-toggle:agent:run-1")).toBe(true);
  expect(shown(app, "text:1 step")).toBe(true);
  expect(shown(app, 'text:/"path"/')).toBe(false);
  expect(shown(app, `id:activity-step:${call.id}`)).toBe(false);

  const { result, summary, reply } = finishTurn(f, a);
  await app.waitFor("text:The workspace has a README and a src folder.");
  await app.waitFor("text:Metro’s activity");
  expect(shown(app, "text:Metro is thinking…")).toBe(false);
  expect(shown(app, "text:3 steps")).toBe(true);
  // Only one disclosure for the whole turn: no per-step toggles.
  expect(shown(app, `id:activity-toggle:event:${call.id}`)).toBe(false);
  expect(shown(app, `id:activity-toggle:event:${result.id}`)).toBe(false);
  expect(shown(app, "text:/README\\.md/")).toBe(false);
  // The question and the final answer stay visible, with their reactions.
  expect(shown(app, "text:@agent what is in the workspace?")).toBe(true);
  expect(shown(app, `id:recipient:${ask.id}:agent`)).toBe(true);
  expect(shown(app, `id:react:${reply.id}`)).toBe(true);
  expect(shown(app, `id:react:${result.id}`)).toBe(false);

  // One press shows every step, in order, with arguments, output and the provider's summary.
  await app.press("id:activity-toggle:agent:run-1");
  await app.waitFor("id:activity-details:agent:run-1");
  for (const id of [call.id, result.id, summary.id]) expect(shown(app, `id:activity-step:${id}`)).toBe(true);
  expect(shown(app, 'text:workspace.list {"path":"."}')).toBe(true);
  expect(shown(app, "text:README.md\nsrc/")).toBe(true);
  expect(shown(app, "text:Thinking · summary shared by the model provider")).toBe(true);
  expect(shown(app, "text:Listed the root before answering.")).toBe(true);
  await app.press("id:activity-toggle:agent:run-1");
  await app.waitFor("id:activity-details:agent:run-1", { state: "gone" });
  expect(shown(app, "text:The workspace has a README and a src folder.")).toBe(true);
});

test("a live turn shows before its first tool; an ended empty one leaves nothing behind", async () => {
  const f = world();
  post(f, { kind: "activity", actor: "agent", text: "", activity: phase(turn("early", "Scout"), "started") });
  post(f, { kind: "activity", actor: "agent", text: "", activity: phase(turn("done"), "started") });
  post(f, { kind: "activity", actor: "agent", text: "", activity: phase(turn("done"), "completed") });
  post(f, { kind: "agent", actor: "agent", text: "plain answer", activity: turn("done") });
  const app = await signedIntoAcme(f);
  await app.waitFor("text:plain answer");
  expect(shown(app, "id:activity-title:agent:early")).toBe(true); // no steps yet: a plain status, nothing to open
  expect(shown(app, "text:Scout is thinking…")).toBe(true);
  expect(shown(app, "id:activity-toggle:agent:early")).toBe(false);
  expect(shown(app, "id:activity:agent:done")).toBe(false);
});

test("later steps join their group in place and keep it open across polls", async () => {
  const f = world();
  const a = turn("run-2");
  startTurn(f, a);
  const app = await signedIntoAcme(f);
  await app.press("id:activity-toggle:agent:run-2");
  await app.waitFor("id:activity-details:agent:run-2");

  post(f, { kind: "message", actor: "bob", text: "bob speaks after the call" });
  await app.waitFor("text:bob speaks after the call");
  const later = post(f, { kind: "tool_call", actor: "agent", text: 'workspace.read {"path":"README.md"}', activity: a });
  post(f, { kind: "message", actor: "carol", text: "carol speaks last" });
  await app.waitFor("text:carol speaks last");
  await app.waitFor(`id:activity-step:${later.id}`);
  expect(shown(app, "id:activity-details:agent:run-2")).toBe(true); // still open
  expect(shown(app, "text:2 steps")).toBe(true);
  // The group stays where it started; people's messages keep their order.
  const t = app.texts();
  const at = (s: string) => t.indexOf(s);
  for (const s of ["Metro is thinking…", "bob speaks after the call", "carol speaks last", 'workspace.read {"path":"README.md"}']) expect(at(s)).toBeGreaterThan(-1);
  expect(at("Metro is thinking…")).toBeLessThan(at("bob speaks after the call"));
  expect(at("bob speaks after the call")).toBeLessThan(at("carol speaks last"));
  expect(at('workspace.read {"path":"README.md"}')).toBeLessThan(at("bob speaks after the call"));
});

test("interleaved runs with the same name are separate groups", async () => {
  const f = world();
  const one = turn("run-a");
  const two = turn("run-b");
  post(f, { kind: "activity", actor: "agent", text: "", activity: phase(one, "started") });
  post(f, { kind: "activity", actor: "agent", text: "", activity: phase(two, "started") });
  post(f, { kind: "tool_call", actor: "agent", text: 'workspace.read {"path":"A.md"}', activity: one });
  post(f, { kind: "tool_call", actor: "agent", text: 'workspace.read {"path":"B.md"}', activity: two });
  post(f, { kind: "activity", actor: "agent", text: "", activity: phase(two, "completed") });
  const app = await signedIntoAcme(f);
  await app.waitFor("id:activity:agent:run-b");
  expect(shown(app, "id:activity:agent:run-a >> text:Metro is thinking…")).toBe(true);
  expect(shown(app, "id:activity:agent:run-b >> text:Metro’s activity")).toBe(true);
  await app.press("id:activity-toggle:agent:run-b");
  await app.waitFor("text:/B\\.md/");
  expect(shown(app, "text:/A\\.md/")).toBe(false);
  expect(shown(app, "id:activity-details:agent:run-a")).toBe(false);
});

test("failed, cancelled and interrupted turns say so while collapsed; errors stay visible", async () => {
  const f = world();
  const bad = turn("bad");
  post(f, { kind: "activity", actor: "agent", text: "", activity: phase(bad, "started") });
  post(f, { kind: "tool_result", actor: "runner", text: "workspace.check failed\nexit status 1", activity: bad });
  post(f, { kind: "error", actor: "system", text: "workspace.read failed: runner offline", activity: bad });
  const stop = turn("stop");
  post(f, { kind: "activity", actor: "agent", text: "", activity: phase(stop, "started") });
  post(f, { kind: "tool_call", actor: "agent", text: "workspace.list {}", activity: stop });
  post(f, { kind: "activity", actor: "agent", text: "", activity: phase(stop, "cancelled") });
  post(f, { kind: "status", actor: "system", text: "The agent run was cancelled." });
  const cut = turn("cut");
  post(f, { kind: "activity", actor: "agent", text: "", activity: phase(cut, "started") });
  post(f, { kind: "tool_call", actor: "agent", text: "workspace.list {}", activity: cut });
  post(f, { kind: "activity", actor: "agent", text: "The server restarted.", activity: phase(cut, "interrupted") });
  const app = await signedIntoAcme(f);
  await app.waitFor("text:workspace.read failed: runner offline");
  expect(shown(app, "text:The agent run was cancelled.")).toBe(true);
  expect(shown(app, "text:/thinking/")).toBe(false);
  expect(shown(app, "id:activity-outcome:agent:bad >> text:failed")).toBe(true);
  expect(shown(app, "id:activity-outcome:agent:stop >> text:cancelled")).toBe(true);
  expect(shown(app, "id:activity-outcome:agent:cut >> text:interrupted")).toBe(true);
  expect(shown(app, "text:exit status 1")).toBe(false);
  await app.press("id:activity-toggle:agent:cut");
  await app.waitFor("text:The server restarted.");
});

test("Demo is Runner activity, not thinking, and never the person's name", async () => {
  const f = world();
  const app = await signedIntoAcme(f);
  await app.press("id:run-demo");
  await app.waitFor("text:Runner activity");
  expect(shown(app, "text:/thinking/")).toBe(false);
  expect(shown(app, "text:Demo: workspace.list on the Space's runner")).toBe(true); // the request stays top-level
  expect(shown(app, "text:/alice’s activity/")).toBe(false);
  expect(shown(app, "text:README.md\nsrc/")).toBe(false);
  const g = f.state.events.get("acme/lobby")!.find((e) => e.kind === "demo")!;
  await app.press(`id:activity-toggle:demo:${(g as any).activity.id}`);
  await app.waitFor("text:README.md\nsrc/");
});

test("older events without metadata each stay collapsed on their own", async () => {
  const f = world();
  const call = post(f, { kind: "tool_call", actor: "agent", text: 'workspace.list {"path":"."}' });
  const result = post(f, { kind: "tool_result", actor: "runner", text: "workspace.list ok\nREADME.md\nsrc/" });
  const human = post(f, { kind: "tool_call", actor: "bob", text: "workspace.list {}" });
  const app = await signedIntoAcme(f);
  await app.waitFor(`id:activity-toggle:event:${result.id}`);
  expect(shown(app, `id:activity:event:${call.id} >> text:Metro’s activity`)).toBe(true);
  expect(shown(app, `id:activity:event:${result.id} >> text:Runner activity`)).toBe(true);
  expect(shown(app, `id:activity:event:${human.id} >> text:Runner activity`)).toBe(true);
  expect(shown(app, "text:/bob’s activity|runner is thinking/i")).toBe(false);
  await app.press(`id:activity-toggle:event:${result.id}`);
  await app.waitFor("text:README.md\nsrc/");
  expect(shown(app, `id:activity-details:event:${call.id}`)).toBe(false);
});

test("another Space with the same activity id doesn't inherit an open group", async () => {
  const f = world();
  const a = turn("same");
  startTurn(f, a);
  finishTurn(f, a);
  post(f, { kind: "activity", actor: "agent", text: "", activity: phase(a, "started") }, "side/lobby");
  post(f, { kind: "tool_result", actor: "runner", text: "workspace.list ok\nSIDE-ONLY.txt", activity: a }, "side/lobby");
  const app = await signedIntoAcme(f);
  await app.press("id:activity-toggle:agent:same");
  await app.waitFor("text:/README\\.md/");

  await app.press("id:space:side");
  await app.waitFor("text:side lobby only");
  await app.waitFor("id:activity-toggle:agent:same");
  expect(shown(app, "id:activity-details:agent:same")).toBe(false);
  expect(shown(app, "text:/SIDE-ONLY/")).toBe(false);

  await app.press("id:space:acme");
  await app.waitFor("text:/README\\.md/"); // still open where the user opened it
});

test("screen readers hear the group's title, outcome, size and whether it is open", async () => {
  const f = world();
  const a = turn("kb");
  startTurn(f, a);
  finishTurn(f, a);
  const app = await signedIntoAcme(f);
  // The toggle is NSH's Button: a keyboard focus stop that Space and Return press.
  await app.waitFor("label:Show details: Metro\u2019s activity, completed, 3 steps");
  await app.press("label:Show details: Metro\u2019s activity, completed, 3 steps");
  await app.waitFor("id:activity-details:agent:kb");
  expect(shown(app, "label:Hide details: Metro\u2019s activity, completed, 3 steps")).toBe(true);
});
