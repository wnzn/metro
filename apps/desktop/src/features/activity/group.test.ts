import { expect, test } from "nsh/test";
import type { EventActivity, RoomEvent } from "../../api/client";
import { toActivity } from "../../api/client";
import { currentStep, durationText, groupAccessibleLabel, groupOutcomeText, groupTitle, groupTranscript, liveTitle, pickBadge, routeBadge, routeConfidence, routeText, shortArgs, summaryParts, timeline, toolCount } from "./group";
import type { ActivityGroup, TranscriptItem } from "./group";

let nextId = 1;
const ev = (kind: string, actor: string, text: string, activity?: EventActivity): RoomEvent => ({ id: nextId++, kind, actor, text, ...(activity ? { activity } : {}) });
const run = (id: string, name = "Metro", phase?: EventActivity["phase"]): EventActivity => ({ id, name, source: "agent", ...(phase ? { phase } : {}) });
const shape = (items: TranscriptItem[]) => items.map((i) => (i.type === "event" ? `${i.event.kind}:${i.event.text}` : `[${i.group.key} ${i.group.steps.length}]`));
const groups = (items: TranscriptItem[]) => items.filter((i) => i.type === "group").map((i) => (i as { group: ActivityGroup }).group);

test("one turn folds into one group at its first event; people and the reply stay top-level", () => {
  const items = groupTranscript([
    ev("message", "alice", "@agent list it"),
    ev("activity", "agent", "", run("r1", "Metro", "started")),
    ev("tool_call", "agent", 'workspace.list {"path":"."}', run("r1")),
    ev("message", "bob", "meanwhile"),
    ev("tool_result", "runner", "workspace.list ok\nREADME.md", run("r1")),
    ev("activity", "agent", "", run("r1", "Metro", "completed")),
    ev("agent", "agent", "Done.", run("r1")),
  ]);
  expect(shape(items)).toEqual(["message:@agent list it", "[agent:r1 2]", "message:meanwhile", "agent:Done."]);
  const [g] = groups(items);
  expect(g!.outcome).toBe("completed");
  expect(groupTitle(g!)).toBe("Metro’s activity");
  expect(groupOutcomeText(g!)).toBe(null);
  expect(g!.steps.map((s) => s.activity.summary)).toEqual(["Tool · List workspace · called", "Tool · List workspace · complete"]);
});

test("a live turn says it is thinking, even before its first tool", () => {
  const items = groupTranscript([ev("activity", "agent", "", run("r2", "Scout", "started"))]);
  const [g] = groups(items);
  expect(g!.outcome).toBe("live");
  expect(g!.steps).toEqual([]);
  expect(groupTitle(g!)).toBe("Scout is thinking…");
  expect(groupAccessibleLabel(g!)).toBe("Scout is thinking…, in progress");
});

test("keys and positions don't move as later steps arrive", () => {
  const a = [ev("activity", "agent", "", run("r3", "Metro", "started")), ev("tool_call", "agent", "workspace.read {}", run("r3")), ev("message", "bob", "hi")];
  const first = groupTranscript(a);
  const later = groupTranscript([...a, ev("tool_result", "runner", "workspace.read ok\nx", run("r3")), ev("message", "carol", "yo")]);
  expect(first.map((i) => i.key)).toEqual(later.slice(0, first.length).map((i) => i.key));
  expect(shape(later)).toEqual(["[agent:r3 2]", "message:hi", "message:yo"]);
});

test("interleaved runs with the same display name stay apart", () => {
  const items = groupTranscript([
    ev("activity", "agent", "", run("a", "Metro", "started")),
    ev("activity", "agent", "", run("b", "Metro", "started")),
    ev("tool_call", "agent", "workspace.list {}", run("b")),
    ev("tool_call", "agent", "workspace.read {}", run("a")),
    ev("activity", "agent", "", run("b", "Metro", "completed")),
  ]);
  expect(shape(items)).toEqual(["[agent:a 1]", "[agent:b 1]"]);
  const [a, b] = groups(items);
  expect(groupTitle(a!)).toBe("Metro is thinking…");
  expect(groupTitle(b!)).toBe("Metro’s activity");
});

