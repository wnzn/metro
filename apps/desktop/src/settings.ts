// Settings state: registered runners (personal or organization) and which
// one serves a Space, and the user's personal model connections. Pages read
// these signals and call these functions; nothing here knows about the UI.
//
// Races
// - Each list has its own request sequence. An answer is applied only if it
//   is the latest request of that list and the session is still the same, so
//   a slow, older response never overwrites a newer one, and nothing lands
//   after sign-out.
// - Space runner answers are also checked against the Space's epoch (see
//   store.ts), so they never land in another Space or after access was lost.
//
// Secrets
// - Runner, dedicated and device tokens are returned once by the server. The
//   one just issued lives only in `revealed` (memory), masked until the user
//   asks to see it, and is wiped when they press Done, change page, section,
//   Space or room, or sign out. It is never written to storage or logs.
// - A provider API key is sent once and never read back (`has_key` only). The
//   form that holds it clears it as soon as it is sent.

import { batch, computed, effect, signal, untracked } from "nsh";
import { ApiError, EndpointPatchError } from "./api/client";
import { serverHost } from "./api/serverUrl";
import { readStartupProviderCatalog } from "./startupModelRequests";
import type { PageScope } from "./ui";
import type {
  Capability,
  CatalogProvider,
  ConnectionModel,
  ConnectionModelList,
  ConnectionModelPatch,
  DeviceStatus,
  ModelThinking,
  NewConnectionModel,
  NewProviderConnection,
  ProviderCatalog,
  ProviderConnection,
  ProviderPatch,
  RegisteredRunner,
  RunnerPending,
  RunnerVisibility,
  SpaceRunner,
} from "./api/client";
import {
  activeSpace,
  activeSpaceId,
  apiClient,
  connectedTo,
  describe,
  disconnect,
  handleManageError,
  isAborted,
  onManageOpen,
  onSessionEnd,
  onTransientClear,
  orgAdmin,
  place,
  sessionGuard,
  setRunnerPaired,
  settingsSection,
  spaces,
  view,
} from "./store";
import { forgetQuietly, localConnections, localStep, onLocalPairingChanged, packaged, refreshHelper } from "./device/state";

export type { Capability, CatalogProvider, ConnectionModel, DeviceStatus, ModelThinking, NewConnectionModel, ProviderConnection, RegisteredRunner, RunnerVisibility, SpaceRunner };

export function settingsAccountScope(): PageScope | undefined {
  const server = connectedTo();
  return server ? { kind: "account", host: serverHost(server) } : undefined;
}

// ------------------------------------------------------------------ the one-time reveal

export type Reveal =
  | { kind: "runner"; reason: "created" | "rotated"; runnerId: string; runnerName: string; token: string }
  | { kind: "dedicated"; spaceId: string; spaceName: string; token: string }
  /**
   * A device connection. `token` is the device token for a manual
   * metro-connector (unpackaged app only), or null in the packaged app: the
   * page then shows in-app setup, and the launcher's bridge gets the token
   * from the server itself, so the UI never holds it.
   */
  | { kind: "device"; reason: "created" | "rotated"; connectionId: string; connectionName: string; providerId: string; token: string | null };

/** The token just issued, until it is dismissed. See "Secrets" above. */
export const revealed = signal<Reveal | null>(null);
/** The revealed token is shown in clear (it starts masked). */
export const revealShown = signal(false);
export const revealCopied = signal(false);

export function dismissReveal() {
  batch(() => {
    revealed.set(null);
    revealShown.set(false);
    revealCopied.set(false);
  });
}

function reveal(next: Reveal) {
  batch(() => {
    revealShown.set(false);
    revealCopied.set(false);
    revealed.set(next);
  });
}

// ------------------------------------------------------------------ registered runners

export const runners = signal<RegisteredRunner[] | null>(null);
export const runnersError = signal<string | null>(null);
/** What is in flight: "create", "rotate:<id>", "delete:<id>". */
export const runnerBusy = signal<string | null>(null);
export const runnerNotice = signal<string | null>(null);
/** A rotate or delete waiting for confirmation, with what it affects. */
export const runnerConfirm = signal<{ action: "rotate" | "delete"; runnerId: string } | null>(null);
/** Spaces the user can see that a runner serves (for the confirmation). null: still looking. */
export const runnerImpact = signal<{ runnerId: string; spaces: string[] | null; partial: boolean } | null>(null);
let runnersSeq = 0;
let impactSeq = 0;

export async function loadRunners(): Promise<void> {
  const c = apiClient();
  if (!c) return;
  const seq = ++runnersSeq;
  const ok = sessionGuard();
  try {
    const list = await c.listRunners();
    if (!ok() || seq !== runnersSeq) return;
    batch(() => {
      runners.set(list);
      runnersError.set(null);
    });
  } catch (error) {
    if (!ok() || seq !== runnersSeq) return;
    settingsError(error, runnersError, "Could not load runners");
  }
}

/** Put an acknowledged update behind the same read fence as runner creation/deletion. */
export function recordRunnerUpdateAccepted(id: string, pending: RunnerPending, valid: () => boolean): void {
  if (!valid()) return;
  runnersSeq++;
  const list = runners();
  const runner = list?.find((r) => r.id === id);
  const update = runner?.update;
  if (!list || !runner || !update) return;
  // A late POST acknowledgement must not roll back progress a roster read
  // has already reported. Later GETs replace this provisional pending value.
  if (runner.presence?.version === pending.target ||
      (update.pending && update.pending.requestedAt >= pending.requestedAt) ||
      (update.lastResult?.target === pending.target && update.lastResult.at > pending.requestedAt)) return;
  runners.set(list.map((r) => r.id === id ? { ...r, update: { ...update, pending } } : r));
}

