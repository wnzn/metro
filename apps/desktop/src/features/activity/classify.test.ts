import { expect, test } from "nsh/test";
import { activityOf, toolName } from "./classify";

test("tool calls and results become collapsed activity with friendly labels", () => {
  expect(activityOf({ kind: "tool_call", text: 'workspace.list {"path":"."}' })).toEqual({
    kind: "tool_call",
    summary: "Tool · List workspace · called",
    tone: "runner",
    details: 'workspace.list {"path":"."}',
    mono: true,
  });
  expect(activityOf({ kind: "tool_result", text: "workspace.list ok\nREADME.md\nsrc/\n" })).toMatchObject({
    summary: "Tool · List workspace · complete",
    tone: "runner",
    details: "README.md\nsrc/",
  });
  expect(activityOf({ kind: "tool_result", text: "workspace.check failed\nexit 1" })).toMatchObject({
    summary: "Tool · Run check · failed",
    tone: "danger",
    details: "exit 1",
  });
  // No server header: still hidden, labelled only as what it is.
  expect(activityOf({ kind: "tool_result", text: "README.md\n" })).toMatchObject({ summary: "Tool · output", details: "README.md" });
  expect(activityOf({ kind: "tool_result", text: "workspace.list ok" })).toMatchObject({ details: "" });
});

test("only events labelled as thinking are treated as thinking", () => {
  expect(activityOf({ kind: "reasoning_summary", text: "Checked the listing first." })).toMatchObject({
    kind: "thinking",
    summary: "Thinking · summary shared by the model provider",
    details: "Checked the listing first.",
  });
  expect(activityOf({ kind: "agent.progress", text: "still listing" })).toMatchObject({ kind: "progress", summary: "Agent · working" });
});

test("people, replies, errors, status, threads and Demo stay visible", () => {
  for (const kind of ["message", "agent", "error", "status", "thread", "demo", "reaction", "runner.error", "tool_error", "something.new"]) {
    expect(activityOf({ kind, text: "x" })).toBe(null);
  }
});

test("tool names read naturally, unknown ones are spelled out", () => {
  expect(toolName("workspace.mcp_list")).toBe("List MCP tools");
  expect(toolName("workspace_read")).toBe("Read file");
  expect(toolName("git.status")).toBe("Git status");
});
