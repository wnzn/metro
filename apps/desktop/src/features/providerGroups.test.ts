// Grouping the server's provider catalog by provider: pure functions, so
// plain unit tests. The catalog entries mirror the server's real ids
// (crates/server/src/providers/catalog.rs).

import { expect, test } from "nsh/test";
import type { CatalogProvider } from "../settings";
import { CAPABILITY_TONE, describePath, groupCatalog, matchesQuery, methodDetail, pickerGroups, vendorOf, wayLabel } from "./providerGroups";

function entry(id: string, name: string, more: Partial<CatalogProvider> = {}): CatalogProvider {
  const device = id.startsWith("device-");
  return {
    id,
    name,
    requiresKey: !device,
    allowCustomUrl: false,
    defaultBaseUrl: device ? null : "https://example.invalid/v1",
    execution: device ? "device" : "server",
    description: null,
    capability: "chat",
    listsModels: true,
    ...more,
  };
}

const CATALOG: CatalogProvider[] = [
  entry("venice", "Venice"),
  entry("openai", "OpenAI (OpenAI Responses)"),
  entry("openai/anthropic", "OpenAI (Anthropic Messages)"),
  entry("deepseek", "DeepSeek (OpenAI Chat Completions)"),
  entry("deepseek/anthropic", "DeepSeek (Anthropic Messages)"),
  entry("anthropic", "Anthropic"),
  entry("custom", "Custom OpenAI-compatible endpoint", { requiresKey: false, allowCustomUrl: true, defaultBaseUrl: null }),
  entry("device-openai-oauth", "Sign in with ChatGPT (on your device)"),
  entry("device-claude-code", "Claude (local CLI)"),
  entry("device-openai-compatible", "OpenAI-compatible endpoint (on your device)"),
  entry("systemone", "TypeSafe System One", { capability: "system_one" }),
  entry("device-systemone", "System One (on your device)", { capability: "system_one" }),
  entry("openai-embeddings", "OpenAI embeddings", { capability: "embeddings" }),
  entry("custom-embeddings", "OpenAI-compatible embeddings endpoint", { capability: "embeddings", requiresKey: false, allowCustomUrl: true, defaultBaseUrl: null }),
  entry("embeddinggemma-2", "EmbeddingGemma 2 on an approved endpoint", { capability: "embeddings", requiresKey: false, allowCustomUrl: true, defaultBaseUrl: null }),
  entry("device-embeddinggemma-2", "EmbeddingGemma 2 (on your device)", { capability: "embeddings" }),
];

test("OpenAI is one provider: Sign in with ChatGPT, API keys in both API families, and embeddings", () => {
  const groups = groupCatalog(CATALOG);
  const openai = groups.find((g) => g.key === "openai")!;
  expect(openai.name).toBe("OpenAI");
  // Device first (preferred), then by capability.
  expect(openai.paths.map((p) => p.id)).toEqual(["device-openai-oauth", "openai/anthropic", "openai", "openai-embeddings"]);
  expect(openai.capabilities).toEqual(["chat", "embeddings"]);
  expect(groups.filter((g) => g.paths.some((p) => p.id.startsWith("openai") || p.id === "device-openai-oauth"))).toHaveLength(1);
});

test("Anthropic is one provider: Claude Code on your device and an API key", () => {
  const anthropic = groupCatalog(CATALOG).find((g) => g.key === "anthropic")!;
  expect(anthropic.paths.map((p) => p.id)).toEqual(["device-claude-code", "anthropic"]);
  expect(anthropic.capabilities).toEqual(["chat"]);
});

