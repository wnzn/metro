// A turn says which model answered and why (its `started` marker's
// `activity.route`), as a compact "Routed: … · N%" part of the one collapsed
// row and a complete sentence only in its opened details. The raw "(p 0.91)"
// is never shown. A message System One judged to be for the agent is labelled
// as detected, never shown as an "@agent" its sender typed.

import { expect, open, test } from "nsh/test";
import type { App } from "nsh/test";
import { BASE, TOKENS, world } from "../../test/fakeServer";
import { launched } from "../../test/launched";

type F = ReturnType<typeof world>;
type Act = { id: string; name: string; source: "agent" | "demo"; phase?: string; route?: unknown };

async function inAcme(f: F) {
  const app = await open("../../App.tsx", { modules: launched(), network: f.network as any, platform: "linux", width: 1200, height: 800 });
  await app.clear("id:server-url");
  await app.type("id:server-url", BASE);
  await app.type("id:access-token", TOKENS.alice!);
  await app.press("id:connect");
  await app.waitFor("id:space:acme");
  await app.press("id:space:acme");
  await app.waitFor("text:hello from bob in acme");
  return app;
}

const post = (f: F, ev: { kind: string; actor: string; text: string; activity?: Act; mentions?: unknown[]; audience?: string }) => f.post("acme/lobby", ev as any);

function turn(f: F, id: string, route: unknown, withTool: boolean) {
  const a: Act = { id, name: "Metro", source: "agent" };
  post(f, { kind: "activity", actor: "agent", text: "", activity: { ...a, phase: "started", route } });
  if (withTool) {
    post(f, { kind: "tool_call", actor: "agent", text: 'workspace.list {"path":"."}', activity: a });
    post(f, { kind: "tool_result", actor: "runner", text: "workspace.list ok\nREADME.md", activity: a });
  }
  post(f, { kind: "activity", actor: "agent", text: "", activity: { ...a, phase: "completed" } });
  return post(f, { kind: "agent", actor: "agent", text: `reply for ${id}`, activity: a });
}

const text = (app: App) => app.texts().join(" | ");

test("a classifier-started, use-case-routed turn: the agent shown plainly, one row with a badge, the reason only when opened", async () => {
  const f = world();
  const ask = post(f, {
    kind: "message",
    actor: "alice",
    text: "could someone review the diff in src/?",
    mentions: [{ kind: "agent", addressed_by: "classifier" }],
    audience: "agent",
  });
  turn(f, "r1", {
    source: "case",
    addressed_by: "classifier",
    case_key: "code_review",
    case_label: "Code review",
    connection_id: "pc_0123456789abcdef0123456789abcdef",
    provider_id: "openai",
    model_id: "gpt-fixture-2",
    reason: 'System One matched the use case "Code review" (p 0.91)',
  }, true);
  const app = await inAcme(f);
  await app.waitFor("text:reply for r1");

  // The agent the classifier picked, shown like any agent recipient, with no "detected" label.
  expect(app.shows(`id:recipient:${ask.id}:agent`)).toBe(true);
  expect(app.shows("text:agent · detected by System One")).toBe(false);
  expect(app.shows(`id:event:${ask.id} >> text:to the agent`)).toBe(false); // the recipient pill already says it

  // One collapsed row with a compact badge; no sentence, model or connection id in the transcript.
  expect(app.count("id:activity:agent:r1")).toBe(1);
  expect(app.shows("id:activity-route-badge:agent:r1 >> text:Routed: Code review · 91%")).toBe(true);
  expect(app.shows("id:activity-route:agent:r1")).toBe(false);
  expect(text(app)).not.toContain("p 0.91");
  expect(text(app)).not.toContain("System One matched");
  expect(text(app)).not.toContain("gpt-fixture-2");
  expect(text(app)).not.toContain("pc_0123456789abcdef");
  expect(app.shows("id:activity-tools:agent:r1 >> text:1 tool")).toBe(true); // the route is not a tool

  await app.press("id:activity-toggle:agent:r1");
  await app.waitFor("id:activity-route:agent:r1");
  const line = text(app);
  expect(line).toContain("Use case “Code review” (openai · gpt-fixture-2) answered.");
  expect(line).not.toMatch(/System One|classifier/i); // never attributed, whatever an older server stored
  expect(line).not.toContain("(p 0.91)"); // never the raw probability
  expect(line).not.toContain("pc_0123456789abcdef"); // connection ids are never shown
});

