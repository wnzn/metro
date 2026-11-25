// Model routing and connection sharing: state only, no UI.
//
// Model routing (GET/PUT /v1/model-routing) is the signed-in user's own:
// - Use-case routing: when agent work starts for them, System One picks
//   which of their usable chat connections answers.
// - Confidence for unaddressed messages in Spaces where classification is on.
// - The embeddings connection search uses (and the classifier reranks with).
// The page edits a draft and saves it whole.
//
// Sharing (GET/PUT /v1/providers/{id}/sharing) is decided by a connection's
// owner only. It grants use of the connection, never its key, sign-in or
// device token.
//
// Races follow src/settings.ts: one sequence per request kind, and every
// answer is dropped unless it is the latest and the session is unchanged.

import { batch, computed, effect, signal, untracked } from "nsh";
import { ApiError } from "./api/client";
import type { AvailableModel, Capability, ModelRouting, RoutingCase, Sharing, ShareVisibility } from "./api/client";
import { connections, loadProviders } from "./settings";
import { apiClient, describe, disconnect, isAborted, onSessionEnd, onTransientClear, place, sessionGuard, settingsSection, spaces } from "./store";

export type { AvailableModel, ModelRouting, RoutingCase, Sharing, ShareVisibility };

// ------------------------------------------------------------------ models the user may pick

export const available = signal<AvailableModel[] | null>(null);
export const availableError = signal<string | null>(null);
export const availableLoading = signal(false);
let availableSeq = 0;

export async function loadAvailable(): Promise<void> {
  const c = apiClient();
  if (!c) return;
  const seq = ++availableSeq;
  const ok = sessionGuard();
  availableLoading.set(true);
  try {
    const list = await c.availableModels();
    if (!ok() || seq !== availableSeq) return;
    batch(() => {
      available.set(list);
      availableError.set(null);
    });
  } catch (error) {
    if (!ok() || seq !== availableSeq) return;
    fail(error, availableError, "Could not load the models you can use");
  } finally {
    if (ok() && seq === availableSeq) availableLoading.set(false);
  }
}

/** The models of one capability, for a picker. A connection is never offered for another capability. */
export function modelsFor(capability: Capability): AvailableModel[] {
  return (available() ?? []).filter((m) => m.capability === capability);
}

export function findAvailable(id: string | null): AvailableModel | null {
  return id ? (available() ?? []).find((m) => m.connectionId === id) ?? null : null;
}

/** Choices store a connection, so its pinned/default model must be usable. */
export function modelChoiceUsable(m: AvailableModel, capability: Capability): boolean {
  return m.capability === capability && !!m.model && m.ready !== false &&
    (m.capability !== "chat" || !m.chatModels || m.chatModels.some((x) => x.id === m.model && x.available !== false));
}

/**
 * Who besides its owner may use one of my connections, as last known: what
 * this session saved or loaded, else what the available-models list says,
 * else unknown (null).
 */
export function shareOf(connectionId: string): ShareVisibility | null {
  return shares()[connectionId]?.visibility ?? available()?.find((m) => m.owned && m.connectionId === connectionId)?.visibility ?? null;
}

// ------------------------------------------------------------------ the routing draft

/** A use case being edited. `key` is null until it is first saved. */
export type DraftCase = { uid: string; key: string | null; label: string; description: string; connectionId: string | null };
export type RoutingDraft = {
  enabled: boolean;
  /** Retained only to round-trip legacy settings. */
  inferAddressing: boolean;
  classifierId: string | null;
  embeddingId: string | null;
  defaultChatId: string | null;
  /** As typed: validated on save. */
  minDirectedness: string;
  minRouteProbability: string;
  cases: DraftCase[];
};

/** What the server has stored, as last loaded or saved. */
export const routing = signal<ModelRouting | null>(null);
export const draft = signal<RoutingDraft | null>(null);
export const routingError = signal<string | null>(null);
export const routingNotice = signal<string | null>(null);
/** The server answered 404: it has no model routing. */
export const routingUnsupported = signal(false);
export const routingSaving = signal(false);
let routingSeq = 0;
let routingVisit = 0;
let uidSeq = 0;

