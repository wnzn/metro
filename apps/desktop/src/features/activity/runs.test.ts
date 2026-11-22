import { expect, test } from "nsh/test";
import { runStatusLabel, runTitle, runTone } from "./runs";

test("a run's title drops the @agent addressing and keeps the task", () => {
  expect(runTitle("@agent please delegate a workspace audit to a thread")).toBe("Please delegate a workspace audit to a thread");
  expect(runTitle("@agent what is in the workspace?")).toBe("What is in the workspace?");
  expect(runTitle("@agent, list the files")).toBe("List the files");
  expect(runTitle("hey @agent: check the build")).toBe("Hey check the build");
  expect(runTitle("List the workspace and report the files.")).toBe("List the workspace and report the files.");
});

test("only the agent mention as a whole word is removed", () => {
  expect(runTitle("email me@agent.dev about @agents")).toBe("Email me@agent.dev about @agents");
  expect(runTitle("ask @bob and @agent")).toBe("Ask @bob and");
  // Nothing but the mention: the task stays as it was, never an empty title.
  expect(runTitle("@agent")).toBe("@agent");
});

test("status tones and labels", () => {
  expect(runTone("running")).toBe("runner");
  expect(runTone("in_progress")).toBe("runner");
  expect(runTone("completed")).toBe("success");
  expect(runTone("failed")).toBe("danger");
  expect(runTone("stopped")).toBe("warning");
  expect(runTone("whatever")).toBe("neutral");
  expect(runStatusLabel("in_progress")).toBe("in progress");
  expect(runStatusLabel("stopped")).toBe("stopped");
});