test("a default route adds nothing: no badge, and a turn with nothing to inspect leaves no row", async () => {
  const f = world();
  post(f, { kind: "message", actor: "alice", text: "@agent hi", mentions: [{ kind: "agent" }], audience: "agent" });
  const route = { source: "default", addressed_by: "explicit", reason: "your default chat connection" };
  turn(f, "plain", route, false);
  turn(f, "tools", route, true);
  const app = await inAcme(f);
  await app.waitFor("text:reply for tools");
  expect(app.shows("id:activity:agent:plain")).toBe(false); // no empty shell
  expect(app.shows("id:activity:agent:tools")).toBe(true);
  expect(app.shows("id:activity-route-badge:agent:tools")).toBe(false);
  expect(app.shows("text:@agent")).toBe(true); // typed, so shown as typed
  await app.press("id:activity-toggle:agent:tools");
  await app.waitFor("id:activity-route:agent:tools");
  expect(text(app)).toContain("The default chat model of the person who asked answered: your default chat connection.");
});

test("a routed turn with no tool steps stays one row with its badge, and opens to its route", async () => {
  const f = world();
  turn(f, "quick", { source: "override", addressed_by: "explicit", model_id: "qwen-fixture", reason: "the model you chose for this message" }, false);
  const app = await inAcme(f);
  await app.waitFor("text:reply for quick");
  await app.waitFor("id:activity-route-badge:agent:quick");
  expect(app.shows("id:activity-route-badge:agent:quick >> text:Routed: chosen model")).toBe(true);
  await app.press("id:activity-toggle:agent:quick");
  await app.waitFor("id:activity-route:agent:quick");
  expect(text(app)).toContain("The model chosen for this message (qwen-fixture) answered: the model you chose for this message.");
});

test("a malformed route is ignored, and the composer never sends addressed_by", async () => {
  const f = world();
  turn(f, "odd", { source: "telepathy", reason: "x" }, true);
  const app = await inAcme(f);
  await app.waitFor("text:reply for odd");
  expect(app.shows("id:activity-route-badge:agent:odd")).toBe(false);
  await app.type("id:composer", "@agent ping");
  await app.press("id:ask-agent");
  await app.waitUntil(() => f.posts().some((p) => p.path === "/v1/spaces/acme/rooms/lobby/messages"));
  const sent = f.posts().find((p) => p.path === "/v1/spaces/acme/rooms/lobby/messages")!;
  expect(JSON.stringify(sent.body)).not.toContain("addressed_by");
});

test("a probability of 1.00 reads 100% on the badge and in words when opened, never as p 1.00", async () => {
  const f = world();
  turn(f, "sure", {
    source: "case",
    addressed_by: "classifier",
    case_key: "incident_review",
    case_label: "Incident review",
    provider_id: "custom",
    model_id: "fixture/shared-local",
    reason: 'System One matched the use case "Incident review" (p 1.00)',
  }, false);
  const app = await inAcme(f);
  await app.waitFor("id:activity-route-badge:agent:sure");
  expect(app.shows("id:activity-route-badge:agent:sure >> text:Routed: Incident review · 100%")).toBe(true);
  await app.press("id:activity-toggle:agent:sure");
  await app.waitFor("id:activity-route:agent:sure");
  expect(text(app)).not.toMatch(/System One|classifier/i);
  expect(text(app)).not.toContain("p 1.00");
});
