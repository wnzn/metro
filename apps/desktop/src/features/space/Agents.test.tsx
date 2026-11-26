// A Space with several agents in the room: the @ list (generic @agent plus
// each named agent, with pictures), the Ask-agent chooser (default first, the
// classifier only when the person opted in), what each sends, and how runs
// and replies show their agent. The shared fake server plus this lane's Space
// routes (./test/fixtures.ts); fake tokens only.

import { expect, open, test } from "nsh/test";
import type { App } from "nsh/test";
import { BASE, TOKENS } from "../../../test/fakeServer";
import { spaceWorld } from "./test/fixtures";
import { launched } from "../../../test/launched";

type W = ReturnType<typeof spaceWorld>;

async function inAcme(w: W): Promise<App> {
  const app = await open("./test/Harness.tsx", { modules: launched(), network: w.network as any, platform: "linux", width: 1280, height: 900 });
  await app.clear("id:server-url");
  await app.type("id:server-url", BASE);
  await app.type("id:access-token", TOKENS.alice!);
  await app.press("id:connect");
  await app.waitFor("id:space:acme");
  await app.press("id:space:acme");
  await app.waitFor("text:hello from bob in acme");
  return app;
}

const messages = (w: W) => w.f.posts().filter((p) => p.path.endsWith("/messages"));

function withCoder(w: W) {
  return w.agent("acme", { name: "Coder", handle: "coder", description: "Writes and reviews code.", color: "#059669" });
}

test("the @ list offers the generic @agent and each named agent; a named one is sent by id", async () => {
  const w = spaceWorld();
  const coder = withCoder(w);
  const app = await inAcme(w);
  await app.type("id:composer", "please @");
  await app.waitFor(`id:pick-agent:${coder.id}`);
  // The default agent keeps the handle "agent": it is the generic entry, not listed twice.
  expect(app.shows("id:pick:agent")).toBe(true);
  expect(app.shows("id:pick-agent:ag-1")).toBe(false);
  expect(app.shows("id:pick:bob")).toBe(true);
  // Typing narrows by handle or name.
  await app.type("id:composer", "co");
  await app.waitUntil(() => !app.shows("id:pick:agent") && !app.shows("id:pick:bob"));
  await app.press(`id:pick-agent:${coder.id}`);
  await app.waitFor(`id:mention-chip:agent:${coder.id}`);
  expect(app.value("id:composer")).toBe("please @Coder ");
  await app.type("id:composer", "review the diff");
  const before = messages(w).length;
  await app.press("id:send");
  await app.waitUntil(() => messages(w).length === before + 1);
  // Only the wire form: the agent's id, never its handle or addressed_by.
  expect(messages(w).at(-1)!.body).toEqual({ text: "please @Coder review the diff", invoke_agent: false, infer_text_mentions: false, defer_decisions: true,
    mentions: [{ kind: "agent", agent_id: coder.id }], spans: [{ start: 7, end: 13, index: 0 }] });
});

test("Ask agent: one agent has no chooser; with two, the default is preselected and another can be chosen", async () => {
  const w = spaceWorld();
  const app = await inAcme(w);
  expect(app.shows("id:ask-agent-choose")).toBe(false);
  await app.type("id:composer", "hello");
  await app.press("id:ask-agent");
  await app.waitUntil(() => messages(w).length > 0);
  expect(messages(w).at(-1)!.body).toEqual({ text: "hello", invoke_agent: true, infer_text_mentions: false, defer_decisions: true });

  const w2 = spaceWorld();
  const coder = withCoder(w2);
  const app2 = await inAcme(w2);
  await app2.waitFor("id:ask-agent-choose");
  expect(app2.shows("id:ask-agent >> text:Ask Metro")).toBe(true);
  await app2.press("id:ask-agent-choose");
  await app2.waitFor(`id:ask-agent-choose:${coder.id}`);
  // No classifier connection: no classifier choice.
  expect(app2.shows("id:ask-agent-choose:classifier")).toBe(false);
  await app2.press(`id:ask-agent-choose:${coder.id}`);
  await app2.waitFor("id:ask-agent >> text:Ask Coder");
  await app2.type("id:composer", "fix the build");
  await app2.press("id:ask-agent");
  await app2.waitUntil(() => messages(w2).length > 0);
  expect(messages(w2).at(-1)!.body).toEqual({ text: "fix the build", invoke_agent: true, agent_id: coder.id, infer_text_mentions: false, defer_decisions: true });

  // Back to the default: its id is sent explicitly.
  await app2.press("id:ask-agent-choose");
  await app2.press("id:ask-agent-choose:ag-1");
  await app2.waitFor("id:ask-agent >> text:Ask Metro");
  await app2.type("id:composer", "status?");
  await app2.press("id:ask-agent");
  await app2.waitUntil(() => messages(w2).length > 1);
  expect(messages(w2).at(-1)!.body).toEqual({ text: "status?", invoke_agent: true, agent_id: "ag-1", infer_text_mentions: false, defer_decisions: true });
});