test("failed, cancelled and interrupted turns are never shown as ongoing", () => {
  const failedTool = groups(groupTranscript([ev("activity", "agent", "", run("f", "Metro", "started")), ev("tool_result", "runner", "workspace.check failed\nexit 1", run("f"))]))[0]!;
  expect([failedTool.outcome, groupOutcomeText(failedTool)]).toEqual(["live", "failed"]);
  expect(groupOutcomeText({ ...failedTool, outcome: "completed" })).toBe("failed");
  const failed = groups(groupTranscript([ev("activity", "agent", "", run("g", "Metro", "started")), ev("activity", "agent", "model unavailable", run("g", "Metro", "failed"))]))[0]!;
  expect([failed.outcome, groupTitle(failed), groupOutcomeText(failed)]).toEqual(["failed", "Metro’s activity", "failed"]);
  expect(failed.steps.map((s) => [s.activity.summary, s.activity.details])).toEqual([["Failed", "model unavailable"]]);
  const cancelled = groups(groupTranscript([ev("activity", "agent", "", run("c", "Metro", "started")), ev("activity", "agent", "", run("c", "Metro", "cancelled"))]))[0]!;
  expect([groupTitle(cancelled), groupOutcomeText(cancelled)]).toEqual(["Metro’s activity", "cancelled"]);
  const interrupted = groups(groupTranscript([ev("activity", "agent", "", run("i", "Metro", "started")), ev("activity", "agent", "", run("i", "Metro", "interrupted"))]))[0]!;
  expect(groupOutcomeText(interrupted)).toBe("interrupted");
  // An error or status for the turn ends it, and an error marks it failed.
  const errored = groups(groupTranscript([ev("activity", "agent", "", run("e", "Metro", "started")), ev("tool_call", "agent", "workspace.list {}", run("e")), ev("error", "system", "runner offline", run("e"))]))[0]!;
  expect([errored.outcome, groupTitle(errored)]).toEqual(["failed", "Metro’s activity"]);
  const stopped = groups(groupTranscript([ev("activity", "agent", "", run("s", "Metro", "started")), ev("tool_call", "agent", "workspace.list {}", run("s")), ev("status", "system", "cancelled", run("s"))]))[0]!;
  expect(groupTitle(stopped)).toBe("Metro’s activity");
  // Without any lifecycle, tool rows alone don't claim the turn is live.
  expect(groups(groupTranscript([ev("tool_call", "agent", "workspace.list {}", run("t"))]))[0]!.outcome).toBe("ended");
});

test("an ended turn with nothing to inspect leaves no shell", () => {
  const items = groupTranscript([ev("activity", "agent", "", run("x", "Metro", "started")), ev("activity", "agent", "", run("x", "Metro", "completed")), ev("agent", "agent", "Hi", run("x"))]);
  expect(shape(items)).toEqual(["agent:Hi"]);
});

test("Demo is runner activity, never thinking", () => {
  const demo: EventActivity = { id: "d1", name: "alice", source: "demo" };
  const items = groupTranscript([
    ev("demo", "alice", "Demo: workspace.list on the Space's runner", demo),
    ev("activity", "alice", "", { ...demo, phase: "started" }),
    ev("tool_call", "alice", "workspace.list {}", demo),
  ]);
  expect(shape(items)).toEqual(["demo:Demo: workspace.list on the Space's runner", "[demo:d1 1]"]);
  const g = groups(items)[0]!;
  expect([groupTitle(g), groupOutcomeText(g)]).toEqual(["Runner activity", "running"]);
  expect(groupAccessibleLabel(g)).not.toContain("thinking");
  expect(groupAccessibleLabel(g)).not.toContain("alice");
});

