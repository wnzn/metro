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
import { ApiError } from "./api/client";
import type {
  Capability,
  CatalogProvider,
  DeviceStatus,
  NewProviderConnection,
  ProviderCatalog,
  ProviderConnection,
  ProviderPatch,
  RegisteredRunner,
  RunnerVisibility,
  SpaceRunner,
} from "./api/client";
import {
  activeSpace,
  activeSpaceId,
  apiClient,
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
import { forgetQuietly, localConnections, onLocalPairingChanged, packaged, refreshHelper } from "./device/state";

export type { Capability, CatalogProvider, DeviceStatus, ProviderConnection, RegisteredRunner, RunnerVisibility, SpaceRunner };

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
/** What is in flight: "create", "patch:<id>", "delete:<id>", "token:<id>". */
export const providerBusy = signal<string | null>(null);
export const providerNotice = signal<string | null>(null);
export const providerConfirmDelete = signal<string | null>(null);

export type ModelListing =
  | { state: "loading" }
  | { state: "ok"; models: string[] }
  /** `offline`: a device connection whose metro-connector didn't answer. */
  | { state: "error"; message: string; offline: boolean };
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
    const value = await c.providerCatalog();
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
  const c = apiClient();
  if (!c) return;
  const seq = ++connectionsSeq;
  const ok = sessionGuard();
  try {
    const list = await c.listProviders();
    if (!ok() || seq !== connectionsSeq) return;
    batch(() => {
      connections.set(list);
      connectionsError.set(null);
    });
    for (const x of list) if (isDeviceConnection(x)) void loadDeviceStatus(x.id);
  } catch (error) {
    if (!ok() || seq !== connectionsSeq) return;
    settingsError(error, connectionsError, "Could not load your connections");
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
    connectionsError.set(null);
    providerNotice.set(null);
    providerConfirmDelete.set(null);
    dismissReveal();
  });
  try {
    const created = await c.createProvider(body);
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
        if (ok()) settingsError(error, connectionsError, `“${created.name}” was saved, but its device token could not be issued; press New device token to try again`);
      }
    } else {
      providerNotice.set(`Saved “${created.name}”.${created.hasKey ? " Its key is stored encrypted and is never shown again." : ""}`);
    }
    void loadConnections();
    if (opts.fetchModels && !device) void fetchModels(created.id);
    return created;
  } catch (error) {
    if (ok()) settingsError(error, connectionsError, "Could not save the connection");
    return null;
  } finally {
    if (ok()) providerBusy.set(null);
  }
}

/** Lists a connection's models. A device connection answers only while its metro-connector is online. */
export async function fetchModels(id: string): Promise<void> {
  const c = apiClient();
  if (!c) return;
  const seq = (modelsSeq.get(id) ?? 0) + 1;
  modelsSeq.set(id, seq);
  const ok = sessionGuard();
  modelListings.set({ ...modelListings(), [id]: { state: "loading" } });
  const known = connections()?.find((x) => x.id === id);
  if (known && isDeviceConnection(known)) void loadDeviceStatus(id);
  try {
    const models = await c.listProviderModels(id);
    if (!ok() || modelsSeq.get(id) !== seq) return;
    modelListings.set({ ...modelListings(), [id]: { state: "ok", models } });
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
      ? `This connection's device didn't answer. ${localConnections()[id] ? "Finish its setup on this computer (Set up on this computer), or check that the computer serving it is online." : "It isn't set up on this computer: press Set up on this computer, or start it where it was paired."} (${detail})`
      : device
      ? `metro-connector for this connection didn't answer. Start it on the computer you set it up on (metro-connector run), finish signing in there, then test again. (${detail})`
      : error instanceof ApiError && !error.transient
        ? `This endpoint didn't list its models, so pick nothing from a list: enter the model ID by hand below. (${detail})`
        : `Couldn't reach the endpoint to list models. Try again, or enter the model ID by hand. (${detail})`;
    modelListings.set({ ...modelListings(), [id]: { state: "error", message, offline: device } });
  }
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
    connectionsError.set(null);
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
    if (ok()) settingsError(error, connectionsError, "Could not save the change", () => void loadConnections());
    return false;
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
    connectionsError.set(null);
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
    if (ok()) settingsError(error, connectionsError, "Could not delete the connection", () => void loadConnections());
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
    deviceStatuses.set({ ...deviceStatuses(), [id]: value });
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
    connectionsError.set(null);
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
    if (ok()) settingsError(error, connectionsError, "Could not unpair the device", () => void loadConnections());
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
    connectionsError.set(null);
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
    connectionsError.set(null);
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
    if (ok()) settingsError(error, connectionsError, "Could not issue a device token", () => void loadConnections());
  } finally {
    if (ok()) providerBusy.set(null);
  }
}

const onProvidersPage = () => place() === "settings" && settingsSection() === "providers";

/** Whether `url` may be used as a custom base URL: exactly one of the server's allowed URLs. */
export function isAllowedBaseUrl(url: string): boolean {
  const allowed = catalog()?.allowedBaseUrls ?? [];
  return allowed.includes(url.trim());
}

// ------------------------------------------------------------------ errors and lifecycle

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
    else if (here === "providers") {
      void loadProviders();
      void refreshHelper();
    }
  });
});

// A pairing on this computer changed: the server's paired/online view follows.
onLocalPairingChanged((id) => {
  if (connections()?.some((x) => x.id === id)) void loadDeviceStatus(id);
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
  connectionsError.set(null);
  providerBusy.set(null);
  providerNotice.set(null);
  providerConfirmDelete.set(null);
  modelListings.set({});
  deviceStatuses.set({});
  deviceConfirmUnpair.set(null);
});