export async function createRunner(nameInput: string, visibility: RunnerVisibility): Promise<boolean> {
  const name = nameInput.trim();
  const c = apiClient();
  if (!c || runnerBusy()) return false;
  if (!name) {
    runnersError.set("Give the runner a name, such as the machine it runs on.");
    return false;
  }
  if (visibility === "organization" && !orgAdmin()) {
    runnersError.set("Only organization admins can add organization runners.");
    return false;
  }
  const ok = sessionGuard();
  batch(() => {
    runnerBusy.set("create");
    runnersError.set(null);
    runnerNotice.set(null);
    runnerConfirm.set(null);
    dismissReveal();
  });
  try {
    const { runner, token } = await c.createRunner(name, visibility);
    if (!ok()) return false;
    runnersSeq++; // an older list answer must not drop the new runner
    runners.set([...(runners() ?? []).filter((r) => r.id !== runner.id), runner]);
    // Show the token only while the user is still on this page.
    if (onRunnersPage()) reveal({ kind: "runner", reason: "created", runnerId: runner.id, runnerName: runner.name, token });
    else runnerNotice.set(`Runner “${runner.name}” was added. Rotate its token to see one again.`);
    void loadRunners();
    return true;
  } catch (error) {
    if (ok()) settingsError(error, runnersError, "Could not add the runner");
    return false;
  } finally {
    if (ok()) runnerBusy.set(null);
  }
}

/** Asks to rotate or delete: shows the confirmation and looks up which visible Spaces it serves. */
export async function confirmRunnerAction(action: "rotate" | "delete", runnerId: string): Promise<void> {
  const c = apiClient();
  if (!c) return;
  const seq = ++impactSeq;
  const ok = sessionGuard();
  batch(() => {
    runnerConfirm.set({ action, runnerId });
    runnerImpact.set({ runnerId, spaces: null, partial: false });
    runnersError.set(null);
  });
  // Only Spaces this user belongs to can be checked; others may use an organization runner too.
  const checks = await Promise.allSettled(spaces().map(async (s) => ({ space: s, runner: await c.getSpaceRunner(s.id) })));
  if (!ok() || seq !== impactSeq) return;
  const using: string[] = [];
  let partial = false;
  for (const check of checks) {
    if (check.status === "rejected") partial = true;
    else if (check.value.runner?.kind === "registered" && check.value.runner.id === runnerId) using.push(check.value.space.name);
  }
  const runner = runners()?.find((r) => r.id === runnerId);
  runnerImpact.set({ runnerId, spaces: using, partial: partial || runner?.visibility === "organization" });
}

export function cancelRunnerAction() {
  impactSeq++;
  batch(() => {
    runnerConfirm.set(null);
    runnerImpact.set(null);
  });
}

export async function rotateRunner(runnerId: string): Promise<void> {
  const c = apiClient();
  if (!c || runnerBusy()) return;
  const runner = runners()?.find((r) => r.id === runnerId);
  const ok = sessionGuard();
  batch(() => {
    runnerBusy.set(`rotate:${runnerId}`);
    runnersError.set(null);
    runnerNotice.set(null);
    dismissReveal();
  });
  cancelRunnerAction();
  try {
    const token = await c.rotateRunnerToken(runnerId);
    if (!ok()) return;
    const name = runner?.name ?? "the runner";
    if (onRunnersPage()) reveal({ kind: "runner", reason: "rotated", runnerId, runnerName: name, token });
    else runnerNotice.set(`The token of “${name}” was rotated. Rotate again to see a new one.`);
  } catch (error) {
    if (ok()) settingsError(error, runnersError, "Could not rotate the runner's token", () => void loadRunners());
  } finally {
    if (ok()) runnerBusy.set(null);
  }
}

export async function deleteRunner(runnerId: string): Promise<void> {
  const c = apiClient();
  if (!c || runnerBusy()) return;
  const runner = runners()?.find((r) => r.id === runnerId);
  const ok = sessionGuard();
  batch(() => {
    runnerBusy.set(`delete:${runnerId}`);
    runnersError.set(null);
    runnerNotice.set(null);
  });
  cancelRunnerAction();
  try {
    await c.deleteRunner(runnerId);
    if (!ok()) return;
    runnersSeq++;
    batch(() => {
      runners.set((runners() ?? []).filter((r) => r.id !== runnerId));
      const shown = revealed();
      if (shown?.kind === "runner" && shown.runnerId === runnerId) dismissReveal();
      runnerNotice.set(`Runner “${runner?.name ?? runnerId}” was deleted. It no longer serves any Space.`);
      // Spaces it served now have no runner; the next Space list refresh confirms it.
      const cached = spaceRunners();
      const next: Record<string, SpaceRunner | null> = {};
      for (const [sid, r] of Object.entries(cached)) next[sid] = r?.kind === "registered" && r.id === runnerId ? null : r;
      spaceRunners.set(next);
      for (const [sid, r] of Object.entries(cached)) if (r?.kind === "registered" && r.id === runnerId) setRunnerPaired(sid, false);
    });
    void loadRunners();
  } catch (error) {
    if (ok()) settingsError(error, runnersError, "Could not delete the runner", () => void loadRunners());
  } finally {
    if (ok()) runnerBusy.set(null);
  }
}

const onRunnersPage = () => place() === "settings" && settingsSection() === "runners";

// ------------------------------------------------------------------ a Space's runner (owner's Manage view)

/** Who serves each Space, as last loaded. Missing: not loaded yet. */
export const spaceRunners = signal<Record<string, SpaceRunner | null>>({});
export const activeSpaceRunner = computed(() => {
  const sid = activeSpaceId();
  if (!sid) return undefined;
  const all = spaceRunners();
  return Object.prototype.hasOwnProperty.call(all, sid) ? all[sid] : undefined;
});
/** The server has no registered runners (an older version): the Manage view falls back to the dedicated token only. */
export const registeredRunnersSupported = signal(true);
/** What is in flight in the Manage view: "attach:<id>", "detach", "issue", "revoke". */
export const spaceRunnerBusy = signal<string | null>(null);
export const spaceRunnerNotice = signal<string | null>(null);
let spaceRunnerSeq = 0;