test("uncorrelated history stays collapsed one event at a time, never attributed to a person", () => {
  const items = groupTranscript([
    ev("tool_call", "agent", "workspace.list {}"),
    ev("tool_result", "runner", "workspace.list ok\nREADME.md"),
    ev("tool_call", "bob", "workspace.list {}"),
    ev("reasoning_summary", "agent", "Looked first."),
  ]);
  const gs = groups(items);
  expect(gs.map((g) => [g.key.startsWith("event:"), g.correlated, groupTitle(g), g.outcome])).toEqual([
    [true, false, "Metro’s activity", "ended"],
    [true, false, "Runner activity", "ended"],
    [true, false, "Runner activity", "ended"],
    [true, false, "Metro’s activity", "ended"],
  ]);
  expect(groupOutcomeText(groups(groupTranscript([ev("tool_result", "runner", "workspace.check failed\nexit 1")]))[0]!)).toBe("failed");
});

test("activity metadata is read strictly", () => {
  expect(toActivity({ id: "r1", name: " Metro ", source: "agent", phase: "started" })).toEqual({ id: "r1", name: "Metro", source: "agent", phase: "started" });
  expect(toActivity({ id: 7, source: "demo" })).toEqual({ id: "7", name: "", source: "demo" });
  expect(toActivity({ id: "r1", name: "x", source: "agent", phase: "exploded" })).toEqual({ id: "r1", name: "x", source: "agent" });
  for (const bad of [null, "r1", { id: "", source: "agent" }, { id: "r1", source: "human" }, { name: "x", source: "agent" }]) expect(toActivity(bad)).toBe(undefined);
  // A turn without a name falls back to the built-in agent's.
  expect(groupTitle(groups(groupTranscript([ev("activity", "agent", "", { id: "n", name: "", source: "agent", phase: "started" })]))[0]!)).toBe("Metro is thinking…");
});

const route = (over: Partial<import("../../api/client").ActivityRoute>): import("../../api/client").ActivityRoute => ({
  source: "case", addressedBy: "classifier", caseLabel: "Code review", providerId: null, modelId: null, reason: 'System One matched the use case "Code review" (p 0.91)', ...over,
});

test("route attribution: a compact badge, and the full sentence in words, never the raw p", () => {
  expect(routeConfidence(route({}))).toBe(91);
  expect(routeBadge(route({}))).toBe("Routed: Code review · 91%");
  expect(routeBadge(route({ reason: 'System One matched the use case "Code review" (p 1.00)' }))).toBe("Routed: Code review · 100%");
  expect(routeBadge(route({ reason: "picked" }))).toBe("Routed: Code review"); // no probability given
  expect(routeBadge(route({ source: "override", caseLabel: null, addressedBy: "explicit" }))).toBe("Routed: chosen model");
  expect(routeBadge(route({ source: "default", caseLabel: null }))).toBe("Started by System One");
  expect(routeBadge(route({ source: "default", caseLabel: null, addressedBy: "schedule" }))).toBe("Scheduled");
  expect(routeBadge(route({ source: "default", caseLabel: null, addressedBy: "explicit" }))).toBe(null);

  const sentence = routeText(route({ reason: 'System One matched the use case "Code review" (p 0.91).' }));
  expect(sentence).toBe('Use case “Code review” answered: System One matched the use case "Code review" with 91% probability. System One judged that the message asked the agent; nobody typed @agent.');
  // An out-of-range probability is dropped, not shown.
  expect(routeConfidence(route({ reason: "matched (p 7.5)" }))).toBe(null);
  expect(routeText(route({ reason: "matched (p 7.5)" }))).not.toContain("p 7.5");
});

