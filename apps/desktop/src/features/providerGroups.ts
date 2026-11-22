// How Model providers groups the server's catalog: one entry per provider
// (vendor), each with the ways to connect to it, so OpenAI is one choice
// whose ways are Sign in with ChatGPT on your device, an API key and, where
// the server offers it, embeddings. Pure functions of the catalog the server
// sent (GET /v1/provider-catalog): nothing here adds a provider, a sign-in or
// a capability the catalog doesn't have; it only decides where an entry is
// listed and how its way of connecting is described.
//
// Marks: a provider's bundled mark where Metro ships one (src/ui/icons,
// attributed in LICENSE-brand-marks.txt), else a lettermark from its own name.
// Custom, self-hosted and private endpoints always get a lettermark. Nothing
// is fetched.

import type { Capability, CatalogProvider } from "../settings";
import { hasBrandLogo } from "../ui";
import type { Mark, Tone } from "../ui";

export type VendorGroup = {
  /** Stable key: "openai", "anthropic", "custom", or the catalog id's vendor part. */
  key: string;
  name: string;
  mark: Mark;
  /** The ways to connect, device ones first, then by capability. */
  paths: CatalogProvider[];
  capabilities: Capability[];
};

/** Vendors Metro knows how to name, and which catalog ids belong to them. */
const KNOWN: readonly { key: string; name: string; letters: string; tone: Tone; ids: (id: string) => boolean }[] = [
  {
    key: "openai",
    name: "OpenAI",
    letters: "AI",
    tone: "success",
    ids: (id) => id === "openai" || id.startsWith("openai/") || id === "openai-embeddings" || id === "device-openai-oauth",
  },
  { key: "anthropic", name: "Anthropic", letters: "A", tone: "warning", ids: (id) => id === "anthropic" || id.startsWith("anthropic/") || id === "device-claude-code" },
  { key: "gemini", name: "Google Gemini", letters: "G", tone: "runner", ids: (id) => id === "gcp.gemini" || id.startsWith("gcp.gemini/") },
  { key: "azure-openai", name: "Azure OpenAI", letters: "Az", tone: "runner", ids: (id) => id === "azure.openai" || id.startsWith("azure.openai/") },
  { key: "systemone", name: "System One", letters: "S1", tone: "agent", ids: (id) => id === "systemone" || id === "custom-systemone" || id === "device-systemone" },
  { key: "embeddinggemma", name: "EmbeddingGemma 2", letters: "EG", tone: "system", ids: (id) => id === "embeddinggemma-2" || id === "device-embeddinggemma-2" },
  {
    key: "custom",
    name: "Custom & self-hosted",
    letters: "{}",
    tone: "neutral",
    ids: (id) => id === "custom" || id === "custom-embeddings" || id === "device-openai-compatible" || id === "device-embeddings",
  },
];

/** Groups listed first, in this order; the rest alphabetically, then these last. */
const FIRST = ["openai", "anthropic", "gemini"];
const LAST = ["systemone", "embeddinggemma", "custom"];
const TONES: readonly Tone[] = ["agent", "runner", "success", "warning", "system", "danger", "neutral"];

/** "OpenAI (OpenAI Responses)" -> "OpenAI"; "Venice" -> "Venice". */
const baseName = (name: string) => name.replace(/\s*\([^)]*\)\s*$/, "").trim() || name;

function lettersOf(name: string): string {
  const words = name.replace(/[^\p{L}\p{N}\s.]/gu, " ").split(/[\s.]+/).filter(Boolean);
  if (words.length >= 2) return (words[0][0] + words[1][0]).toUpperCase();
  const w = words[0] ?? "?";
  return w.length <= 2 ? w.toUpperCase() : w[0].toUpperCase() + w[1].toLowerCase();
}

function toneOf(key: string): Tone {
  let h = 0;
  for (const ch of key) h = (h * 31 + ch.charCodeAt(0)) >>> 0;
  return TONES[h % TONES.length];
}

/** Provider keys that share another key's mark (one company, two products). */
const SAME_MARK: Record<string, string> = { "opencode-go": "opencode" };

/** A mark for `key`: its bundled logo if Metro ships one, plus the lettermark fallback. */
function markOf(key: string, letters: string, tone: Tone): Mark {
  const logo = SAME_MARK[key] ?? key;
  return key !== "custom" && hasBrandLogo(logo) ? { letters, tone, logo } : { letters, tone };
}

