// The Space API's parsing and the client-side checks behind the live hints.

import { expect, test } from "nsh/test";
import { handleProblem, initialOf, parseAgent, parsePrefs, prefsBody, suggestHandle } from "../../api/spaces";
import { NO_CHOICE, selectionOf, selectionValue, spaceModelOptions, unlisted } from "./modelOptions";

test("agents parse from the server's shape; a missing initial or colour is filled in", () => {
  const a = parseAgent({ id: "ag-1", space_id: "acme", name: "  ünder", handle: "u", description: "d", instructions: "", tools: ["threads", 3], is_default: true, avatar_version: null, color: "", created_at: "x", updated_at: "x" });
  expect(a).toEqual({ id: "ag-1", spaceId: "acme", name: "  ünder", handle: "u", description: "d", instructions: "", tools: ["threads"], isDefault: true, avatarVersion: null, color: "#4f46e5", initial: "Ü" });
  expect(parseAgent({ id: "", name: "x", handle: "x" })).toBe(null);
  expect(initialOf("--")).toBe("?");
});

test("handles: lowercase letters, digits, - and _; @agent reserved except for the agent that has it; unique", () => {
  expect(handleProblem("", [])).toContain("Enter");
  expect(handleProblem("Bad", [])).toContain("lowercase");
  expect(handleProblem("a".repeat(33), [])).toContain("32");
  expect(handleProblem("agent", [])).toContain("reserved");
  expect(handleProblem("agent", [], "agent")).toBe(null);
  expect(handleProblem("writer", ["writer"])).toContain("taken");
  expect(handleProblem("writer", ["writer"], "writer")).toBe(null);
  expect(suggestHandle("Code Reviewer!")).toBe("code-reviewer");
});

test("preferences round-trip in the wire shape; unreadable selections are dropped", () => {
  const p = parsePrefs({ classify_unaddressed: true, default_model: { provider_connection_id: "pc1", model_id: "m" }, agent_models: { "ag-1": { provider_connection_id: "pc2" }, "ag-2": { nope: 1 } }, routing: false, classifier_available: true })!;
  expect(p).toEqual({ classifyUnaddressed: true, defaultModel: { connectionId: "pc1", modelId: "m" }, agentModels: { "ag-1": { connectionId: "pc2" } }, routing: false, classifierAvailable: true });
  expect(prefsBody(p)).toEqual({ classify_unaddressed: true, default_model: { provider_connection_id: "pc1", model_id: "m" }, agent_models: { "ag-1": { provider_connection_id: "pc2" } }, routing: false });
  // Routing defaults on, as the server's.
  expect(parsePrefs({})!.routing).toBe(true);
});

test("model options: grouped chat models of usable connections, never the operator's; saved choices map back", () => {
  const conn = { owned: false, visibility: null, billing: "owner", ready: null, isDefault: false, defaultModel: null };
  const options = spaceModelOptions(
    {
      models: [],
      operatorModel: { modelId: "op", label: "Server model" },
      chatProviders: [
        {
          providerId: "openai",
          providerName: "OpenAI",
          connections: [
            { ...conn, connectionId: null, name: "Server", ownerId: null, runsOn: "operator", operator: true, models: [{ id: "op", displayName: "op", reasoningEfforts: [], isDefault: true, available: true }] },
            { ...conn, connectionId: "pc1", name: "Bob's key", ownerId: "bob", runsOn: "owner_connection", operator: false, models: [{ id: "gpt", displayName: "GPT", reasoningEfforts: [], isDefault: true, available: true }, { id: "old", displayName: "Old", reasoningEfforts: [], isDefault: false, available: false }] },
          ],
        },
      ],
    },
    "fallback",
  );
  expect(options.map((o) => o.value)).toEqual([NO_CHOICE, selectionValue({ connectionId: "pc1", modelId: "gpt" }), selectionValue({ connectionId: "pc1", modelId: "old" })]);
  expect(options[1]!.detail).toContain("they can see these requests and may pay for them");
  expect(options[2]!.disabled).toBe(true);
  expect(selectionOf(options[1]!.value)).toEqual({ connectionId: "pc1", modelId: "gpt" });
  expect(selectionOf(NO_CHOICE)).toBe(null);
  expect(unlisted(options, { connectionId: "gone", modelId: "x" })).toBe(true);
  expect(unlisted(options, null)).toBe(false);
});