// The server's limits (crates/server/src/model_routing.rs).
export const MAX_CASES = 16;
export const MAX_LABEL = 64;
export const MAX_DESCRIPTION = 500;
const MAX_KEY = 32;
/** "Is this asking the agent?" must be at least this sure; lower would start the agent on chatter. */
export const MIN_DIRECTEDNESS_FLOOR = 0.5;

export function toDraft(r: ModelRouting): RoutingDraft {
  return {
    enabled: r.enabled,
    inferAddressing: r.inferAddressing,
    classifierId: r.classifierId,
    embeddingId: r.embeddingId,
    defaultChatId: r.defaultChatId,
    minDirectedness: formatProbability(r.minDirectedness),
    minRouteProbability: formatProbability(r.minRouteProbability),
    cases: r.cases.map((c) => ({ uid: `c${++uidSeq}`, key: c.key, label: c.label, description: c.description, connectionId: c.connectionId })),
  };
}

export function formatProbability(n: number): string {
  return String(Math.round(n * 100) / 100);
}

/** A probability typed by hand: 0 to 1, as "0.7", ".7" or "70%". null when it isn't one. */
export function parseProbability(text: string): number | null {
  const t = text.trim();
  if (!t) return null;
  const percent = t.endsWith("%");
  const n = Number(percent ? t.slice(0, -1).trim() : t);
  if (!Number.isFinite(n)) return null;
  const value = percent ? n / 100 : n;
  return value >= 0 && value <= 1 ? value : null;
}

/** A stable key for a new use case, from its label (`[a-z][a-z0-9_]{0,31}`), unique among `taken`. */
export function caseKey(label: string, taken: ReadonlySet<string>): string {
  let base = label
    .toLowerCase()
    .normalize("NFKD")
    .replace(/[^a-z0-9]+/g, "_")
    .replace(/^_+|_+$/g, "");
  if (!base) base = "case";
  else if (!/^[a-z]/.test(base)) base = `case_${base}`;
  const fit = (stem: string, suffix: string) => `${stem.slice(0, MAX_KEY - suffix.length).replace(/_+$/, "")}${suffix}`;
  let key = fit(base, "");
  for (let n = 2; taken.has(key); n++) key = fit(base, `_${n}`);
  return key;
}

/**
 * Checks a draft against what the user can use, and turns it into what is
 * saved. Returns the first problem in words, or the routing.
 */