test("Let the classifier choose is offered only with classification on; it sends no agent id", async () => {
  const w = spaceWorld();
  withCoder(w);
  w.s.classifier.add("alice");
  w.s.prefs.set("acme/alice", { classify_unaddressed: true, default_model: null, agent_models: {}, routing: true });
  const app = await inAcme(w);
  await app.waitFor("id:ask-agent-choose");
  await app.press("id:ask-agent-choose");
  await app.waitFor("id:ask-agent-choose:classifier");
  await app.press("id:ask-agent-choose:classifier");
  await app.waitFor("id:ask-agent >> text:Ask agent");
  await app.type("id:composer", "which one?");
  await app.press("id:ask-agent");
  await app.waitUntil(() => messages(w).length > 0);
  expect(messages(w).at(-1)!.body).toEqual({ text: "which one?", invoke_agent: true, infer_text_mentions: false, defer_decisions: true });
});

test("a run and its reply show their agent: name, picture or initial, and how it was picked", async () => {
  const w = spaceWorld();
  const coder = withCoder(w);
  const badge = { id: coder.id, handle: "coder", name: "Coder", color: "#059669" };
  const a = { id: "run_c1", name: "Coder", source: "agent", agent: badge };
  w.f.post("acme/lobby", { kind: "message", actor: "alice", text: "can someone check the tests?", mentions: [{ kind: "agent", addressed_by: "classifier", agent_id: coder.id }], audience: "agent" } as any);
  w.f.post("acme/lobby", { kind: "activity", actor: "agent", text: "", activity: { ...a, phase: "started", pick: "Picked by classifier: Coder" } } as any);
  w.f.post("acme/lobby", { kind: "reasoning_summary", actor: "agent", text: "Look at the failing test first.", activity: a } as any);
  w.f.post("acme/lobby", { kind: "activity", actor: "agent", text: "", activity: { ...a, phase: "completed" } } as any);
  const reply = w.f.post("acme/lobby", { kind: "agent", actor: "agent", text: "The flaky test depends on the clock.", activity: a } as any);
  const app = await inAcme(w);
  await app.waitFor(`id:event:${reply.id}`);
  expect(app.shows(`id:author:${reply.id}`)).toBe(true);
  expect(app.shows("label:Coder, agent")).toBe(true);
  // Its initial on its colour, not the generic agent glyph.
  expect(app.shows(`id:avatar:${reply.id} >> text:C`)).toBe(true);
  // A classifier's pick shows the agent normally: no pick label, no "detected" pill.
  await app.waitFor("id:activity-toggle:agent:run_c1");
  expect(app.shows("id:activity-pick:agent:run_c1")).toBe(false);
  expect(app.shows("text:Picked by classifier: Coder")).toBe(false);
  await app.press("id:activity-toggle:agent:run_c1");
  await app.waitFor("text:Look at the failing test first.");
  expect(app.shows("text:Coder · detected by System One")).toBe(false);
  expect(app.texts().join(" ").includes("System One")).toBe(false);
});

test("every member opens Space settings from the room header and the Space's menu", async () => {
  const w = spaceWorld();
  const app = await inAcme(w);
  await app.press("id:space-settings-open");
  await app.waitFor("id:space-settings");
  await app.press("id:space-settings-close");
  await app.waitFor("id:space-settings", { state: "gone" });
  await app.press("id:space-menu:acme");
  await app.press("id:space-agents:acme");
  await app.waitFor("id:space-settings");
  await app.waitFor("id:space-agents");
});
