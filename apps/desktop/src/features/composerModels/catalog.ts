import { signal } from "nsh";
import type { AvailableModel, TurnModelChoice } from "../../api/client";

type Scope = string | null;
type Target = { key: string; label: string; detail?: string; levels: string[]; choice: TurnModelChoice; fetchFrom?: string };
const keyFor = (c: TurnModelChoice) => JSON.stringify([c.connectionId, c.modelId ?? null]);

function targets(rows: AvailableModel[]): Target[] {
  return rows.filter(m => m.capability === "chat").flatMap(m => {
    const detail = m.runsOn === "owner_connection"
      ? `${m.ownerId ?? "The owner"} can see these requests and may pay for them.`
      : m.runsOn === "your_own_sign_in" ? "Shared preset · uses your own sign-in." : undefined;
    return (m.chatModels ?? [{ id: undefined, reasoningEfforts: [] }]).map(model => {
      const choice = model.id === undefined ? { connectionId: m.connectionId } : { connectionId: m.connectionId, modelId: model.id };
      return {
        key: keyFor(choice), choice, detail, levels: model.reasoningEfforts,
        fetchFrom: m.owned && m.execution === "server" && m.runsOn === "your_connection" ? m.name || m.providerId : undefined,
        label: [model.id ?? m.model, m.name, m.providerId].filter(Boolean).join(" · "),
      };
    });
  });
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
  async function load(scope: Scope, valid: () => boolean, fetch: () => Promise<AvailableModel[]>) {
    const ticket = ++generation;
    const old = state();
    const base = { scope, valid, pin: old?.scope === scope && old.valid() ? old.pin : undefined };
    state.set({ ...base, phase: "loading", targets: [], systemOne: false });
    try {
      const rows = await fetch();
      if (ticket !== generation || !valid()) return;
      const usable = rows.filter(m => m.ready !== false && (m.runsOn !== "your_own_sign_in" || m.ready === true)
        && (m.owned || m.visibility === "organization" || (scope !== null && m.visibility === "spaces")));
      state.set({ ...base, phase: "ready", targets: targets(usable), systemOne: usable.some(m => m.capability === "system_one") });
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