export async function loadSpaceRunner(spaceId: string): Promise<void> {
  const c = apiClient();
  if (!c) return;
  const seq = ++spaceRunnerSeq;
  const ok = sessionGuard(spaceId);
  try {
    const runner = await c.getSpaceRunner(spaceId);
    if (!ok() || seq !== spaceRunnerSeq) return;
    batch(() => {
      registeredRunnersSupported.set(true);
      spaceRunners.set({ ...spaceRunners(), [spaceId]: runner });
    });
  } catch (error) {
    if (!ok() || seq !== spaceRunnerSeq || isAborted(error)) return;
    // 404 for a Space we're still in: this server predates registered runners.
    if (error instanceof ApiError && error.kind === "not_found" && spaces().some((s) => s.id === spaceId)) {
      const legacy = spaces().find((s) => s.id === spaceId)?.runnerPaired;
      batch(() => {
        registeredRunnersSupported.set(false);
        spaceRunners.set({ ...spaceRunners(), [spaceId]: legacy ? { kind: "dedicated", name: "Dedicated Space runner" } : null });
      });
      return;
    }
    handleManageError(error, "Could not load this Space's runner", spaceId);
  }
}

/** Attaches a registered runner to the active Space (owner only). Replaces nothing: detach first. */
export async function attachRunner(runnerId: string): Promise<void> {
  const sid = activeSpaceId();
  const c = apiClient();
  if (!sid || !c || spaceRunnerBusy()) return;
  const ok = sessionGuard(sid);
  batch(() => {
    spaceRunnerBusy.set(`attach:${runnerId}`);
    spaceRunnerNotice.set(null);
    dismissReveal();
  });
  try {
    const attached = await c.attachRunner(sid, runnerId);
    if (!ok()) return;
    spaceRunnerSeq++;
    const runner = runners()?.find((r) => r.id === runnerId);
    const value: SpaceRunner | null = attached ?? (runner ? { kind: "registered", id: runner.id, name: runner.name, visibility: runner.visibility } : null);
    batch(() => {
      spaceRunners.set({ ...spaceRunners(), [sid]: value });
      setRunnerPaired(sid, true);
      spaceRunnerNotice.set(`Attached “${value?.kind === "registered" ? value.name : runnerId}”. Jobs from this Space go to it from now on, in its own folder for this Space.`);
    });
    if (!attached) void loadSpaceRunner(sid);
  } catch (error) {
    if (ok()) handleManageError(error, "Could not attach the runner", sid);
  } finally {
    if (ok()) spaceRunnerBusy.set(null);
  }
}

/** Detaches the active Space's registered runner. The runner keeps serving its other Spaces. */
export async function detachRunner(): Promise<void> {
  const sid = activeSpaceId();
  const c = apiClient();
  if (!sid || !c || spaceRunnerBusy()) return;
  const ok = sessionGuard(sid);
  batch(() => {
    spaceRunnerBusy.set("detach");
    spaceRunnerNotice.set(null);
  });
  try {
    await c.detachRunner(sid);
    if (!ok()) return;
    spaceRunnerSeq++;
    batch(() => {
      spaceRunners.set({ ...spaceRunners(), [sid]: null });
      setRunnerPaired(sid, false);
      spaceRunnerNotice.set("Runner detached. It no longer gets jobs from this Space; its other Spaces are unchanged.");
    });
  } catch (error) {
    if (ok()) handleManageError(error, "Could not detach the runner", sid);
  } finally {
    if (ok()) spaceRunnerBusy.set(null);
  }
}

/** Issues or rotates the Space's dedicated (single-Space) token and shows it once. */
export async function issueDedicatedToken(): Promise<void> {
  const sid = activeSpaceId();
  const c = apiClient();
  if (!sid || !c || spaceRunnerBusy()) return;
  const ok = sessionGuard(sid);
  batch(() => {
    spaceRunnerBusy.set("issue");
    spaceRunnerNotice.set(null);
    dismissReveal();
  });
  try {
    const token = await c.issueRunnerToken(sid);
    if (!ok()) return;
    spaceRunnerSeq++;
    batch(() => {
      spaceRunners.set({ ...spaceRunners(), [sid]: { kind: "dedicated", name: "Dedicated Space runner" } });
      setRunnerPaired(sid, true);
    });
    // Show it only if the owner is still looking at this Space's management view.
    if (activeSpaceId() === sid && view() === "manage" && place() === "space") {
      reveal({ kind: "dedicated", spaceId: sid, spaceName: activeSpace()?.name ?? sid, token });
    }
  } catch (error) {
    if (ok()) handleManageError(error, "Could not issue a runner token", sid);
  } finally {
    if (ok()) spaceRunnerBusy.set(null);
  }
}

export async function revokeDedicatedToken(): Promise<void> {
  const sid = activeSpaceId();
  const c = apiClient();
  if (!sid || !c || spaceRunnerBusy()) return;
  const ok = sessionGuard(sid);
  batch(() => {
    spaceRunnerBusy.set("revoke");
    spaceRunnerNotice.set(null);
    dismissReveal();
  });
  try {
    await c.revokeRunnerToken(sid);
    if (!ok()) return;
    spaceRunnerSeq++;
    batch(() => {
      spaceRunners.set({ ...spaceRunners(), [sid]: null });
      setRunnerPaired(sid, false);
      spaceRunnerNotice.set("Runner token revoked. The runner can no longer serve this Space.");
    });
  } catch (error) {
    if (ok()) handleManageError(error, "Could not revoke the runner token", sid);
  } finally {
    if (ok()) spaceRunnerBusy.set(null);
  }
}

/** Registered runners this owner may attach: their own personal ones, and the organization's. */
export const attachableRunners = computed(() => (runners() ?? []).filter((r) => r.visibility === "organization" || r.canManage));

// ------------------------------------------------------------------ model connections