export function finalize(d: RoutingDraft, usable: readonly AvailableModel[]): { ok: true; routing: ModelRouting } | { ok: false; error: string } {
  const has = (id: string | null, capability: Capability) =>
    !!id && usable.some((m) => m.connectionId === id && modelChoiceUsable(m, capability));
  const minDirectedness = parseProbability(d.minDirectedness);
  const minRouteProbability = parseProbability(d.minRouteProbability);
  // Shown as percentages, as the page asks for them; the values stay 0 to 1.
  if (minRouteProbability === null) return { ok: false, error: "The routing confidence must be a whole number from 0 to 100%, such as 65%." };
  if (minDirectedness === null || minDirectedness < MIN_DIRECTEDNESS_FLOOR) {
    return { ok: false, error: `Space classification confidence must be a whole number from ${Math.round(MIN_DIRECTEDNESS_FLOOR * 100)} to 100%, such as 85%.` };
  }
  if (d.enabled && !d.classifierId) {
    return { ok: false, error: "Choose a System One classifier to use model routing." };
  }
  if (d.classifierId && !has(d.classifierId, "system_one")) return { ok: false, error: "The classifier you chose isn't available to you any more. Choose another one." };
  if (d.embeddingId && !has(d.embeddingId, "embeddings")) return { ok: false, error: "The embeddings model you chose isn't available to you any more. Choose another one." };
  if (d.defaultChatId && !has(d.defaultChatId, "chat")) return { ok: false, error: "The fallback answer model isn't available to you any more. Choose another one, or your default." };
  if (d.cases.length > MAX_CASES) return { ok: false, error: `At most ${MAX_CASES} use cases.` };
  if (d.enabled && d.cases.length === 0) return { ok: false, error: "Add at least one use case, or turn use-case routing off." };
  const labels = new Set<string>();
  for (const c of d.cases) {
    const label = c.label.trim();
    if (!label) return { ok: false, error: "Every use case needs a name." };
    if (label.length > MAX_LABEL) return { ok: false, error: `“${label.slice(0, 20)}…” is too long a name (at most ${MAX_LABEL} characters).` };
    if (c.description.trim().length > MAX_DESCRIPTION) return { ok: false, error: `The description of “${label}” is too long (at most ${MAX_DESCRIPTION} characters).` };
    if (labels.has(label.toLowerCase())) return { ok: false, error: `Two use cases are called “${label}”. Give each its own name.` };
    labels.add(label.toLowerCase());
    if (!c.connectionId) return { ok: false, error: `Choose which model answers “${label}”.` };
    if (!has(c.connectionId, "chat")) return { ok: false, error: `The model for “${label}” isn't available to you any more. Choose another one.` };
  }
  const taken = new Set(d.cases.map((c) => c.key).filter((k): k is string => !!k));
  const cases: RoutingCase[] = d.cases.map((c) => {
    const key = c.key ?? caseKey(c.label.trim(), taken);
    taken.add(key);
    return { key, label: c.label.trim(), description: c.description.trim(), connectionId: c.connectionId };
  });
  return {
    ok: true,
    routing: {
      enabled: d.enabled,
      inferAddressing: d.inferAddressing,
      classifierId: d.classifierId,
      embeddingId: d.embeddingId,
      defaultChatId: d.defaultChatId,
      minDirectedness,
      minRouteProbability,
      cases,
    },
  };
}

/** The draft differs from what is stored. */
export const routingDirty = computed(() => {
  const d = draft();
  const r = routing();
  if (!d || !r) return false;
  // Availability changes independently of the draft. A revoked selection
  // must be shown and resolved, but it isn't itself an unsaved edit.
  return d.enabled !== r.enabled || d.classifierId !== r.classifierId || d.embeddingId !== r.embeddingId ||
    d.defaultChatId !== r.defaultChatId || parseProbability(d.minDirectedness) !== parseProbability(formatProbability(r.minDirectedness)) ||
    parseProbability(d.minRouteProbability) !== parseProbability(formatProbability(r.minRouteProbability)) ||
    d.cases.length !== r.cases.length || d.cases.some((c, i) => {
      const saved = r.cases[i];
      return c.label.trim() !== saved.label || c.description.trim() !== saved.description || c.connectionId !== saved.connectionId;
    });
});

export async function loadRouting(): Promise<void> {
  const c = apiClient();
  if (!c || routingSaving()) return;
  const seq = ++routingSeq;
  const ok = sessionGuard();
  routingError.set(null);
  try {
    const value = await c.getModelRouting();
    if (!ok() || seq !== routingSeq) return;
    batch(() => {
      routingUnsupported.set(false);
      routing.set(value);
      // Keep what the user is typing; only a fresh page starts from the server's copy.
      if (!draft()) draft.set(toDraft(value));
      routingError.set(null);
    });
  } catch (error) {
    if (!ok() || seq !== routingSeq) return;
    if (error instanceof ApiError && error.kind === "not_found") {
      routingUnsupported.set(true);
      return;
    }
    fail(error, routingError, "Could not load your model routing");
  }
}

export function editRouting(patch: Partial<Omit<RoutingDraft, "cases">>) {
  const d = draft();
  if (d) draft.set({ ...d, ...patch });
}

export function addCase() {
  const d = draft();
  if (!d || d.cases.length >= MAX_CASES) return;
  draft.set({ ...d, cases: [...d.cases, { uid: `c${++uidSeq}`, key: null, label: "", description: "", connectionId: null }] });
}

export function editCase(uid: string, patch: Partial<Pick<DraftCase, "label" | "description" | "connectionId">>) {
  const d = draft();
  if (d) draft.set({ ...d, cases: d.cases.map((c) => (c.uid === uid ? { ...c, ...patch } : c)) });
}