test("the timeline pairs each tool call with its result; thoughts and notes keep their place", () => {
  const [g] = groups(groupTranscript([
    ev("activity", "agent", "", run("t", "Metro", "started")),
    ev("tool_call", "agent", 'workspace.read {"path":"README.md"}', run("t")),
    ev("reasoning_summary", "agent", "Read the README first.", run("t")),
    ev("tool_result", "runner", "workspace.read ok\n# hi", run("t")),
    ev("tool_call", "agent", 'workspace.check {"name":"tests"}', run("t")),
    ev("tool_result", "runner", "workspace.check failed\nexit 1", run("t")),
    ev("tool_result", "runner", "workspace.list ok\norphan", run("t")),
  ]));
  const items = timeline(g!);
  expect(items.map((i) => i.type)).toEqual(["tool", "thought", "tool", "tool"]);
  const [read, , check, orphan] = items as Extract<ReturnType<typeof timeline>[number], { type: "tool" }>[];
  expect([read!.name, read!.args, read!.result?.activity.details, read!.failed]).toEqual(["Read file", "README.md", "# hi", false]);
  expect([check!.name, check!.args, check!.failed]).toEqual(["Run check", "tests", true]);
  expect([orphan!.call, orphan!.name]).toEqual([null, "List workspace"]);
  expect(toolCount(g!)).toBe(2);
});

test("short arguments, the live step, and the summary's parts", () => {
  expect(shortArgs('{"path":"src/lib.rs"}')).toBe("src/lib.rs");
  expect(shortArgs("{}")).toBe("");
  expect(shortArgs('{"a":1,"b":"two"}')).toBe("1 two");
  expect(shortArgs("x".repeat(80))).toHaveLength(60);
  const live = (text: string) => groups(groupTranscript([ev("activity", "agent", "", run("l", "Scout", "started")), ev("tool_call", "agent", text, run("l"))]))[0]!;
  expect(liveTitle(live('workspace.read {"path":"a.md"}'))).toBe("Scout · Reading a.md…");
  expect(liveTitle(live('workspace.list {"path":"."}'))).toBe("Scout · Listing the workspace…");
  expect(liveTitle(live("web_search {}"))).toBe("Scout · Using Web search…");
  expect(currentStep(groups(groupTranscript([ev("activity", "agent", "", run("e", "Scout", "started"))]))[0]!)).toBe("Scout is thinking…");
  const g = groups(groupTranscript([
    ev("activity", "agent", "", run("c", "Metro", "started")),
    ev("tool_call", "agent", "workspace.list {}", run("c")),
    ev("activity", "agent", "", run("c", "Metro", "cancelled")),
  ]))[0]!;
  expect(summaryParts(g)).toEqual({ pick: null, duration: null, tools: "1 tool", route: null, stopped: "cancelled" });
  // How a Space turn's agent was chosen: on the collapsed line only when it says more than the name.
  expect(summaryParts({ ...g, pick: "Picked by classifier: Coder" }).pick).toBe("Picked by classifier: Coder");
  expect(pickBadge("Default agent")).toBe(null);
  expect(pickBadge("Addressed: @coder")).toBe(null);
  expect(pickBadge("Default agent (the classifier was unsure: best Coder at 0.41)")).toBe("Default agent (the classifier was unsure: best Coder at 0.41)");
  // The started marker's agent and pick reach the group.
  const badge = { id: "ag-2", handle: "coder", name: "Coder", color: "#059669" };
  const picked = groups(groupTranscript([
    ev("activity", "agent", "", { ...run("p", "Coder", "started"), agent: badge, pick: "Picked by classifier: Coder" } as any),
    ev("activity", "agent", "", { ...run("p", "Coder", "completed"), agent: badge } as any),
  ]))[0]!;
  expect(picked.agent).toEqual(badge);
  expect(picked.pick).toBe("Picked by classifier: Coder");
  expect(durationText({ ...g, firstMs: 0, lastMs: 12_400 })).toBe("thought 12s");
  expect(durationText({ ...g, firstMs: 0, lastMs: 400 })).toBe(null);
  expect(durationText({ ...g, outcome: "live", firstMs: 0, lastMs: 9_000 })).toBe(null);
});