/** Which group a catalog id belongs to, and that group's name and mark. */
export function vendorOf(provider: Pick<CatalogProvider, "id" | "name">): { key: string; name: string; mark: Mark } {
  const known = KNOWN.find((k) => k.ids(provider.id));
  if (known) return { key: known.key, name: known.name, mark: markOf(known.key, known.letters, known.tone) };
  // A registered vendor in a second API family is `vendor/format`; it joins its vendor.
  const key = provider.id.split("/")[0];
  const name = baseName(provider.name);
  return { key, name, mark: markOf(key, lettersOf(name), toneOf(key)) };
}

const CAPABILITY_ORDER: Record<Capability, number> = { chat: 0, system_one: 1, embeddings: 2 };

/** The catalog grouped by provider, in a stable, readable order. */
export function groupCatalog(providers: readonly CatalogProvider[]): VendorGroup[] {
  const groups = new Map<string, VendorGroup>();
  for (const p of providers) {
    const v = vendorOf(p);
    let g = groups.get(v.key);
    if (!g) {
      g = { key: v.key, name: v.name, mark: v.mark, paths: [], capabilities: [] };
      groups.set(v.key, g);
    }
    g.paths.push(p);
    if (!g.capabilities.includes(p.capability)) g.capabilities.push(p.capability);
  }
  for (const g of groups.values()) {
    g.paths.sort(
      (a, b) =>
        (a.execution === "device" ? 0 : 1) - (b.execution === "device" ? 0 : 1) ||
        CAPABILITY_ORDER[a.capability] - CAPABILITY_ORDER[b.capability] ||
        a.name.localeCompare(b.name),
    );
    g.capabilities.sort((a, b) => CAPABILITY_ORDER[a] - CAPABILITY_ORDER[b]);
  }
  const rank = (key: string) => {
    const first = FIRST.indexOf(key);
    if (first >= 0) return first;
    const last = LAST.indexOf(key);
    return last >= 0 ? 1000 + last : 100;
  };
  return [...groups.values()].sort((a, b) => rank(a.key) - rank(b.key) || a.name.localeCompare(b.name));
}

/** How one way of connecting works, in a few words each: what you use, where it runs, who pays, and what it's for. */
export type PathSummary = {
  /** "Sign in with ChatGPT", "API key", "Approved endpoint", ... */
  method: string;
  /** The API family when the catalog names one ("OpenAI Responses"). */
  variant: string | null;
  where: string;
  billing: string;
  /** The default scope of a new connection: always the person's own. */
  scope: string;
};

/**
 * `packaged`: the desktop app sets device connections up itself, so a way is
 * described by what the person does in Metro, never by a helper's name.
 */
export function describePath(p: CatalogProvider, opts: { packaged?: boolean } = {}): PathSummary {
  // A server adapter's API family, as the catalog names it ("OpenAI (OpenAI Responses)").
  // A device entry's parenthesis says where it runs, which `where` already says.
  const variant = p.execution === "server" ? /\(([^)]*)\)\s*$/.exec(p.name)?.[1] ?? null : null;
  const scope = "Only you, until you share it";
  if (p.id === "device-openai-oauth") {
    return { method: "Sign in with ChatGPT", variant: null, where: "On your device", billing: "Your own ChatGPT plan; can't be shared or lent", scope };
  }
  if (p.id === "device-claude-code") {
    return { method: "Claude Code with your API key", variant: null, where: "On your device", billing: "Anthropic API billing on your key (not a Pro or Max sign-in)", scope };
  }
  if (p.execution === "device") {
    return { method: opts.packaged ? "Your endpoint, from this computer" : "Your endpoint, via metro-connector", variant, where: "On your device", billing: "Whoever runs the endpoint", scope };
  }
  if (p.requiresKey) {
    return { method: "API key", variant, where: "Key stored encrypted on the server", billing: "Billed to the key's account", scope };
  }
  if (p.allowCustomUrl && !p.defaultBaseUrl) {
    return { method: "Approved endpoint", variant, where: "Called by the server", billing: "Whoever runs the endpoint", scope };
  }
  return { method: "API key optional", variant, where: "Called by the server", billing: "Whoever runs the endpoint", scope };
}