export function removeCase(uid: string) {
  const d = draft();
  if (d) draft.set({ ...d, cases: d.cases.filter((c) => c.uid !== uid) });
}

/** Back to what is stored. */
export function revertRouting() {
  const r = routing();
  batch(() => {
    draft.set(r ? toDraft(r) : null);
    routingError.set(null);
    routingNotice.set(null);
  });
}

export async function saveRouting(): Promise<boolean> {
  const c = apiClient();
  const d = draft();
  if (!c || !d || !available() || availableLoading() || availableError() || routingSaving()) return false;
  const done = finalize(d, available() ?? []);
  if (!done.ok) {
    batch(() => {
      routingError.set(done.error);
      routingNotice.set(null);
    });
    return false;
  }
  const ok = sessionGuard();
  const visit = routingVisit;
  batch(() => {
    routingSaving.set(true);
    routingError.set(null);
    routingNotice.set(null);
  });
  try {
    const stored = await c.putModelRouting(done.routing);
    if (!ok()) return false;
    routingSeq++; // an older GET must not bring back the old settings
    batch(() => {
      routing.set(stored);
      if (onModelsPage()) draft.set(toDraft(stored));
      if (visit === routingVisit) routingNotice.set(savedText(stored));
    });
    return true;
  } catch (error) {
    if (ok()) {
      if (visit === routingVisit || (error instanceof ApiError && error.kind === "auth")) fail(error, routingError, "Could not save your model choices");
      else if (onModelsPage()) {
        routingSaving.set(false);
        void loadRouting();
      }
    }
    return false;
  } finally {
    if (ok()) routingSaving.set(false);
  }
}

function savedText(r: ModelRouting): string {
  return r.enabled
    ? `Saved. Use-case routing is on (${r.cases.length} use case${r.cases.length === 1 ? "" : "s"}).`
    : "Saved. Use-case routing is off.";
}

// ------------------------------------------------------------------ sharing (owner only)

/** The connection whose sharing panel is open. One at a time. */
export const sharingOpen = signal<string | null>(null);
/** The share as stored, per connection id, once loaded. */
export const shares = signal<Record<string, Sharing>>({});
export const shareDraft = signal<{ connectionId: string; visibility: ShareVisibility; spaceIds: string[] } | null>(null);
export const shareBusy = signal<string | null>(null);
export const shareError = signal<string | null>(null);
export const shareNotice = signal<string | null>(null);
const shareSeq = new Map<string, number>();

/** Sign-ins tied to one person's account and plan: never shared, only as a bring-your-own preset. */
export const isSubscription = (providerId: string) => providerId === "device-openai-oauth";

export async function openSharing(connectionId: string): Promise<void> {
  const c = apiClient();
  if (!c) return;
  if (sharingOpen() === connectionId) {
    closeSharing();
    return;
  }
  const seq = (shareSeq.get(connectionId) ?? 0) + 1;
  shareSeq.set(connectionId, seq);
  const ok = sessionGuard();
  batch(() => {
    sharingOpen.set(connectionId);
    shareDraft.set(null);
    shareError.set(null);
    shareNotice.set(null);
  });
  try {
    const value = await c.getSharing(connectionId);
    if (!ok() || shareSeq.get(connectionId) !== seq || sharingOpen() !== connectionId) return;
    batch(() => {
      shares.set({ ...shares(), [connectionId]: value });
      shareDraft.set({ connectionId, visibility: value.visibility, spaceIds: value.spaceIds });
    });
  } catch (error) {
    if (!ok() || shareSeq.get(connectionId) !== seq || sharingOpen() !== connectionId) return;
    fail(error, shareError, "Could not load who can use this connection");
  }
}

export function closeSharing() {
  batch(() => {
    sharingOpen.set(null);
    shareDraft.set(null);
    shareError.set(null);
  });
}

export function setShareVisibility(visibility: ShareVisibility) {
  const d = shareDraft();
  if (d) shareDraft.set({ ...d, visibility });
}

