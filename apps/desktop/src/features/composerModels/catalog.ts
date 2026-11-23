import { signal } from "nsh";
import type { AvailableModel, TurnModelChoice } from "../../api/client";
import type { Mark } from "../../ui";
import { vendorOf } from "../providerGroups";

type Scope = string | null;
/** The provider a target is listed under: its visible name and mark, as Model providers shows them. */
export type TargetProvider = { key: string; name: string; mark: Mark };
export type Target = {
  key: string;
  /** "model · connection · provider": what search matches and screen readers say. */
  label: string;
  /** The model as the row and the closed trigger show it. */
  model: string;
  /** The connection's own name (two connections of one provider stay apart). */
  connection: string;
  provider: TargetProvider;
  detail?: string;
  /** Exactly the levels the server advertised for this model; nothing is assumed. */
  levels: string[];
  choice: TurnModelChoice;
  fetchFrom?: string;
};
/** A catalog entry's visible name, by provider id (GET /v1/provider-catalog); only used to name groups. */
export type ProviderName = { id: string; name: string };
const keyFor = (c: TurnModelChoice) => JSON.stringify([c.connectionId, c.modelId ?? null]);

/** Listed first, in this order; custom endpoints last; the rest by name. */
const FIRST = ["openai", "anthropic", "gemini"];
const byName = (a: string, b: string) => { const x = a.toLowerCase(), y = b.toLowerCase(); return x < y ? -1 : x > y ? 1 : 0; };
const rank = (key: string) => { const i = FIRST.indexOf(key); return i >= 0 ? i : key === "custom" ? 1000 : 100; };

/**
 * The provider a connection belongs to. Known vendors get their canonical name
 * (device-openai-oauth and openai are both "OpenAI"); others take the server
 * catalog's name, else the provider id, with a lettermark when Metro ships no logo.
 */
function providerOf(providerId: string, names: ReadonlyMap<string, string>): TargetProvider {
  return vendorOf({ id: providerId, name: names.get(providerId) ?? providerId });
}

function targets(rows: AvailableModel[], names: ReadonlyMap<string, string>): Target[] {
  const list = rows.filter(m => m.capability === "chat").flatMap(m => {
    const provider = providerOf(m.providerId, names);
    const detail = m.runsOn === "owner_connection"
      ? `${m.ownerId ?? "The owner"} can see these requests and may pay for them.`
      : m.runsOn === "your_own_sign_in" ? "Shared preset · uses your own sign-in." : undefined;
    return (m.chatModels ?? [{ id: undefined, reasoningEfforts: [] }]).map(model => {
      const choice = model.id === undefined ? { connectionId: m.connectionId } : { connectionId: m.connectionId, modelId: model.id };
      return {
        key: keyFor(choice), choice, detail, levels: model.reasoningEfforts,
        fetchFrom: m.owned && m.execution === "server" && m.runsOn === "your_connection" ? m.name || m.providerId : undefined,
        label: [model.id ?? m.model, m.name, provider.name].filter(Boolean).join(" · "),
        model: model.id ?? m.model ?? m.name, connection: m.name, provider,
      };
    });
  });
  // Grouped by provider; a provider's targets keep the server's order.
  return list.map((t, i) => ({ t, i }))
    .sort((a, b) => rank(a.t.provider.key) - rank(b.t.provider.key) || byName(a.t.provider.name, b.t.provider.name)
      || a.t.provider.key.localeCompare(b.t.provider.key) || a.i - b.i)
    .map(x => x.t);
}

type Snapshot = {
  scope: Scope; valid: () => boolean; phase: "loading" | "ready" | "failed";
  targets: Target[]; systemOne: boolean; pin?: TurnModelChoice;
};

/** One active scope only. Refresh drops catalog metadata immediately, retaining only the
 * wire choice so loss of access or removal of an effort can never imply Default. */
export function createComposerCatalog() {
  const state = signal<Snapshot | undefined>(undefined);
  let generation = 0;
  function clear() {
    generation++;
    state.set(undefined);
  }
  /** `providers` only names groups: when it fails, groups fall back to provider ids and nothing else changes. */
  async function load(scope: Scope, valid: () => boolean, fetch: () => Promise<AvailableModel[]>,
    providers?: () => Promise<readonly ProviderName[]>) {
    const ticket = ++generation;
    const old = state();
    const base = { scope, valid, pin: old?.scope === scope && old.valid() ? old.pin : undefined };
    state.set({ ...base, phase: "loading", targets: [], systemOne: false });
    try {
      const [rows, named] = await Promise.all([fetch(), providers ? providers().catch(() => []) : []]);
      const names = new Map(named.map(p => [p.id, p.name]));
      if (ticket !== generation || !valid()) return;
      const usable = rows.filter(m => m.ready !== false && (m.runsOn !== "your_own_sign_in" || m.ready === true)
        && (m.owned || m.visibility === "organization" || (scope !== null && m.visibility === "spaces")));
      state.set({ ...base, phase: "ready", targets: targets(usable, names), systemOne: usable.some(m => m.capability === "system_one") });
    } catch {
      if (ticket === generation && valid()) state.set({ ...base, phase: "failed", targets: [], systemOne: false });
    }
  }
  function current(scope: Scope | undefined) {
    const s = state();
    return s && s.scope === scope && s.valid() ? s : undefined;
  }
  function selected(s?: Snapshot) {
    return s?.pin ? s.targets.find(t => t.key === keyFor(s.pin!)) : undefined;
  }
  function select(s: Snapshot, key: string | null, level?: string) {
    if (current(s.scope) !== s || s.phase !== "ready") return;
    if (key === null) { state.set({ ...s, pin: undefined }); return; }
    const t = s.targets.find(t => t.key === key);
    if (!t || (level !== undefined && !t.levels.includes(level))) return;
    state.set({ ...s, pin: level === undefined ? t.choice : { ...t.choice, reasoningEffort: level } });
  }
  function choice(scope: Scope | undefined): { choice?: TurnModelChoice; error?: string } {
    const s = state();
    if (!s || s.scope !== scope || !s.pin) return {};
    const t = s.valid() ? selected(s) : undefined;
    if (s.phase !== "ready" || !t || (s.pin.reasoningEffort !== undefined && !t.levels.includes(s.pin.reasoningEffort)))
      return { error: "The selected model or thinking level is unavailable. Open Model to choose again." };
    return { choice: { ...s.pin } };
  }
  function knownNoSystemOne(scope: Scope | undefined) {
    const s = current(scope);
    return !!s && s.phase === "ready" && !s.systemOne && s.targets.length > 0 && !choice(scope).error;
  }
  return { clear, load, current, select, selected, choice, knownNoSystemOne };
}