export const catalog = signal<ProviderCatalog | null>(null);
export const catalogError = signal<string | null>(null);
export const connections = signal<ProviderConnection[] | null>(null);
export const connectionsError = signal<string | null>(null);
/** The technical text behind `connectionsError` (HTTP status, server wording), shown only on request. */
export const connectionsErrorDetail = signal<string | null>(null);
/** What is in flight: "create", "patch:<id>", "delete:<id>", "token:<id>", "model:<id>". */
export const providerBusy = signal<string | null>(null);
export const providerNotice = signal<string | null>(null);
export const providerConfirmDelete = signal<string | null>(null);

export type ModelListing =
  | { state: "loading" }
  /** `added` / `unavailable`: what the refresh changed in the connection's list. */
  | { state: "ok"; models: string[]; added?: number; unavailable?: number }
  /**
   * `message` is one plain sentence; `detail` the technical text behind it
   * (shown only under Technical details). `offline`: a device connection
   * whose metro-connector didn't answer.
   */
  | { state: "error"; message: string; offline: boolean; detail?: string };
/** The last model listing per connection id. */
export const modelListings = signal<Record<string, ModelListing>>({});
/** Paired / online per device connection, as last loaded; `error` when it couldn't be read. */
export const deviceStatuses = signal<Record<string, DeviceStatus | { error: string }>>({});
/** A device connection waiting for "Unpair" to be confirmed. */
export const deviceConfirmUnpair = signal<string | null>(null);
const statusSeq = new Map<string, number>();
let catalogSeq = 0;
let connectionsSeq = 0;
const modelsSeq = new Map<string, number>();

export const providerById = (id: string): CatalogProvider | undefined => catalog()?.providers.find((p) => p.id === id);
export const isDeviceConnection = (c: ProviderConnection) => (providerById(c.providerId)?.execution ?? (c.providerId.startsWith("device-") ? "device" : "server")) === "device";
/** What a connection serves: as the server says, else as its catalog entry says, else chat (an older server). */
export const connectionCapability = (c: ProviderConnection): Capability => c.capability ?? providerById(c.providerId)?.capability ?? "chat";

export const CAPABILITY_LABEL: Record<Capability, string> = { chat: "Chat", system_one: "System One", embeddings: "Embeddings" };
/** What a default of each capability is used for, in a sentence. */
export const CAPABILITY_USE: Record<Capability, string> = {
  chat: "answers and tool turns of runs you start",
  system_one: "typed System One decisions (routing, opt-in classification, search rerank)",
  embeddings: "indexing and searching the messages you can read",
};

/** Loads the catalog and the user's connections together (the Model providers page). */
export async function loadProviders(): Promise<void> {
  await Promise.all([loadCatalog(), loadConnections()]);
}

async function loadCatalog(): Promise<void> {
  const c = apiClient();
  if (!c) return;
  const seq = ++catalogSeq;
  const ok = sessionGuard();
  try {
    const value = await readStartupProviderCatalog(c);
    if (!ok() || seq !== catalogSeq) return;
    batch(() => {
      catalog.set(value);
      catalogError.set(null);
    });
  } catch (error) {
    if (!ok() || seq !== catalogSeq) return;
    settingsError(error, catalogError, "Could not load the providers this server offers");
  }
}

export async function loadConnections(): Promise<void> {
  await readConnections();
}

/** Returns only an authoritative list applied to this session, not a cached one. */
async function readConnections(confirmEndpointOf?: string): Promise<ProviderConnection[] | null> {
  const c = apiClient();
  if (!c) return null;
  const seq = ++connectionsSeq;
  const ok = sessionGuard();
  try {
    const list = await c.listProviders(confirmEndpointOf);
    if (!ok() || seq !== connectionsSeq) return null;
    batch(() => {
      connections.set(list);
      clearProviderError();
    });
    for (const x of list) if (isDeviceConnection(x)) void loadDeviceStatus(x.id);
    return list;
  } catch (error) {
    if (ok() && seq === connectionsSeq) providerError(error, "Could not load your connections");
    return null;
  }
}

/**
 * Creates a personal connection. A device connection then gets its token for
 * metro-connector, shown once; `fetchModels` lists a server profile's models
 * right away (Save & fetch models).
 */
export async function createConnection(body: NewProviderConnection, opts: { fetchModels: boolean }): Promise<ProviderConnection | null> {
  const c = apiClient();
  if (!c || providerBusy()) return null;
  const ok = sessionGuard();
  batch(() => {
    providerBusy.set("create");
    clearProviderError();
    providerNotice.set(null);
    providerConfirmDelete.set(null);
    dismissReveal();
  });
  try {
    const listing = opts.fetchModels && providerById(body.provider_id)?.execution !== "device" && providerById(body.provider_id)?.listsModels !== false;
    const created = await c.createProvider(listing ? { ...body, fetch_models: true } : body);
    if (!ok()) return null;
    connectionsSeq++;
    connections.set([...(connections() ?? []).filter((x) => x.id !== created.id), created]);
    const device = isDeviceConnection(created);
    if (device && packaged()) {
      // In-app setup: nothing is issued until the user agrees to pair this computer.
      if (onProvidersPage()) reveal({ kind: "device", reason: "created", connectionId: created.id, connectionName: created.name, providerId: created.providerId, token: null });
      else providerNotice.set(`“${created.name}” was added. Open it and press Set up on this computer.`);
      void loadDeviceStatus(created.id);
    } else if (device) {
      try {
        const token = await c.issueDeviceToken(created.id);
        if (!ok()) return null;
        void loadDeviceStatus(created.id);
        if (onProvidersPage()) {
          reveal({ kind: "device", reason: "created", connectionId: created.id, connectionName: created.name, providerId: created.providerId, token });
        } else providerNotice.set(`“${created.name}” was added. Open it and press New device token to set up metro-connector.`);
      } catch (error) {
        if (ok()) providerError(error, `“${created.name}” was saved, but its device token could not be issued; press New device token to try again`);
      }
    } else {
      const n = created.models.filter((m) => m.enabled).length;
      const models = n > 0 ? ` ${n} model${n === 1 ? "" : "s"} ready.` : "";
      providerNotice.set(`Saved “${created.name}”.${models}${created.hasKey ? " Its key is stored encrypted and is never shown again." : ""}`);
    }
    if (created.modelsError) {
      modelListings.set({
        ...modelListings(),
        [created.id]: { state: "error", offline: false, message: "Saved, but its models couldn't be listed. Check the key and endpoint, then press Refresh models, or add a model by hand.", detail: created.modelsError },
      });
    } else if (listing) {
      modelListings.set({ ...modelListings(), [created.id]: { state: "ok", models: created.models.map((m) => m.modelId), added: created.models.length } });
    }
    void loadConnections();
    return created;
  } catch (error) {
    if (ok()) providerError(error, "Could not save the connection");
    return null;
  } finally {
    if (ok()) providerBusy.set(null);
  }
}