export function toggleShareSpace(spaceId: string) {
  const d = shareDraft();
  if (!d) return;
  const has = d.spaceIds.includes(spaceId);
  shareDraft.set({ ...d, spaceIds: has ? d.spaceIds.filter((s) => s !== spaceId) : [...d.spaceIds, spaceId] });
}

/** Spaces the owner may share with: the ones they're a member of now. */
export const shareableSpaces = computed(() => spaces());

export async function saveSharing(): Promise<boolean> {
  const c = apiClient();
  const d = shareDraft();
  if (!c || !d || shareBusy()) return false;
  const member = new Set(spaces().map((s) => s.id));
  const spaceIds = d.spaceIds.filter((s) => member.has(s));
  if (d.visibility === "spaces" && spaceIds.length === 0) {
    shareError.set("Pick at least one Space, or choose Only me.");
    return false;
  }
  const id = d.connectionId;
  const name = connections()?.find((x) => x.id === id)?.name ?? "the connection";
  const ok = sessionGuard();
  batch(() => {
    shareBusy.set(id);
    shareError.set(null);
    shareNotice.set(null);
  });
  try {
    const stored = await c.putSharing(id, { visibility: d.visibility, spaceIds });
    if (!ok()) return false;
    shareSeq.set(id, (shareSeq.get(id) ?? 0) + 1);
    batch(() => {
      shares.set({ ...shares(), [id]: stored });
      connections.set((connections() ?? []).map((x) => (x.id === id ? { ...x, visibility: stored.visibility } : x)));
      if (shareDraft()?.connectionId === id) shareDraft.set({ connectionId: id, visibility: stored.visibility, spaceIds: stored.spaceIds });
      shareNotice.set(sharedText(name, stored));
    });
    void loadAvailable();
    return true;
  } catch (error) {
    if (ok()) fail(error, shareError, "Could not change who can use this connection");
    return false;
  } finally {
    if (ok()) shareBusy.set(null);
  }
}

function sharedText(name: string, s: Sharing): string {
  if (s.visibility === "personal") return `“${name}” is private again. Nobody else can use it from now on.`;
  if (s.visibility === "organization") return `Everyone signed in to this server can now use “${name}”. You can make it private again at any time.`;
  const names = s.spaceIds.map((id) => spaces().find((x) => x.id === id)?.name ?? id);
  return `Members of ${names.join(", ")} can now use “${name}”. You can make it private again at any time.`;
}

// ------------------------------------------------------------------ errors and lifecycle

function fail(error: unknown, target: { set(v: string | null): void }, what: string) {
  if (isAborted(error)) return;
  if (error instanceof ApiError && error.kind === "auth") {
    disconnect(error.message);
    return;
  }
  target.set(error instanceof ApiError ? `${what}: ${error.message}` : describe(error, what));
}

// Opening Model choices loads it fresh, with the models the user may pick and
// the catalog (to name providers). Model connections also needs what is
// available to say what a share means.
const onModelsPage = () => place() === "settings" && settingsSection() === "models";
effect(() => {
  const here = place() === "settings" ? settingsSection() : null;
  untracked(() => {
    if (here === "models") {
      void loadRouting();
      void loadAvailable();
      void loadProviders();
    } else if (here === "connections") void loadAvailable(); // which of mine are shared
  });
});

// Leaving the page drops an unsaved draft and any open sharing panel.
onTransientClear(() => {
  if (onModelsPage()) {
    routingVisit++;
    routingSeq++;
  }
  batch(() => {
    draft.set(null); // the next visit starts from what is stored
    routingError.set(null);
    routingNotice.set(null);
    sharingOpen.set(null);
    shareDraft.set(null);
    shareError.set(null);
    shareNotice.set(null);
  });
});

onSessionEnd(() => {
  availableSeq++;
  routingSeq++;
  shareSeq.clear();
  available.set(null);
  availableError.set(null);
  availableLoading.set(false);
  routing.set(null);
  draft.set(null);
  routingError.set(null);
  routingNotice.set(null);
  routingUnsupported.set(false);
  routingSaving.set(false);
  sharingOpen.set(null);
  shares.set({});
  shareDraft.set(null);
  shareBusy.set(null);
  shareError.set(null);
  shareNotice.set(null);
});