test("a vendor in two API families joins its vendor; custom and device endpoints form one self-hosted group", () => {
  const groups = groupCatalog(CATALOG);
  expect(groups.find((g) => g.key === "deepseek")!.paths.map((p) => p.id)).toEqual(["deepseek/anthropic", "deepseek"]);
  expect(groups.find((g) => g.key === "deepseek")!.name).toBe("DeepSeek");
  expect(groups.find((g) => g.key === "custom")!.paths.map((p) => p.id)).toEqual(["device-openai-compatible", "custom", "custom-embeddings"]);
  expect(groups.find((g) => g.key === "systemone")!.paths.map((p) => p.id)).toEqual(["device-systemone", "systemone"]);
  expect(groups.find((g) => g.key === "embeddinggemma")!.paths.map((p) => p.id)).toEqual(["device-embeddinggemma-2", "embeddinggemma-2"]);
  // Every catalog entry is listed exactly once.
  expect(groups.flatMap((g) => g.paths.map((p) => p.id)).sort()).toEqual(CATALOG.map((p) => p.id).sort());
});

test("well-known providers first, others alphabetically, typed and self-hosted groups last", () => {
  expect(groupCatalog(CATALOG).map((g) => g.key)).toEqual(["openai", "anthropic", "deepseek", "venice", "systemone", "embeddinggemma", "custom"]);
});

test("bundled marks for providers Metro ships one for; a lettermark from its own name otherwise", () => {
  expect(vendorOf({ id: "openai", name: "OpenAI" }).mark.logo).toBe("openai");
  expect(vendorOf({ id: "device-openai-oauth", name: "Sign in with ChatGPT (on your device)" }).mark.logo).toBe("openai");
  expect(vendorOf({ id: "device-claude-code", name: "Claude (local CLI)" }).mark.logo).toBe("anthropic");
  expect(vendorOf({ id: "systemone", name: "TypeSafe System One" }).mark.logo).toBe("systemone");
  expect(vendorOf({ id: "moonshot", name: "Moonshot AI" }).mark).toMatchObject({ letters: "MA", logo: "moonshot" });
  expect(vendorOf({ id: "opencode-go", name: "OpenCode Go" }).mark.logo).toBe("opencode");
  // Custom, self-hosted and private endpoints, and vendors without a bundled mark: letters only.
  expect(vendorOf({ id: "custom", name: "Custom OpenAI-compatible endpoint" }).mark.logo).toBeUndefined();
  expect(vendorOf({ id: "device-openai-compatible", name: "OpenAI-compatible endpoint (on your device)" }).mark.logo).toBeUndefined();
  const unknown = vendorOf({ id: "acme-llm", name: "Acme LLM" });
  expect(unknown).toEqual({ key: "acme-llm", name: "Acme LLM", mark: { letters: "AL", tone: unknown.mark.tone } });
  expect(vendorOf({ id: "doubleword", name: "Doubleword" }).mark).toEqual({ letters: "Do", tone: vendorOf({ id: "doubleword", name: "Doubleword" }).mark.tone });
});

test("each way says how you sign in, where it runs, who pays, and that it starts private", () => {
  const byId = (id: string) => describePath(CATALOG.find((p) => p.id === id)!);
  expect(byId("device-openai-oauth")).toEqual({
    method: "Sign in with ChatGPT",
    variant: null,
    where: "On your device",
    billing: "Your own ChatGPT plan; can't be shared or lent",
    scope: "Only you, until you share it",
  });
  expect(byId("device-claude-code").billing).toBe("Anthropic API billing on your key (not a Pro or Max sign-in)");
  expect(byId("openai")).toMatchObject({ method: "API key", variant: "OpenAI Responses", where: "Key stored encrypted on the server", billing: "Billed to the key's account" });
  expect(byId("custom")).toMatchObject({ method: "Approved endpoint", where: "Called by the server", billing: "Whoever runs the endpoint" });
  // The packaged app sets device connections up itself: no helper names.
  expect(describePath(CATALOG.find((p) => p.id === "device-openai-compatible")!, { packaged: true }).method).toBe("Your endpoint, from this computer");
  expect(byId("device-openai-compatible")).toEqual({ method: "Your endpoint, via metro-connector", variant: null, where: "On your device", billing: "Whoever runs the endpoint", scope: "Only you, until you share it" });
});