/**
 * Lists a connection's models again and merges them into its list (new ones
 * enabled, ones no longer listed marked unavailable; nothing of the owner's
 * is dropped). For a device connection this is also its connection test: it
 * answers only while its metro-connector is online.
 */
export async function fetchModels(id: string, afterEndpointChange = false): Promise<void> {
  const c = apiClient();
  if (!c || endpointNeedsReload(id)) return;
  const seq = (modelsSeq.get(id) ?? 0) + 1;
  modelsSeq.set(id, seq);
  const ok = sessionGuard();
  modelListings.set({ ...modelListings(), [id]: { state: "loading" } });
  const known = connections()?.find((x) => x.id === id);
  if (known && isDeviceConnection(known)) void loadDeviceStatus(id);
  try {
    const result = await c.refreshConnectionModels(id, afterEndpointChange);
    if (!ok() || modelsSeq.get(id) !== seq) return;
    batch(() => {
      modelListings.set({ ...modelListings(), [id]: { state: "ok", models: result.models, added: result.added, unavailable: result.unavailable } });
      if (result.connection) replaceConnection(result.connection);
    });
    if (!result.connection) void loadConnections();
    // A test that went through the device: this computer's state may have moved on too (status only).
    if (known && isDeviceConnection(known) && packaged()) void refreshHelper();
  } catch (error) {
    if (!ok() || modelsSeq.get(id) !== seq || isAborted(error)) return;
    if (error instanceof ApiError && error.kind === "auth") {
      disconnect(error.message);
      return;
    }
    const connection = connections()?.find((x) => x.id === id);
    const device = connection ? isDeviceConnection(connection) : false;
    if (error instanceof ApiError && error.kind === "not_found") {
      modelListings.set({ ...modelListings(), [id]: { state: "error", message: "This connection no longer exists.", offline: false } });
      void loadConnections();
      return;
    }
    const detail = error instanceof ApiError ? error.message : describe(error, "error");
    const message = device && packaged()
      ? localConnections()[id]
        ? "This device didn't answer. Check that the computer serving it is online, or finish its setup here."
        : "This device isn't set up here yet."
      : device
      ? "metro-connector for this connection didn't answer. Start it on the computer you set it up on (metro-connector run), then test again."
      : error instanceof ApiError && !error.transient
        ? "This endpoint didn't list its models. Add the model you want by hand."
        : "Couldn't reach the endpoint to list its models. Try again, or add a model by hand.";
    modelListings.set({ ...modelListings(), [id]: { state: "error", message, offline: device, detail } });
  }
}

/** Puts the server's latest view of one connection in the list (its models included). */
function replaceConnection(next: ProviderConnection) {
  connectionsSeq++;
  const list = connections() ?? [];
  connections.set(list.some((x) => x.id === next.id) ? list.map((x) => (x.id === next.id ? { ...next, visibility: next.visibility ?? x.visibility } : x)) : [...list, next]);
}

/** Applies a model-list answer to its connection: the models, and the default model they name. */
function applyModelList(list: ConnectionModelList) {
  connectionsSeq++;
  connections.set(
    (connections() ?? []).map((x) =>
      x.id === list.connectionId ? { ...x, models: list.models, customThinking: list.customThinking, model: list.defaultModel ?? x.model } : x,
    ),
  );
}

/** A connection's models in the order the server keeps them. */
export const connectionModels = (c: ProviderConnection): ConnectionModel[] => [...c.models].sort((a, b) => a.position - b.position);

/** The connection's default model's name as pickers show it, else its id, else null. */
export function defaultModelName(c: ProviderConnection): string | null {
  const m = c.models.find((x) => x.isDefault) ?? c.models.find((x) => x.modelId === c.model);
  return m?.displayName ?? c.model;
}

async function modelListChange(id: string, run: (c: NonNullable<ReturnType<typeof apiClient>>) => Promise<ConnectionModelList>, what: string, done?: string): Promise<boolean> {
  const c = apiClient();
  if (!c || providerBusy()) return false;
  const ok = sessionGuard();
  batch(() => {
    providerBusy.set(`model:${id}`);
    clearProviderError();
    providerNotice.set(null);
  });
  try {
    const list = await run(c);
    if (!ok()) return false;
    batch(() => {
      applyModelList(list);
      if (done) providerNotice.set(done);
    });
    return true;
  } catch (error) {
    if (ok()) providerError(error, what, () => void loadConnections());
    return false;
  } finally {
    if (ok()) providerBusy.set(null);
  }
}

/** Adds a model by hand (enabled). */
export function addConnectionModel(id: string, model: NewConnectionModel): Promise<boolean> {
  const body: NewConnectionModel = { model_id: model.model_id.trim() };
  if (model.display_name?.trim()) body.display_name = model.display_name.trim();
  if (model.thinking && model.thinking.efforts.length > 0 && model.thinking.wire !== "none") body.thinking = model.thinking;
  if (!body.model_id) {
    connectionsError.set("Enter the model ID your provider documents.");
    return Promise.resolve(false);
  }
  return modelListChange(id, (c) => c.addConnectionModel(id, body), "Couldn't add the model", `Added ${body.display_name ?? body.model_id}.`);
}

