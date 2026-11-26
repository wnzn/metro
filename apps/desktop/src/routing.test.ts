// The routing draft's rules, without the UI: thresholds, stable keys, and
// that only a usable model of the right capability can be saved.

import { expect, test } from "nsh/test";
import type { AvailableModel } from "./api/client";
import { caseKey, finalize, parseProbability, toDraft } from "./routing";
import type { RoutingDraft } from "./routing";

const model = (id: string, capability: AvailableModel["capability"], owned = true, m: string | null = "m1"): AvailableModel => ({
  connectionId: id,
  name: id,
  providerId: "p",
  model: m,
  capability,
  visibility: owned ? "personal" : "spaces",
  owned,
  ownerId: owned ? "me" : "bob",
  execution: "server",
  runsOn: owned ? "your_connection" : "owner_connection",
  billing: owned ? "you" : "owner",
  ready: null,
  isDefault: false,
});
const usable = [model("chat1", "chat"), model("chat2", "chat", false), model("s1", "system_one"), model("e1", "embeddings")];
const base = (): RoutingDraft => ({
  enabled: false,
  inferAddressing: false,
  classifierId: null,
  embeddingId: null,
  defaultChatId: null,
  minDirectedness: "0.35",
  minRouteProbability: "0.65",
  cases: [],
});

test("thresholds are probabilities from 0 to 1, as decimals or percentages", () => {
  expect(parseProbability("0.7")).toBe(0.7);
  expect(parseProbability(".7")).toBe(0.7);
  expect(parseProbability("70%")).toBe(0.7);
  expect(parseProbability("0")).toBe(0);
  expect(parseProbability("1")).toBe(1);
  expect(parseProbability("1.2")).toBe(null);
  expect(parseProbability("-0.1")).toBe(null);
  expect(parseProbability("")).toBe(null);
  expect(parseProbability("high")).toBe(null);
});

test("a threshold that can't be saved is explained in percent; the values stay 0 to 1", () => {
  expect(finalize({ ...base(), minRouteProbability: "high" }, usable)).toEqual({ ok: false, error: "The routing confidence must be a whole number from 0 to 100%, such as 65%." });
  expect(finalize({ ...base(), minDirectedness: "0.19" }, usable)).toEqual({ ok: false, error: "Space classification confidence must be a whole number from 20 to 100%, such as 35%." });
  expect(finalizeOk({ ...base(), minRouteProbability: "70%", minDirectedness: "0.9" })).toMatchObject({ minRouteProbability: 0.7, minDirectedness: 0.9 });
});

test("new use cases get a key from their name, unique, and existing keys never change", () => {
  expect(caseKey("Code review", new Set())).toBe("code_review");
  expect(caseKey("Code review", new Set(["code_review"]))).toBe("code_review_2");
  expect(caseKey("¿¿", new Set())).toBe("case");
  expect(caseKey("2nd opinion", new Set())).toBe("case_2nd_opinion"); // keys start with a letter
  const long = caseKey("a very long use case name that goes on and on", new Set());
  expect(long.length).toBe(32);
  expect(/^[a-z][a-z0-9_]{0,31}$/.test(caseKey("a very long use case name that goes on and on", new Set([long])))).toBe(true);
  const d = toDraft({ ...finalizeOk({ ...base() }), cases: [{ key: "kept", label: "Old name", description: "", connectionId: "chat1" }] });
  d.cases[0].label = "Renamed";
  d.cases.push({ uid: "n", key: null, label: "Kept", description: "", connectionId: "chat2" });
  const done = finalize(d, usable);
  expect(done.ok).toBe(true);
  if (done.ok) expect(done.routing.cases.map((c) => c.key)).toEqual(["kept", "kept_2"]);
});

test("the server's bounds: asking-the-agent at least 0.2, at most 16 use cases", () => {
  expect(finalize({ ...base(), minDirectedness: "19%" }, usable).ok).toBe(false);
  expect(finalizeOk({ ...base(), minDirectedness: "20%" }).minDirectedness).toBe(0.2);
  const many = Array.from({ length: 17 }, (_, i) => ({ uid: `u${i}`, key: null, label: `Case ${i}`, description: "", connectionId: "chat1" }));
  expect(finalize({ ...base(), cases: many }, usable).ok).toBe(false);
  expect(finalize({ ...base(), cases: many.slice(0, 16) }, usable).ok).toBe(true);
});

test("everything is off by default and saves with no classifier", () => {
  const done = finalize(base(), usable);
  expect(done).toMatchObject({ ok: true, routing: { enabled: false, inferAddressing: false, classifierId: null, cases: [] } });
});

test("routing needs a System One classifier and a use case; the legacy flag is ignored", () => {
  expect(finalize({ ...base(), enabled: true }, usable)).toMatchObject({ ok: false, error: "Choose a System One classifier to use model routing." });
  expect(finalize({ ...base(), inferAddressing: true }, usable)).toMatchObject({ ok: true, routing: { inferAddressing: true } });
  expect(finalize({ ...base(), inferAddressing: true, classifierId: "s1" }, usable).ok).toBe(true);
  expect(finalize({ ...base(), enabled: true, classifierId: "s1" }, usable)).toMatchObject({ ok: false, error: "Add at least one use case, or turn use-case routing off." });
});

test("a picker's value must be a usable model of its own capability", () => {
  // A chat model never stands in for System One, nor an embeddings model for chat.
  expect(finalize({ ...base(), classifierId: "chat1" }, usable).ok).toBe(false);
  expect(finalize({ ...base(), embeddingId: "s1" }, usable).ok).toBe(false);
  expect(finalize({ ...base(), defaultChatId: "e1" }, usable).ok).toBe(false);
  // Gone (deleted or unshared) is refused too.
  expect(finalize({ ...base(), classifierId: "s-gone" }, usable).ok).toBe(false);
  const withCase = (connectionId: string | null) => ({ ...base(), cases: [{ uid: "a", key: null, label: "Quick", description: "", connectionId }] });
  expect(finalize(withCase("e1"), usable).ok).toBe(false);
  expect(finalize(withCase(null), usable)).toMatchObject({ ok: false, error: "Choose which model answers “Quick”." });
  expect(finalize(withCase("chat2"), usable).ok).toBe(true); // shared with me is fine
  // A preset I have no connection of my own for, or a connection with no model, can't answer.
  expect(finalize(withCase("p1"), [...usable, { ...model("p1", "chat", false), runsOn: "your_own_sign_in", ready: false }]).ok).toBe(false);
  expect(finalize(withCase("p1"), [...usable, { ...model("p1", "chat", false), runsOn: "your_own_sign_in", ready: true }]).ok).toBe(true);
  expect(finalize(withCase("n1"), [...usable, model("n1", "chat", true, null)]).ok).toBe(false);
});

test("use cases need distinct names", () => {
  const d = { ...base(), cases: [
    { uid: "a", key: null, label: "Writing", description: "", connectionId: "chat1" },
    { uid: "b", key: null, label: " writing ", description: "", connectionId: "chat2" },
  ] };
  expect(finalize(d, usable)).toMatchObject({ ok: false });
  expect(finalize({ ...d, cases: [{ ...d.cases[0], label: "  " }] }, usable)).toMatchObject({ ok: false, error: "Every use case needs a name." });
});

function finalizeOk(d: RoutingDraft) {
  const done = finalize(d, usable);
  if (!done.ok) throw new Error(done.error);
  return done.routing;
}
