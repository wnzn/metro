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

test("targets group by provider under canonical names and marks; custom endpoints fall back to lettermarks", async () => {
  const c = createComposerCatalog();
  const rows = [
    model({ connectionId: "local", name: "Laptop", providerId: "device-openai-compatible", chatModels: [{ id: "qwen", reasoningEfforts: [] }] }),
    model({ connectionId: "ds", name: "DS", providerId: "deepseek", chatModels: [{ id: "r1", reasoningEfforts: [] }] }),
    model({ connectionId: "oauth", name: "ChatGPT", providerId: "device-openai-oauth", chatModels: [{ id: "gpt-x", reasoningEfforts: ["low", "high"] }] }),
    model({ connectionId: "claude", name: "Claude", providerId: "anthropic", chatModels: [{ id: "opus", reasoningEfforts: [] }] }),
    model({ connectionId: "api", name: "Work", providerId: "openai", chatModels: [{ id: "gpt-y", reasoningEfforts: [] }] }),
    model({ connectionId: "mystery", name: "Lab", providerId: "acme-llm", chatModels: [{ id: "m", reasoningEfforts: [] }] }),
  ];
  await c.load(null, ok, async () => rows, async () => [{ id: "deepseek", name: "DeepSeek" }]);
  const t = c.current(null)!.targets;
  expect(t.map(x => [x.provider.name, x.model, x.connection])).toEqual([
    ["OpenAI", "gpt-x", "ChatGPT"], ["OpenAI", "gpt-y", "Work"], ["Anthropic", "opus", "Claude"],
    ["acme-llm", "m", "Lab"], ["DeepSeek", "r1", "DS"], ["Custom & self-hosted", "qwen", "Laptop"],
  ]);
  // Same provider, two connections: identities and levels stay with their own connection.
  expect(t[0].choice).toEqual({ connectionId: "oauth", modelId: "gpt-x" });
  expect(t[0].levels).toEqual(["low", "high"]);
  expect(t[1].choice).toEqual({ connectionId: "api", modelId: "gpt-y" });
  expect(t[1].levels).toEqual([]);
  expect(t[0].provider.mark.logo).toBe("openai");
  expect(t[4].provider.mark.logo).toBe("deepseek");
  expect(t[3].provider.mark.logo).toBeUndefined();
  expect(t[5].provider.mark.logo).toBeUndefined();
  expect(t[5].provider.mark.letters).toBe("{}");
  expect(t[0].label).toBe("gpt-x · ChatGPT · OpenAI");
  // A failed provider catalog only loses the friendly name; every choice stays.
  await c.load(null, ok, async () => rows, async () => { throw new Error("offline"); });
  const again = c.current(null)!;
  expect(again.phase).toBe("ready");
  expect(again.targets.find(x => x.choice.connectionId === "ds")!.provider.name).toBe("deepseek");
  expect(again.targets).toHaveLength(6);
});

function deferred<T>() {
  let resolve!: (v: T) => void;
  const promise = new Promise<T>(r => { resolve = r; });
  return { promise, resolve };
}

test("optional provider names never block rows; late names rename in place only while current", async () => {
  const c = createComposerCatalog();
  const rows = [model({ connectionId: "ds", name: "DS", providerId: "deepseek", chatModels: [{ id: "r1", reasoningEfforts: ["low", "high"] }] })];
  const slow = deferred<{ id: string; name: string }[]>();
  await c.load(null, ok, async () => rows, () => slow.promise);
  const s = c.current(null)!;
  expect(s.phase).toBe("ready");
  expect(s.targets[0].provider.name).toBe("deepseek");
  c.select(s, s.targets[0].key, "high");
  const pinned = c.current(null)!;
  // An Ask can already use the choice while the names are still pending.
  expect(c.choice(null)).toEqual({ choice: { connectionId: "ds", modelId: "r1", reasoningEffort: "high" } });
  expect(c.knownNoSystemOne(null)).toBe(true);
  slow.resolve([{ id: "deepseek", name: "DeepSeek" }]);
  await slow.promise;
  await Promise.resolve();
  // Same snapshot (open picker, in-progress fetch stay valid), renamed, pin and effort kept.
  expect(c.current(null)).toBe(pinned);
  expect(pinned.targets[0].provider.name).toBe("DeepSeek");
  expect(pinned.targets[0].label).toBe("r1 · DS · DeepSeek");
  expect(c.choice(null)).toEqual({ choice: { connectionId: "ds", modelId: "r1", reasoningEffort: "high" } });

  // A scope switch before the names arrive: the late names never touch the new scope.
  const late = deferred<{ id: string; name: string }[]>();
  await c.load(null, ok, async () => rows, () => late.promise);
  await c.load("space-a", ok, async () => rows, async () => { throw new Error("offline"); });
  late.resolve([{ id: "deepseek", name: "Stale" }]);
  await late.promise;
  await Promise.resolve();
  expect(c.current("space-a")!.targets[0].provider.name).toBe("deepseek");
  expect(c.current("space-a")!.phase).toBe("ready");

  // Names that never arrive leave rows usable; clearing drops them.
  const never = deferred<{ id: string; name: string }[]>();
  await c.load(null, ok, async () => rows, () => never.promise);
  expect(c.current(null)!.targets).toHaveLength(1);
  c.clear();
  never.resolve([{ id: "deepseek", name: "Gone" }]);
  await never.promise;
  await Promise.resolve();
  expect(c.current(null)).toBeUndefined();
});