/** Renames, switches on or off, changes thinking options, or makes a model the connection's default. */
export function patchConnectionModel(id: string, patch: ConnectionModelPatch, done?: string): Promise<boolean> {
  return modelListChange(id, (c) => c.patchConnectionModel(id, patch), "Couldn't save the model change", done);
}

/** Removes a model added by hand. */
export function removeConnectionModel(id: string, modelId: string): Promise<boolean> {
  return modelListChange(id, (c) => c.removeConnectionModel(id, modelId), "Couldn't remove the model");
}

/** Saves a change to a connection: its model, name, default or a replacement key. */
export async function updateConnection(id: string, patch: ProviderPatch, done?: string): Promise<boolean> {
  const c = apiClient();
  if (!c || providerBusy()) return false;
  if (patch.model !== undefined && !patch.model.trim()) {
    connectionsError.set("Enter a model ID, or pick one from the list.");
    return false;
  }
  const ok = sessionGuard();
  batch(() => {
    providerBusy.set(`patch:${id}`);
    clearProviderError();
    providerNotice.set(null);
  });
  try {
    await c.updateProvider(id, patch.model !== undefined ? { ...patch, model: patch.model.trim() } : patch);
    if (!ok()) return false;
    connectionsSeq++;
    const changed = connections()?.find((x) => x.id === id);
    const capability = changed ? connectionCapability(changed) : null;
    connections.set(
      (connections() ?? []).map((x) => {
        if (x.id === id) {
          return {
            ...x,
            name: patch.name ?? x.name,
            model: patch.model !== undefined ? patch.model.trim() : x.model,
            isDefault: patch.is_default ?? x.isDefault,
            hasKey: patch.api_key !== undefined ? patch.api_key !== "" : x.hasKey,
          };
        }
        // One default per user and capability: a System One default never unsets the chat one.
        return patch.is_default && connectionCapability(x) === capability ? { ...x, isDefault: false } : x;
      }),
    );
    if (done) providerNotice.set(done);
    void loadConnections();
    return true;
  } catch (error) {
    if (ok()) providerError(error, "Could not save the change", () => void loadConnections());
    return false;
  } finally {
    if (ok()) providerBusy.set(null);
  }
}

type EndpointResult =
  | { kind: "ignored" | "not-approved" | "unconfirmed" }
  | { kind: "refused"; message: string; retry: boolean; detail?: string }
  | { kind: "changed"; listing?: ModelListing };

type EndpointCheck = {
  from: string | null; to: string; detail?: string;
  refusal?: "conflict" | "missing";
  phase: "unknown" | "reload-failed" | "saved" | "not-settled" | "review";
};
/** A check survives editor closes/remounts. Only positive refusal evidence allows review instead of waiting for a possible commit. */
export const endpointChecks = signal<Partial<Record<string, EndpointCheck>>>({});

export function endpointNeedsReload(id: string): boolean {
  const phase = endpointChecks()[id]?.phase;
  return phase === "unknown" || phase === "reload-failed" || phase === "not-settled";
}

export function dismissEndpointCheck(id: string) {
  if (!endpointChecks()[id] || endpointNeedsReload(id)) return;
  const checks = { ...endpointChecks() };
  delete checks[id];
  endpointChecks.set(checks);
}

function invalidateEndpointListing(id: string) {
  modelsSeq.set(id, (modelsSeq.get(id) ?? 0) + 1);
  const listings = { ...modelListings() };
  delete listings[id];
  modelListings.set(listings);
}

export async function reloadEndpoint(id: string): Promise<void> {
  const check = endpointChecks()[id];
  if (!check || !endpointNeedsReload(id) || providerBusy() || !apiClient()) return;
  const ok = sessionGuard();
  providerBusy.set(`endpoint-reload:${id}`);
  try {
    const list = await readConnections(id);
    if (!ok()) return;
    if (!list) {
      endpointChecks.set({ ...endpointChecks(), [id]: { ...check, phase: "reload-failed" } });
      return;
    }
    const connection = list.find((x) => x.id === id) ?? null;
    batch(() => {
      if (connection?.baseUrl !== check.from) invalidateEndpointListing(id);
      const checks = { ...endpointChecks() };
      if (!connection) {
        delete checks[id];
        connectionsError.set("This connection is no longer available to edit.");
      } else if (check.refusal) {
        // A definite refusal never saved the draft, even if someone else
        // has since chosen the same URL. Always ask for fresh review.
        checks[id] = { ...check, phase: "review" };
      } else {
        // A GET is a snapshot, not proof an uncertain PATCH has stopped.
        checks[id] = { ...check, phase: connection.baseUrl === check.to ? "saved" : "not-settled" };
      }
      endpointChecks.set(checks);
    });
  } finally {
    if (ok()) providerBusy.set(null);
  }
}

