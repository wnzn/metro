import { expect, test } from "nsh/test";
import type { AvailableModel } from "../../api/client";
import { createComposerCatalog } from "./catalog";

const ok = () => true;
const model = (extra: Partial<AvailableModel> = {}): AvailableModel => ({
  connectionId: "c", name: "Work", providerId: "openai", model: "legacy", capability: "chat",
  visibility: "personal", owned: true, ownerId: "alice", execution: "server", runsOn: "your_connection",
  billing: "you", ready: null, isDefault: false, ...extra,
});
const models = [{ id: "a", reasoningEfforts: ["low", "high"] }, { id: "b", reasoningEfforts: ["medium"] }];
const shared = model({ owned: false, ownerId: "bob", runsOn: "owner_connection", visibility: "spaces", billing: "owner" });

test("model IDs and advertised levels stay paired even on the same connection", async () => {
  const c = createComposerCatalog();
  await c.load(null, ok, async () => [model({ chatModels: models })]);
  const s = c.current(null)!;
  const [a, b] = s.targets;
  expect(s.targets.map(t => t.levels)).toEqual([["low", "high"], ["medium"]]);
  c.select(s, a.key, "high");
  expect(c.choice(null).choice).toEqual({ connectionId: "c", modelId: "a", reasoningEffort: "high" });
  c.select(c.current(null)!, b.key, "high");
  expect(c.choice(null).choice?.modelId).toBe("a");
  c.select(c.current(null)!, b.key);
  expect(c.choice(null).choice).toEqual({ connectionId: "c", modelId: "b" });
  c.select(c.current(null)!, null);
  expect(c.choice(null)).toEqual({});
});

test("absent metadata selects just the connection; empty, unready and non-chat targets are excluded", async () => {
  const c = createComposerCatalog();
  await c.load(null, ok, async () => [
    model(), model({ chatModels: [] }), model({ capability: "embeddings" }),
    model({ capability: "system_one", ready: false }), model({ ready: false }),
  ]);
  const s = c.current(null)!;
  expect(s.targets).toHaveLength(1);
  c.select(s, s.targets[0].key);
  expect(c.choice(null)).toEqual({ choice: { connectionId: "c" } });
  expect(c.knownNoSystemOne(null)).toBe(true);
  await c.load(null, ok, async () => [model(), model({ capability: "system_one" })]);
  expect(c.knownNoSystemOne(null)).toBe(false);
});

test("a stale pin blocks submission during refresh, after removal or failure; no auto selection", async () => {
  const c = createComposerCatalog();
  await c.load(null, ok, async () => [model({ chatModels: models })]);
  const s = c.current(null)!;
  c.select(s, s.targets[0].key, "high");
  const reloading = c.load(null, ok, async () => [model({ chatModels: [{ id: "a", reasoningEfforts: [] }] })]);
  expect(c.choice(null).error).toBeTruthy();
  expect(c.knownNoSystemOne(null)).toBe(false);
  await reloading;
  expect(c.choice(null).error).toBeTruthy();
  expect(c.knownNoSystemOne(null)).toBe(false);
  await c.load(null, ok, async () => { throw new Error("offline"); });
  expect(c.choice(null).error).toBeTruthy();
  expect(c.knownNoSystemOne(null)).toBe(false);
});

test("Space shares, pins and late responses cannot cross scopes or survive sign-out", async () => {
  const c = createComposerCatalog();
  await c.load("space-a", ok, async () => [shared]);
  const s = c.current("space-a")!;
  expect(s.targets[0].detail).toContain("bob can see these requests and may pay");
  c.select(s, s.targets[0].key);
  const stale = c.load("space-a", ok, async () => [shared]);
  await c.load(null, ok, async () => [shared, model()]);
  await stale;
  expect(c.choice(null)).toEqual({});
  expect(c.current(null)!.targets).toHaveLength(1);
  expect(c.current(null)!.targets[0].detail).toBeUndefined();
  c.select(s, s.targets[0].key);
  expect(c.choice(null)).toEqual({});
  await c.load("space-b", ok, async () => []);
  expect(c.knownNoSystemOne("space-b")).toBe(false);
  const pending = c.load("space-b", ok, async () => [shared]);
  c.clear();
  await pending;
  expect(c.current("space-b")).toBeUndefined();
});