test("two ways of one provider never read the same: a chat API key and an embeddings API key say which", () => {
  const way = (id: string, packaged = false) => wayLabel(CATALOG.find((p) => p.id === id)!, { packaged });
  expect(way("openai")).toBe("Chat API key · OpenAI Responses");
  expect(way("openai-embeddings")).toBe("Embeddings API key");
  expect(way("systemone")).toBe("System One API key");
  expect(way("custom")).toBe("Approved chat endpoint");
  expect(way("custom-embeddings")).toBe("Approved embeddings endpoint");
  expect(way("device-openai-compatible", true)).toBe("Your chat endpoint, from this computer");
  expect(way("device-embeddinggemma-2")).toBe("Your embeddings endpoint, via metro-connector");
  // Ways that already say what they are keep their name.
  expect(way("device-openai-oauth")).toBe("Sign in with ChatGPT");
  // Within every provider, no two ways share a label.
  for (const g of groupCatalog(CATALOG)) {
    const labels = g.paths.map((p) => wayLabel(p));
    expect(new Set(labels).size).toBe(labels.length);
  }
});

test("each capability has its own badge tone", () => {
  expect(new Set(Object.values(CAPABILITY_TONE)).size).toBe(3);
});

// ------------------------------------------------------------------ the add-connection picker

const ALL = { query: "", capability: "all" as const, vendor: null };
const ids = (groups: ReturnType<typeof pickerGroups>) => groups.flatMap((g) => g.paths.map((p) => p.id));

test("the picker lists every way of every provider at once, chat first, on-device before server-stored", () => {
  const groups = pickerGroups(CATALOG, ALL);
  expect(groups.map((g) => g.key)).toEqual(groupCatalog(CATALOG).map((g) => g.key));
  expect(groups.find((g) => g.key === "openai")!.paths.map((p) => p.id)).toEqual(["device-openai-oauth", "openai/anthropic", "openai", "openai-embeddings"]);
  expect(groups.find((g) => g.key === "systemone")!.paths.map((p) => p.id)).toEqual(["device-systemone", "systemone"]);
  // Exactly the catalog: nothing added, nothing lost.
  expect(ids(groups).sort()).toEqual(CATALOG.map((p) => p.id).sort());
});

test("search matches the provider, the way and the capability; every word must match", () => {
  expect(ids(pickerGroups(CATALOG, { ...ALL, query: "chatgpt" }))).toEqual(["device-openai-oauth"]);
  expect(ids(pickerGroups(CATALOG, { ...ALL, query: "OpenAI sign in" }))).toEqual(["device-openai-oauth"]);
  expect(ids(pickerGroups(CATALOG, { ...ALL, query: "anthropic api key" }))).toContain("anthropic");
  expect(ids(pickerGroups(CATALOG, { ...ALL, query: "embeddings" })).sort()).toEqual(["custom-embeddings", "device-embeddinggemma-2", "embeddinggemma-2", "openai-embeddings"]);
  expect(pickerGroups(CATALOG, { ...ALL, query: "no such provider" })).toEqual([]);
  expect(matchesQuery("OpenAI", CATALOG.find((p) => p.id === "openai")!, "  ")).toBe(true);
});

test("the capability filter and a chosen provider narrow the list", () => {
  expect(ids(pickerGroups(CATALOG, { ...ALL, capability: "system_one" }))).toEqual(["device-systemone", "systemone"]);
  const openaiEmb = pickerGroups(CATALOG, { ...ALL, capability: "embeddings", vendor: "openai" });
  expect(openaiEmb.map((g) => g.key)).toEqual(["openai"]);
  expect(ids(openaiEmb)).toEqual(["openai-embeddings"]);
  expect(pickerGroups(CATALOG, { ...ALL, capability: "system_one", vendor: "anthropic" })).toEqual([]);
});

test("each way's one line says where it runs and who pays; a ChatGPT sign-in can't be shared", () => {
  const by = (id: string) => CATALOG.find((p) => p.id === id)!;
  expect(methodDetail(by("device-openai-oauth"))).toBe("On your device · Your own ChatGPT plan; can't be shared or lent");
  expect(methodDetail(by("openai"))).toBe("Key stored encrypted on the server · Billed to the key's account");
  expect(methodDetail(by("device-openai-compatible"), { packaged: true })).toBe("On your device · Whoever runs the endpoint");
});