/** Keep the committed endpoint in state until the server confirms a successful test and save. */
export async function updateEndpoint(id: string, url: string, revision: string): Promise<EndpointResult> {
  const c = apiClient();
  const current = connections()?.find((x) => x.id === id);
  const provider = current && providerById(current.providerId);
  if (!c || !current || !provider?.allowCustomUrl || isDeviceConnection(current) || providerBusy() || endpointNeedsReload(id) || url === current.baseUrl) {
    return { kind: "ignored" };
  }
  if (catalogError() || !isAllowedBaseUrl(url)) return { kind: "not-approved" };
  const ok = sessionGuard();
  batch(() => {
    dismissEndpointCheck(id);
    providerBusy.set(`endpoint:${id}`);
    clearProviderError();
    providerNotice.set(null);
  });
  try {
    const changed = await c.updateProviderEndpoint(id, url, revision);
    if (!ok()) return { kind: "ignored" };
    // Don't show a listing from the old endpoint, even if it finishes after the PATCH.
    batch(() => {
      invalidateEndpointListing(id);
      replaceConnection(changed);
      providerBusy.set(`endpoint-refresh:${id}`);
    });
    if (provider.listsModels) await fetchModels(id, true);
    if (!ok()) return { kind: "ignored" };
    return { kind: "changed", listing: modelListings()[id] };
  } catch (error) {
    if (!ok() || isAborted(error)) return { kind: "ignored" };
    if (error instanceof ApiError && error.kind === "auth") {
      disconnect(error.message);
      return { kind: "ignored" };
    }
    const status = error instanceof ApiError ? error.status : null;
    const detail = status ? `PATCH endpoint: HTTP ${status}` : undefined;
    let refusal: EndpointCheck["refusal"];
    if (error instanceof EndpointPatchError) {
      switch (error.code) {
        case "endpoint_not_approved":
          void loadCatalog();
          return { kind: "not-approved" };
        case "endpoint_probe_busy":
          return { kind: "refused", message: "The server has too many endpoint tests running. No change was saved. Try again shortly.", retry: true, detail };
        case "endpoint_probe_failed": {
          const reason = status === 504 ? "the test timed out" : "the connection test failed";
          return { kind: "refused", message: `The new endpoint didn't answer (${reason}). The connection still uses the old one.`, retry: true, detail };
        }
        case "endpoint_rejected":
          return { kind: "refused", message: `Metro didn't save the endpoint change: ${error.reason}`, retry: false, detail };
        case "endpoint_conflict":
          refusal = "conflict";
          break;
        case "endpoint_missing":
          refusal = "missing";
          break;
      }
    }
    batch(() => {
      // An old refresh's result can't later overwrite the reloaded endpoint.
      invalidateEndpointListing(id);
      endpointChecks.set({
        ...endpointChecks(), [id]: { phase: "unknown", refusal, from: current.baseUrl, to: url, detail: detail ?? "Endpoint response was not confirmed" },
      });
    });
    return { kind: "unconfirmed" };
  } finally {
    if (ok()) providerBusy.set(null);
  }
}

export async function deleteConnection(id: string): Promise<void> {
  const c = apiClient();
  if (!c || providerBusy()) return;
  const name = connections()?.find((x) => x.id === id)?.name ?? id;
  const ok = sessionGuard();
  batch(() => {
    providerBusy.set(`delete:${id}`);
    providerConfirmDelete.set(null);
    clearProviderError();
    providerNotice.set(null);
  });
  try {
    await c.deleteProvider(id);
    if (!ok()) return;
    connectionsSeq++;
    batch(() => {
      connections.set((connections() ?? []).filter((x) => x.id !== id));
      const { [id]: _gone, ...rest } = modelListings();
      modelListings.set(rest);
      const { [id]: _status, ...statuses } = deviceStatuses();
      deviceStatuses.set(statuses);
      const shown = revealed();
      if (shown?.kind === "device" && shown.connectionId === id) dismissReveal();
      providerNotice.set(`Deleted “${name}”. Its key or device token no longer works.`);
    });
    void forgetQuietly(id);
    void loadConnections();
  } catch (error) {
    if (ok()) providerError(error, "Could not delete the connection", () => void loadConnections());
  } finally {
    if (ok()) providerBusy.set(null);
  }
}

export async function loadDeviceStatus(id: string): Promise<void> {
  const c = apiClient();
  if (!c) return;
  const seq = (statusSeq.get(id) ?? 0) + 1;
  statusSeq.set(id, seq);
  const ok = sessionGuard();
  try {
    const value = await c.deviceStatus(id);
    if (!ok() || statusSeq.get(id) !== seq) return;
    const before = deviceStatuses()[id];
    deviceStatuses.set({ ...deviceStatuses(), [id]: value });
    // A device that just came online with no models yet: list them once, so
    // nobody has to press Refresh after pairing.
    const wasOnline = !!before && !("error" in before) && before.online;
    const connection = connections()?.find((x) => x.id === id);
    if (value.online && !wasOnline && connection && connection.models.length === 0 && modelListings()[id]?.state !== "loading") void fetchModels(id);
  } catch (error) {
    if (!ok() || statusSeq.get(id) !== seq || isAborted(error)) return;
    if (error instanceof ApiError && error.kind === "auth") {
      disconnect(error.message);
      return;
    }
    deviceStatuses.set({ ...deviceStatuses(), [id]: { error: error instanceof ApiError ? error.message : describe(error, "error") } });
  }
}

/** Unpairs a device connection: its token stops working; the connection and its model choice stay. */
export async function unpairDevice(id: string): Promise<void> {
  const c = apiClient();
  if (!c || providerBusy()) return;
  const name = connections()?.find((x) => x.id === id)?.name ?? id;
  const ok = sessionGuard();
  batch(() => {
    providerBusy.set(`unpair:${id}`);
    deviceConfirmUnpair.set(null);
    clearProviderError();
    providerNotice.set(null);
  });
  try {
    await c.revokeDeviceToken(id);
    if (!ok()) return;
    batch(() => {
      deviceStatuses.set({ ...deviceStatuses(), [id]: { paired: false, online: false, lastSeenSecs: null } });
      const shown = revealed();
      if (shown?.kind === "device" && shown.connectionId === id) dismissReveal();
      providerNotice.set(
        packaged()
          ? `Unpaired “${name}”. No computer can serve it until you set it up again.`
          : `Unpaired “${name}”. Its metro-connector can no longer serve it; press New device token to pair again.`,
      );
    });
    void forgetQuietly(id);
    void loadDeviceStatus(id);
  } catch (error) {
    if (ok()) providerError(error, "Could not unpair the device", () => void loadConnections());
  } finally {
    if (ok()) providerBusy.set(null);
  }
}

/**
 * Packaged app: opens the in-app setup of a device connection on this
 * computer (pair, sign in, models, re-pair, forget). Issues nothing by itself.
 */
