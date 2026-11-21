import { expect, test } from "nsh/test";
import { kindLabel, toneForEvent } from "./eventKinds";

test("maps event kinds and actors to tones", () => {
  expect(toneForEvent("message", "alice")).toBe("neutral");
  expect(toneForEvent("agent_message", "agent")).toBe("agent");
  expect(toneForEvent("message", "agent")).toBe("agent");
  expect(toneForEvent("runner.result", "runner")).toBe("runner");
  expect(toneForEvent("tool_call", "agent")).toBe("runner");
  expect(toneForEvent("demo", "system")).toBe("runner");
  expect(toneForEvent("system", "metro")).toBe("system");
  expect(toneForEvent("runner.error", "runner")).toBe("danger");
  expect(toneForEvent("something.new", "x")).toBe("neutral");
});

test("labels kinds readably", () => {
  expect(kindLabel("agent_message")).toBe("agent message");
  expect(kindLabel("")).toBe("event");
});

import { audienceText, mentionsMe, mentionText, reactionSummary } from "./eventKinds";
import { roomTree } from "./Sidebar";

test("summarizes reactions: mine, others, the agent", () => {
  expect(reactionSummary(["user:alice", "user:bob", "agent:agent"], "alice")).toEqual({
    count: 2,
    mine: true,
    agent: true,
    description: "reacted by you, bob and the agent",
  });
  expect(reactionSummary(["agent:agent"], "alice")).toMatchObject({ count: 0, mine: false, agent: true });
});

test("mentions and audiences read naturally", () => {
  expect(mentionText({ kind: "user", id: "alice" }, "alice")).toBe("@you");
  expect(mentionText({ kind: "user", id: "bob" }, "alice")).toBe("@bob");
  expect(mentionText({ kind: "agent" }, "alice")).toBe("@agent");
  expect(audienceText("room")).toBe(null);
  expect(audienceText("mixed")).toBe("to the agent and people");
  expect(mentionsMe([{ kind: "agent" }], "alice")).toBe(false);
});

test("nests threads under their parent and survives bad parents", () => {
  const rows = roomTree([
    { id: "lobby", name: "lobby" },
    { id: "t2", name: "child of t1", parentRoomId: "t1" },
    { id: "t1", name: "thread", parentRoomId: "lobby" },
    { id: "ops", name: "ops" },
    { id: "x", name: "orphan", parentRoomId: "missing" },
    { id: "a", name: "cycle a", parentRoomId: "b" },
    { id: "b", name: "cycle b", parentRoomId: "a" },
  ]);
  expect(rows.map((r) => `${r.room.id}:${r.depth}`)).toEqual(["lobby:1", "t1:2", "t2:3", "ops:1", "x:1", "a:1", "b:2"]);
});