export function openDeviceSetup(id: string) {
  const connection = connections()?.find((x) => x.id === id);
  if (!connection || !packaged()) return;
  batch(() => {
    clearProviderError();
    providerNotice.set(null);
    dismissReveal();
  });
  const paired = !!localConnections()[id];
  if (onProvidersPage()) reveal({ kind: "device", reason: paired ? "rotated" : "created", connectionId: id, connectionName: connection.name, providerId: connection.providerId, token: null });
  void refreshHelper();
}

/** Packaged app: opens in-app setup instead (see openDeviceSetup). Unpackaged: issues a new device token for metro-connector; the old one stops working. */
export async function rotateDeviceToken(id: string): Promise<void> {
  const c = apiClient();
  if (!c || providerBusy()) return;
  const connection = connections()?.find((x) => x.id === id);
  if (!connection) return;
  if (packaged()) {
    openDeviceSetup(id);
    return;
  }
  const ok = sessionGuard();
  batch(() => {
    providerBusy.set(`token:${id}`);
    clearProviderError();
    providerNotice.set(null);
    dismissReveal();
  });
  try {
    const token = await c.issueDeviceToken(id);
    if (!ok()) return;
    void loadDeviceStatus(id);
    if (onProvidersPage()) {
      reveal({ kind: "device", reason: "rotated", connectionId: id, connectionName: connection.name, providerId: connection.providerId, token });
    }
  } catch (error) {
    if (ok()) providerError(error, "Could not issue a device token", () => void loadConnections());
  } finally {
    if (ok()) providerBusy.set(null);
  }
}

const onProvidersPage = () => place() === "settings" && settingsSection() === "connections";

/** Whether `url` may be used as a custom base URL: exactly one of the server's allowed URLs. */
export function isAllowedBaseUrl(url: string): boolean {
  const allowed = catalog()?.allowedBaseUrls ?? [];
  return allowed.includes(url);
}

// ------------------------------------------------------------------ errors and lifecycle

function clearProviderError() {
  connectionsError.set(null);
  connectionsErrorDetail.set(null);
}

/** The server's own short reason in an HTTP error ("…failed with HTTP 409: <reason>"), else null. */
export function serverReason(error: unknown): string | null {
  if (!(error instanceof ApiError) || (error.kind !== "http" && error.kind !== "not_found" && error.kind !== "forbidden")) return null;
  const m = /HTTP \d{3}\)?: (.+?)\.?$/.exec(error.message);
  const reason = m?.[1]?.trim();
  return reason ? reason.charAt(0).toUpperCase() + reason.slice(1) : null;
}

/**
 * A provider change failed: one plain sentence in `connectionsError` (what
 * failed, and the server's reason when it gave one), and the technical text
 * (method, path, HTTP status) in `connectionsErrorDetail`, shown on request.
 */
function providerError(error: unknown, what: string, onGone?: () => void) {
  if (isAborted(error)) return;
  if (error instanceof ApiError && error.kind === "auth") {
    disconnect(error.message);
    return;
  }
  if (error instanceof ApiError && error.kind === "not_found" && onGone) onGone();
  const detail = error instanceof ApiError ? error.message : describe(error, what);
  const reason = serverReason(error);
  const plain =
    error instanceof ApiError && (error.kind === "network" || error.kind === "timeout")
      ? `${what}: the server didn't answer. Try again.`
      : reason
        ? `${what}. ${reason}.`
        : error instanceof ApiError
          ? `${what}.`
          : detail;
  batch(() => {
    connectionsError.set(plain);
    connectionsErrorDetail.set(error instanceof ApiError ? detail : null);
  });
}


function settingsError(error: unknown, target: { set(v: string | null): void }, what: string, onGone?: () => void) {
  if (isAborted(error)) return;
  if (error instanceof ApiError && error.kind === "auth") {
    disconnect(error.message);
    return;
  }
  if (error instanceof ApiError && error.kind === "not_found" && onGone) onGone();
  const detail = error instanceof ApiError ? error.message : describe(error, what);
  target.set(error instanceof ApiError ? `${what}: ${detail}` : detail);
}

// Anything shown once goes away whenever the user moves (place, Space, room, view, section).
onTransientClear(() => {
  dismissReveal();
  impactSeq++;
  runnerConfirm.set(null);
  runnerImpact.set(null);
  providerConfirmDelete.set(null);
  deviceConfirmUnpair.set(null);
});

// Opening a Settings page (again) loads what it shows, fresh.
effect(() => {
  const here = place() === "settings" ? settingsSection() : null;
  untracked(() => {
    if (here === "runners") void loadRunners();
    else if (here === "connections") {
      void loadProviders();
      void refreshHelper();
    }
  });
});

// A pairing on this computer changed: the server's paired/online view follows.
// Pairing just completed here: list the device's models at once (the user never presses Refresh).
onLocalPairingChanged((id) => {
  if (!connections()?.some((x) => x.id === id)) return;
  void loadDeviceStatus(id);
  if (localStep(localConnections()[id]) === "ready" && modelListings()[id]?.state !== "loading") void fetchModels(id);
});

onManageOpen((spaceId) => {
  spaceRunnerNotice.set(null);
  void loadSpaceRunner(spaceId);
  void loadRunners();
});

onSessionEnd(() => {
  runnersSeq++;
  impactSeq++;
  spaceRunnerSeq++;
  catalogSeq++;
  connectionsSeq++;
  modelsSeq.clear();
  statusSeq.clear();
  dismissReveal();
  runners.set(null);
  runnersError.set(null);
  runnerBusy.set(null);
  runnerNotice.set(null);
  runnerConfirm.set(null);
  runnerImpact.set(null);
  spaceRunners.set({});
  registeredRunnersSupported.set(true);
  spaceRunnerBusy.set(null);
  spaceRunnerNotice.set(null);
  catalog.set(null);
  catalogError.set(null);
  connections.set(null);
  endpointChecks.set({});
  clearProviderError();
  providerBusy.set(null);
  providerNotice.set(null);
  providerConfirmDelete.set(null);
  modelListings.set({});
  deviceStatuses.set({});
  deviceConfirmUnpair.set(null);
});
