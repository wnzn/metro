// Space settings (opened per Space, over the app): its name and description,
// deleting or leaving it, its agents, and the signed-in person's own
// preferences there. The pages read these signals and call these functions.
//
// - The owner edits the Space, membership, runner and agents. Everyone can
//   read them and edit their own preferences. The server is the judge.
// - Deleting needs the Space's exact name typed; leaving is for members
//   other than the owner. Both then forget the Space (store.spaceGone).
// - An agent's picture comes only from the system's file picker, PNG or
//   JPEG, checked by its bytes before it is sent (like Settings › Profile).
// - Preferences take effect when changed (no Save). Model choices are only
//   the person's own usable chat connections, or ones shared with them here.

import { batch, cleanup, computed, effect, Picker as SystemPicker, signal, untracked } from "nsh";
import { ApiError } from "../../api/client";
import type { AvailableCatalog, Client, Member, RegisteredRunner, SpaceRunner } from "../../api/client";
import { AVATAR_EXTENSIONS, MAX_AVATAR_BYTES, sniffAvatar } from "../../api/profile";
import type { AvatarMime } from "../../api/profile";
import {
  AGENT_COLORS,
  handleProblem,
  MAX_AGENT_DESCRIPTION,
  MAX_AGENT_INSTRUCTIONS,
  MAX_AGENT_NAME,
  MAX_SPACE_AGENTS,
  MAX_SPACE_DESCRIPTION,
} from "../../api/spaces";
import type { AgentPatch, ModelSelection, SpaceAgent, SpaceDetails, SpacePrefs, SpacePrefsView } from "../../api/spaces";
import { apiClient, closeSettings, describe, disconnect, isAborted, me, onManageOpen, onSessionEnd, onTransientClear, rememberMembers, sessionGuard, spaceGone, spaceRenamed, spaces } from "../../store";
import { acknowledgeSpaceRunnerWrite, beginSpaceRunnerWrite, dismissReveal, revealDedicatedToken, revealed, spaceRunners } from "../../settings";
import { beginSpaceRunnerObservation } from "../../spaceRunnerObservations";
import { spaceRunnerExactPending } from "../../spaceRunnerOrder";
import { dismissRunnerOffer, installedRunnerChoice, snapshotInstalledRunner } from "../../runnersState";
import { clearLocalToolsMessages, localToolsBusy } from "../localtools/localTools";
import { noteClassifierPrefs } from "./askAgent";
import { adoptAgent, agentList, dropAgent, ensureSpaceAgents, forgetSpaceAgents, invalidateSpaceAgents, isOptInTool, showPicture, spaceAgents } from "./agents";

export type SpaceSettingsSection = "general" | "members" | "runner" | "agents" | "preferences";

/** The Space whose settings are open, or null. */
export const spaceSettingsFor = signal<string | null>(null);
export const spaceSettingsSection = signal<SpaceSettingsSection>("general");

/** The open Space as the Space list knows it (name, role). */
export function settingsSpace() {
  const sid = spaceSettingsFor();
  return sid ? spaces().find((s) => s.id === sid) ?? null : null;
}

// ---------------------------------------------------------------- state

export const details = signal<SpaceDetails | null>(null);
export const detailsError = signal<string | null>(null);
export const nameDraft = signal("");
export const descriptionDraft = signal("");
export const savingGeneral = signal(false);
/** The last outcome of an action on these pages, said once. */
export const spaceNotice = signal<{ ok: boolean; message: string } | null>(null);

export const roster = signal<Member[] | null>(null);
export const isSettingsOwner = computed(() => settingsSpace()?.role === "owner" && roster()?.find((m) => m.userId === me())?.role !== "member");
export const rosterError = signal<string | null>(null);
export const memberBusy = signal<string | null>(null);
export const memberNotice = signal<{ ok: boolean; message: string } | null>(null);

type RunnerSnapshot = { runner: SpaceRunner | null; legacy: boolean; guardedTokenChanges: boolean };
const runnerSource = signal<(Omit<RunnerSnapshot, "runner"> & { sid: string }) | null>(null);
export const runnerSnapshot = computed<RunnerSnapshot | null>(() => {
  const source = runnerSource();
  if (!source) return null;
  const runner = spaceRunners()[source.sid];
  return runner === undefined ? null : { runner, legacy: source.legacy, guardedTokenChanges: source.guardedTokenChanges };
});
export const runnerError = signal<string | null>(null);
export const runnerInventory = signal<{ attachIfEmpty: boolean; runners: RegisteredRunner[] } | null>(null);
export const runnerInventoryError = signal<string | null>(null);
export const runnerBusy = signal<string | null>(null);
export const runnerNotice = signal<{ ok: boolean; message: string } | null>(null);
export const runnerAdvanced = signal(false);

export const dangerOpen = signal<null | "delete" | "leave">(null);
export const deleteConfirmText = signal("");
export const dangerBusy = signal(false);
export const dangerError = signal<string | null>(null);

export type AgentDraft = {
  name: string;
  handle: string;
  /** The handle was typed, not suggested from the name. */
  handleTouched: boolean;
  description: string;
  instructions: string;
  tools: string[];
  color: string;
};
/** The agent being edited: a new one, or an existing one by id. */
export const editing = signal<null | { mode: "new" } | { mode: "edit"; id: string }>(null);
export const agentDraft = signal<AgentDraft | null>(null);
/** A picture picked for an agent that doesn't exist yet: sent right after it's created. */
export const pendingPicture = signal<{ bytes: Uint8Array; mime: AvatarMime } | null>(null);
export const savingAgent = signal(false);
export const agentError = signal<string | null>(null);
/** `${action}:${agentId}` while an agent action runs (default, delete, picture). */
export const agentBusy = signal<string | null>(null);
/** The agent whose deletion is being confirmed. */
export const confirmDeleteAgent = signal<string | null>(null);

export const prefs = signal<SpacePrefsView | null>(null);
export const prefsError = signal<string | null>(null);
/** The preference being saved ("classify", "routing", "default", or an agent id). */
export const prefsBusy = signal<string | null>(null);
/** The models this person can use in the Space (GET /v1/models/available?space_id=). */
export const catalog = signal<AvailableCatalog | null>(null);
export const catalogError = signal<string | null>(null);

let generation = 0;
let pageGeneration = 0;
let advancedGeneration = 0;
let memberRead = 0;
let inventoryRead = 0;

// ---------------------------------------------------------------- open and close

/** Opens a Space's settings at `section` (a member may open them; owner-only parts are hidden). */
export function openSpaceSettings(spaceId: string, section: SpaceSettingsSection = "general") {
  const space = spaces().find((s) => s.id === spaceId);
  if (!space) return;
  closeSettings();
  const changed = untracked(spaceSettingsFor) !== spaceId;
  batch(() => {
    if (changed) resetPages();
    else if (untracked(spaceSettingsSection) !== section) resetSection();
    spaceSettingsFor.set(spaceId);
    spaceSettingsSection.set(section);
  });
}

export function showSection(section: SpaceSettingsSection) {
  if (untracked(spaceSettingsSection) === section) return;
  batch(() => {
    resetSection();
    spaceSettingsSection.set(section);
  });
}

export function closeSpaceSettings() {
  if (!untracked(spaceSettingsFor)) return;
  batch(() => {
    spaceSettingsFor.set(null);
    resetPages();
  });
}

function resetSection() {
  pageGeneration++;
  memberBusy.set(null);
  memberNotice.set(null);
  runnerSource.set(null);
  runnerError.set(null);
  runnerInventory.set(null);
  runnerInventoryError.set(null);
  runnerBusy.set(null);
  runnerNotice.set(null);
  runnerAdvanced.set(false);
  clearLocalToolsMessages();
  if (untracked(revealed)?.kind === "dedicated") dismissReveal();
}

function resetPages() {
  generation++;
  batch(() => {
    resetSection();
    roster.set(null);
    rosterError.set(null);
    details.set(null);
    detailsError.set(null);
    nameDraft.set("");
    descriptionDraft.set("");
    savingGeneral.set(false);
    spaceNotice.set(null);
    dangerOpen.set(null);
    deleteConfirmText.set("");
    dangerBusy.set(false);
    dangerError.set(null);
    editing.set(null);
    agentDraft.set(null);
    pendingPicture.set(null);
    savingAgent.set(false);
    agentError.set(null);
    agentBusy.set(null);
    confirmDeleteAgent.set(null);
    prefs.set(null);
    prefsError.set(null);
    prefsBusy.set(null);
    catalog.set(null);
    catalogError.set(null);
  });
}

// Keep read-only pages visible to members; close them when the Space goes away.
effect(() => {
  const sid = spaceSettingsFor();
  if (!sid) return;
  const space = spaces().find((s) => s.id === sid);
  untracked(() => {
    if (!space) closeSpaceSettings();
  });
});

effect(() => {
  const sid = spaceSettingsFor();
  const section = spaceSettingsSection();
  if (!sid) return;
  untracked(() => {
    if (section === "general") {
      if (!details()) void loadDetails();
    } else if (section === "members") {
      let polling = false;
      const tick = async () => {
        if (polling || memberBusy()) return;
        polling = true;
        try { await loadMembers(); }
        finally { polling = false; }
      };
      void tick();
      const timer = setInterval(() => void tick(), 5_000);
      cleanup(() => clearInterval(timer));
    } else if (section === "runner") {
      void loadRunner();
    } else if (section === "agents") {
      void ensureSpaceAgents(sid, true);
    }
  });
});

effect(() => {
  const owner = isSettingsOwner();
  const section = spaceSettingsSection();
  untracked(() => {
    if (owner && section === "runner") void loadRunnerInventory();
    else if (!owner) {
      setRunnerAdvanced(false);
      runnerInventory.set(null);
      if (editing()) cancelAgentEdit();
    }
  });
});

/** A guard for work on the open Space: same session, same Space, same opening. */
function guardFor(sid: string) {
  const gen = generation;
  const ok = sessionGuard(sid);
  return () => ok() && gen === generation && spaceSettingsFor() === sid;
}

function guardPage(sid: string, section: SpaceSettingsSection) {
  const live = guardFor(sid);
  const page = pageGeneration;
  return () => live() && page === pageGeneration && spaceSettingsSection() === section;
}

onManageOpen(openSpaceSettings);
onTransientClear(closeSpaceSettings);

/** Handles an error of a Space request; true when it was handled (closed, signed out). */
function lost(error: unknown, sid: string): boolean {
  if (isAborted(error)) return true;
  if (error instanceof ApiError && error.kind === "auth") {
    disconnect(error.message);
    return true;
  }
  if (error instanceof ApiError && error.kind === "not_found") {
    // The Space is gone or no longer this user's to see.
    forgetSpaceAgents(sid);
    closeSpaceSettings();
    void spaceGone(sid, "That Space is no longer available.");
    return true;
  }
  return false;
}

/** The server's own words for an error, without the method and path. */
function reason(error: unknown, what: string): string {
  if (error instanceof ApiError && error.kind === "http" && error.status !== null) {
    const m = /failed with HTTP \d+: (.*)$/.exec(error.message);
    if (m) return `${what}: ${m[1]}`;
  }
  if (error instanceof ApiError && error.kind === "forbidden") return `${what}: only the Space's owner can change this.`;
  return describe(error, what);
}

// ---------------------------------------------------------------- members

export async function loadMembers(): Promise<void> {
  const api = apiClient();
  const sid = spaceSettingsFor();
  if (!api || !sid) return;
  const page = guardPage(sid, "members");
  const seq = ++memberRead;
  const live = () => page() && seq === memberRead;
  try {
    const list = await api.listMembers(sid);
    if (!live()) return;
    batch(() => {
      roster.set(list);
      rosterError.set(null);
      rememberMembers(sid, list);
    });
  } catch (error) {
    if (!live() || lost(error, sid)) return;
    rosterError.set(reason(error, "Couldn't load members"));
  }
}

export async function changeMember(action: "add" | "remove", input: string): Promise<boolean> {
  const api = apiClient();
  const sid = spaceSettingsFor();
  const uid = input.trim();
  if (!api || !sid || !isSettingsOwner() || spaceSettingsSection() !== "members" || memberBusy() || uid === me()) return false;
  if (!uid) {
    memberNotice.set({ ok: false, message: "Enter a user ID. The person can copy it from their account menu." });
    return false;
  }
  if (action === "remove" && !roster()?.some((m) => m.userId === uid && m.role === "member")) return false;
  const live = guardPage(sid, "members");
  memberRead++; // an older read must not overwrite the post-mutation roster
  memberBusy.set(uid);
  memberNotice.set(null);
  try {
    if (action === "add") await api.addMember(sid, uid);
    else await api.removeMember(sid, uid);
    if (!live()) return false;
    memberNotice.set({ ok: true, message: action === "add" ? `Added ${uid}.` : `Removed ${uid}.` });
    await loadMembers();
    return live();
  } catch (error) {
    if (!live() || isAborted(error)) return false;
    if (error instanceof ApiError && error.kind === "auth") disconnect(error.message);
    else {
      memberNotice.set({ ok: false, message: reason(error, `Couldn't ${action} ${uid}`) });
      // A 404 could also mean a missing user, not lost access to the Space.
      await loadMembers();
    }
    return false;
  } finally {
    if (live()) memberBusy.set(null);
  }
}

// ---------------------------------------------------------------- runner

const dedicatedRunner = { kind: "dedicated", name: "Dedicated Space runner" } as const;
const canAttach = (r: RegisteredRunner) => r.visibility === "organization" || r.canManage;

async function readRunner(api: Client, sid: string, exact: boolean): Promise<RunnerSnapshot> {
  try {
    return { ...await api.getSpaceRunnerAssignment(sid), legacy: false };
  } catch (error) {
    if (exact || !(error instanceof ApiError) || error.kind !== "not_found") throw error;
    // Older servers lack GET …/runner. Prove access before showing any
    // fallback from the Space record; a revoked membership also returns 404.
    const space = (await api.listSpaces()).find((s) => s.id === sid);
    if (!space) throw error;
    if (spaceRunnerExactPending(sid)) throw new ApiError("protocol", "The current runner assignment could not be confirmed.");
    return { runner: space.runnerPaired ? dedicatedRunner : null, legacy: true, guardedTokenChanges: false };
  }
}

async function refreshAssignment(
  api: Client, sid: string, current: () => boolean, show: () => boolean,
  options?: { exact: true; allowFolderAdd?: (runner: SpaceRunner | null) => boolean },
) {
  const observation = beginSpaceRunnerObservation(sid);
  const valid = () => observation.isCurrent() && current();
  try {
    const next = await readRunner(api, sid, !!options?.exact);
    if (!valid()) return undefined;
    batch(() => {
      observation.publish(next.runner, options?.allowFolderAdd?.(next.runner) === true);
      if (show()) {
        runnerSource.set({ sid, legacy: next.legacy, guardedTokenChanges: next.guardedTokenChanges });
        runnerError.set(null);
      }
    });
    return next;
  } catch (error) {
    if (valid()) throw error;
    return undefined;
  }
}

/** A bridge write belongs to this Space even if its Advanced card closed. */
export async function reconcileLocalToolsRunner(sid: string, latest: () => boolean) {
  const api = apiClient();
  const page = guardPage(sid, "runner");
  const current = () => latest() && spaces().some((s) => s.id === sid && s.role === "owner");
  if (!api || !current()) return;
  try {
    await refreshAssignment(api, sid, current, page, { exact: true });
  } catch {
    if (current() && page()) {
      runnerSource.set(null);
      runnerError.set("Couldn't check which runner serves this Space.");
    }
  }
}

export async function loadRunner(stillLatest: () => boolean = () => true): Promise<void> {
  const api = apiClient();
  const sid = spaceSettingsFor();
  if (!api || !sid || spaceSettingsSection() !== "runner" || !stillLatest()) return;
  const page = guardPage(sid, "runner");
  const live = () => page() && stillLatest();
  try {
    const next = await refreshAssignment(api, sid, live, page);
    if (!next && live() && !runnerSnapshot() && !runnerBusy() && !localToolsBusy()) {
      // Another read or write superseded the initial check; ask again.
      await refreshAssignment(api, sid, live, page);
    }
  } catch (error) {
    if (!live() || lost(error, sid)) return;
    runnerSource.set(null);
    runnerError.set("Couldn't check which runner serves this Space.");
  }
}

export async function loadRunnerInventory(): Promise<void> {
  const api = apiClient();
  const sid = spaceSettingsFor();
  if (!api || !sid || !isSettingsOwner() || spaceSettingsSection() !== "runner") return;
  const page = guardPage(sid, "runner");
  const seq = ++inventoryRead;
  const live = () => page() && isSettingsOwner() && seq === inventoryRead;
  try {
    const next = await api.listRunnerInventory();
    if (!live()) return;
    runnerInventory.set({ ...next, runners: next.runners.filter(canAttach) });
    runnerInventoryError.set(null);
  } catch (error) {
    if (!live() || isAborted(error)) return;
    if (error instanceof ApiError && error.kind === "auth") disconnect(error.message);
    else if (error instanceof ApiError && error.kind === "not_found") {
      runnerInventory.set({ attachIfEmpty: false, runners: [] });
      runnerInventoryError.set(null);
    } else {
      runnerInventory.set(null);
      runnerInventoryError.set("Couldn't load runners you can attach.");
    }
  }
}

type RunnerAction =
  | { kind: "attach"; id: string; installed: boolean }
  | { kind: "detach"; id: string; revision: string }
  | { kind: "revoke"; revision: string }
  | { kind: "issue" }
  | { kind: "rotate"; revision: string };

// A later local read/write can invalidate a reveal even after it first opened.
export function dedicatedRevealIsCurrent() {
  const reveal = revealed();
  const r = runnerSnapshot()?.runner;
  return reveal?.kind === "dedicated" && reveal.spaceId === spaceSettingsFor() &&
    spaceSettingsSection() === "runner" && runnerAdvanced() && isSettingsOwner() &&
    r?.kind === "dedicated" && r.assignmentRevision === reveal.revision;
}

effect(() => {
  if (revealed()?.kind === "dedicated" && !dedicatedRevealIsCurrent()) untracked(dismissReveal);
});

const tokenUnconfirmed = "Metro couldn’t confirm the token change. Check the current runner before deciding what to do next. An issued token can’t be recovered here.";

export async function changeRunner(action: RunnerAction): Promise<void> {
  const api = apiClient();
  const sid = spaceSettingsFor();
  const before = runnerSnapshot();
  if (!api || !sid || !before || !isSettingsOwner() || runnerBusy() || localToolsBusy() || spaceSettingsSection() !== "runner") return;
  const page = guardPage(sid, "runner");
  const owns = () => spaces().some((s) => s.id === sid && s.role === "owner");
  const live = () => page() && isSettingsOwner();
  const dedicatedAction = action.kind === "issue" || action.kind === "rotate" || action.kind === "revoke";
  const advanced = advancedGeneration;
  const stillConsented = () => live() && (!dedicatedAction || (runnerAdvanced() && advancedGeneration === advanced));
  if (!stillConsented()) return;
  const guardedRemoval = action.kind === "detach" || action.kind === "revoke";
  const tokenChange = action.kind === "issue" || action.kind === "rotate";
  const expected = (runner: SpaceRunner | null) =>
    action.kind === "attach" || action.kind === "issue" ? runner === null :
      action.kind === "detach" ? runner?.kind === "registered" && runner.id === action.id : runner?.kind === "dedicated";
  if (!expected(before.runner)) return;
  if ((guardedRemoval || action.kind === "rotate") && before.runner?.assignmentRevision !== action.revision) return;
  if (tokenChange && !before.guardedTokenChanges) return;
  if (action.kind === "attach" && (before.legacy || !runnerInventory()?.attachIfEmpty || !runnerInventory()?.runners.some((r) => r.id === action.id))) return;
  const installed = action.kind === "attach" && action.installed ? snapshotInstalledRunner(action.id) : null;
  if (action.kind === "attach" && action.installed && !installed) return;
  const reject = (message: string) => { if (live()) runnerNotice.set({ ok: false, message }); };
  runnerBusy.set(action.kind === "attach" ? `attach:${action.id}` : action.kind);
  runnerNotice.set(null);
  let pendingWrite: (() => boolean) | undefined;
  const writeCurrent = () => !!pendingWrite?.() && owns();
  try {
    let receipt: { token: string; revision: string } | null = null;
    let attachedRevision: string | null = null;
    if (action.kind === "attach") {
      const inventoryRequest = ++inventoryRead;
      const inventory = await api.listRunnerInventory();
      if (!live()) return;
      if (inventoryRead === inventoryRequest) {
        runnerInventory.set({ ...inventory, runners: inventory.runners.filter(canAttach) });
        runnerInventoryError.set(null);
      }
      if (!inventory.attachIfEmpty) return reject("This server doesn't support attaching only to Spaces without a runner.");
      if (!inventory.runners.some((r) => r.id === action.id && canAttach(r)) || (installed && !installed.isCurrent())) {
        return reject("That runner is no longer available to attach.");
      }
      pendingWrite = beginSpaceRunnerWrite(sid);
      attachedRevision = (await api.attachRunnerIfEmpty(sid, action.id)).assignmentRevision;
    } else if (guardedRemoval) {
      pendingWrite = beginSpaceRunnerWrite(sid);
      await api.detachRunnerIfCurrent(sid, action.revision);
    } else {
      pendingWrite = beginSpaceRunnerWrite(sid);
      receipt = await api.changeDedicatedToken(sid, action);
    }
    if (!writeCurrent()) return;
    // U10's ack fence expires pre-commit observations and mid-write consent
    // for this Space only. Keep newer assignment evidence if it already won.
    const afterAck = acknowledgeSpaceRunnerWrite(sid, writeCurrent, before.runner);
    if (!afterAck) return;
    pendingWrite = afterAck;
    // A delayed write reply describes history, not the current assignment.
    // Use the shared exact-Space guards even when the originating page closed.
    const attachConfirmed = (r: SpaceRunner | null) => action.kind === "attach" &&
      r?.kind === "registered" && r.id === action.id && r.assignmentRevision === attachedRevision;
    const actual = await refreshAssignment(api, sid, writeCurrent, live, {
      exact: true, allowFolderAdd: (r) => attachConfirmed(r) && !!installed?.isCurrent(),
    });
    if (!actual) {
      if (receipt && writeCurrent() && stillConsented()) reject(tokenUnconfirmed);
      return;
    }
    const r = actual.runner;
    if (!writeCurrent()) return;
    // Don't associate a credential or an installed-runner side effect with a
    // newer assignment that was already published by another request.
    if (spaceRunners()[sid] !== r) {
      if (receipt && stillConsented()) reject(tokenUnconfirmed);
      return;
    }
    const confirmed = action.kind === "attach" ? attachConfirmed(r) :
      guardedRemoval ? r === null : !!receipt && r?.kind === "dedicated" && r.assignmentRevision === receipt.revision;
    if (!confirmed) return reject(tokenChange
      ? "The runner assignment changed after the token request. No token was shown. Review the current runner above."
      : "The runner assignment changed. Check the current runner above.");
    // Bookkeeping can outlive the page, but needs owner-only proof from
    // this very assignment read. A member view or an older server's
    // unversioned answer can update status, not personal bookkeeping.
    if (action.kind === "attach" && r?.assignmentRevision && installed?.isCurrent()) {
      const space = spaces().find((s) => s.id === sid && s.role === "owner");
      if (space) {
        dismissRunnerOffer();
      }
    }
    if (!live() || !writeCurrent()) return;
    const message = action.kind === "attach" ? `Attached “${r?.name}”. This Space now sends its runner jobs there.` :
      action.kind === "detach" ? "Runner detached from this Space. Its other Spaces are unchanged." :
        action.kind === "revoke" ? "Dedicated token revoked. The runner can no longer serve this Space." :
          "Dedicated token issued. Rotate it if you need a new copy.";
    runnerNotice.set({ ok: true, message });
    if (receipt && stillConsented() && writeCurrent() && spaceRunners()[sid] === r) {
      revealDedicatedToken(sid, settingsSpace()?.name ?? sid, receipt.token, receipt.revision);
    }
  } catch (error) {
    if (!live() || (pendingWrite && !pendingWrite()) || isAborted(error)) return;
    if (error instanceof ApiError && error.kind === "auth") {
      disconnect();
      return;
    }
    // Don't echo server bodies from runner mutations, including token endpoints.
    if ((guardedRemoval || tokenChange) && error instanceof ApiError && error.status === 409) {
      // Hide the old revision immediately; a new confirmation must show a
      // fresh assignment, not the one the server already rejected.
      runnerSource.set(null);
      reject(tokenChange ? "The runner assignment changed. No token was issued or rotated by this request. Review the current runner."
        : "The runner assignment changed. Nothing was removed. Review the current runner before confirming again.");
    } else if ((guardedRemoval || tokenChange) && error instanceof ApiError && (error.status === 404 || error.status === 405)) {
      reject(tokenChange ? "Guarded token changes are unavailable. No unguarded token change was attempted."
        : "The guarded removal route is unavailable. No unguarded removal was attempted.");
    } else if (tokenChange && !(error instanceof ApiError && error.kind === "forbidden")) {
      runnerSource.set(null);
      reject(tokenUnconfirmed);
    } else {
      reject(error instanceof ApiError && error.status === 409
        ? "This Space already has a runner or its assignment changed. No replacement was made."
        : error instanceof ApiError && error.kind === "forbidden"
          ? "Only the owner can change this."
          : "The server did not confirm the runner change. Check its status before trying again.");
    }
    await loadRunner(pendingWrite ? writeCurrent : undefined);
  } finally {
    if (page()) runnerBusy.set(null);
  }
}

export function preferredSpaceRunner() {
  const installed = installedRunnerChoice();
  return installed && runnerInventory()?.runners.some((r) => r.id === installed.id) ? installed.id : null;
}

export function setRunnerAdvanced(open: boolean) {
  if (runnerAdvanced() === open) return;
  advancedGeneration++;
  runnerAdvanced.set(open);
  if (!open) {
    clearLocalToolsMessages();
    if (revealed()?.kind === "dedicated") dismissReveal();
  }
}

// ---------------------------------------------------------------- general

export async function loadDetails(): Promise<void> {
  const api = apiClient();
  const sid = spaceSettingsFor();
  if (!api || !sid) return;
  const live = guardFor(sid);
  detailsError.set(null);
  try {
    const d = await api.spaceDetails(sid);
    if (!live()) return;
    batch(() => {
      details.set(d);
      nameDraft.set(d.name);
      descriptionDraft.set(d.description);
    });
  } catch (error) {
    if (!live() || lost(error, sid)) return;
    detailsError.set(reason(error, "Couldn't load this Space"));
  }
}

export function generalDirty(): boolean {
  const d = details();
  return !!d && (nameDraft().trim() !== d.name || descriptionDraft().trim() !== d.description.trim());
}

export function generalProblem(): string | null {
  if (!nameDraft().trim()) return "Enter a name for the Space.";
  if ([...descriptionDraft().trim()].length > MAX_SPACE_DESCRIPTION) return `Use at most ${MAX_SPACE_DESCRIPTION} characters for the description.`;
  return null;
}

export async function saveGeneral(): Promise<void> {
  const api = apiClient();
  const sid = spaceSettingsFor();
  const d = details();
  if (!api || !sid || !d || !isSettingsOwner() || savingGeneral() || generalProblem() || !generalDirty()) return;
  const live = guardFor(sid);
  const patch: { name?: string; description?: string } = {};
  if (nameDraft().trim() !== d.name) patch.name = nameDraft().trim();
  if (descriptionDraft().trim() !== d.description.trim()) patch.description = descriptionDraft().trim();
  batch(() => {
    savingGeneral.set(true);
    spaceNotice.set(null);
  });
  try {
    const saved = await api.updateSpace(sid, patch);
    if (!live()) return;
    batch(() => {
      details.set(saved);
      nameDraft.set(saved.name);
      descriptionDraft.set(saved.description);
      spaceNotice.set({ ok: true, message: "Saved. Every member sees the new name and description." });
    });
    spaceRenamed(sid, saved.name);
  } catch (error) {
    if (!live() || lost(error, sid)) return;
    spaceNotice.set({ ok: false, message: reason(error, "Couldn't save the Space") });
  } finally {
    if (live()) savingGeneral.set(false);
  }
}

// ---------------------------------------------------------------- delete and leave

export function askDanger(kind: "delete" | "leave" | null) {
  batch(() => {
    dangerOpen.set(kind);
    deleteConfirmText.set("");
    dangerError.set(null);
  });
}

/** The typed name matches the Space's name exactly (as the server checks it). */
export const deleteNameMatches = () => {
  const name = details()?.name ?? settingsSpace()?.name;
  return !!name && deleteConfirmText() === name;
};

export async function deleteSpaceNow(): Promise<void> {
  const api = apiClient();
  const sid = spaceSettingsFor();
  const name = details()?.name ?? settingsSpace()?.name;
  if (!api || !sid || !name || !isSettingsOwner() || dangerBusy() || !deleteNameMatches()) return;
  const live = guardFor(sid);
  batch(() => {
    dangerBusy.set(true);
    dangerError.set(null);
  });
  try {
    await api.deleteSpace(sid, deleteConfirmText());
    if (!live()) return;
    forgetSpaceAgents(sid);
    closeSpaceSettings();
    await spaceGone(sid, `${name} was deleted.`);
  } catch (error) {
    if (!live() || lost(error, sid)) return;
    dangerError.set(
      error instanceof ApiError && error.status === 400 ? "The name you typed doesn't match the Space's name exactly." : reason(error, "Couldn't delete the Space"),
    );
  } finally {
    if (live()) dangerBusy.set(false);
  }
}

export async function leaveSpaceNow(): Promise<void> {
  const api = apiClient();
  const sid = spaceSettingsFor();
  const name = settingsSpace()?.name ?? "the Space";
  if (!api || !sid || isSettingsOwner() || dangerBusy()) return;
  const live = guardFor(sid);
  batch(() => {
    dangerBusy.set(true);
    dangerError.set(null);
  });
  try {
    const { stoppedRuns } = await api.leaveSpace(sid);
    if (!live()) return;
    forgetSpaceAgents(sid);
    closeSpaceSettings();
    const stopped = stoppedRuns === 1 ? " 1 run you started there was stopped." : stoppedRuns > 1 ? ` ${stoppedRuns} runs you started there were stopped.` : "";
    await spaceGone(sid, `You left ${name}.${stopped}`);
  } catch (error) {
    if (!live() || lost(error, sid)) return;
    dangerError.set(
      error instanceof ApiError && error.status === 409 ? "The owner can't leave their Space; delete it instead." : reason(error, "Couldn't leave the Space"),
    );
  } finally {
    if (live()) dangerBusy.set(false);
  }
}

// ---------------------------------------------------------------- agents

function draftOf(a: SpaceAgent): AgentDraft {
  return { name: a.name, handle: a.handle, handleTouched: true, description: a.description, instructions: a.instructions, tools: [...a.tools], color: a.color };
}

/** Opens the editor for a new agent with server defaults and the next unused colour. */
export function startNewAgent() {
  const sid = spaceSettingsFor();
  if (!sid || !isSettingsOwner()) return;
  const used = new Set(spaceAgents(sid).map((a) => a.color));
  const color = AGENT_COLORS.find((c) => !used.has(c)) ?? AGENT_COLORS[0];
  batch(() => {
    editing.set({ mode: "new" });
    agentDraft.set({ name: "", handle: "", handleTouched: false, description: "", instructions: "", tools: (agentList(sid)?.defaultTools ?? []).filter((tool) => !isOptInTool(tool)), color });
    pendingPicture.set(null);
    agentError.set(null);
    spaceNotice.set(null);
    confirmDeleteAgent.set(null);
  });
}

export function startEditAgent(agentId: string) {
  const sid = spaceSettingsFor();
  const a = spaceAgents(sid).find((x) => x.id === agentId);
  if (!a || !isSettingsOwner()) return;
  batch(() => {
    editing.set({ mode: "edit", id: a.id });
    agentDraft.set(draftOf(a));
    pendingPicture.set(null);
    agentError.set(null);
    spaceNotice.set(null);
    confirmDeleteAgent.set(null);
  });
}

export function cancelAgentEdit() {
  batch(() => {
    editing.set(null);
    agentDraft.set(null);
    pendingPicture.set(null);
    agentError.set(null);
  });
}

export function updateDraft(patch: Partial<AgentDraft>) {
  const d = agentDraft();
  if (d) agentDraft.set({ ...d, ...patch });
}

/** The agent being edited, as the server has it (null for a new one). */
export function editedAgent(): SpaceAgent | null {
  const e = editing();
  return e?.mode === "edit" ? spaceAgents(spaceSettingsFor()).find((a) => a.id === e.id) ?? null : null;
}

/** Why the handle can't be used, or null (live, against the Space's other agents). */
export function draftHandleProblem(): string | null {
  const d = agentDraft();
  if (!d) return null;
  const current = editedAgent();
  const taken = spaceAgents(spaceSettingsFor())
    .filter((a) => a.id !== current?.id)
    .map((a) => a.handle);
  return handleProblem(d.handle, taken, current?.handle);
}

/** Why the draft can't be saved, or null. */
export function draftProblem(): string | null {
  const d = agentDraft();
  if (!d) return null;
  const name = d.name.trim();
  if (!name) return "Enter a name for the agent.";
  if ([...name].length > MAX_AGENT_NAME) return `Use at most ${MAX_AGENT_NAME} characters for the name.`;
  const handle = draftHandleProblem();
  if (handle) return handle;
  const desc = d.description.trim();
  if (!desc) return "Describe what the agent is for; the classifier uses it to choose the agent.";
  if (/[\r\n]/.test(desc)) return "Keep the description to one line.";
  if ([...desc].length > MAX_AGENT_DESCRIPTION) return `Use at most ${MAX_AGENT_DESCRIPTION} characters for the description.`;
  if ([...d.instructions.trim()].length > MAX_AGENT_INSTRUCTIONS) return `Use at most ${MAX_AGENT_INSTRUCTIONS} characters for the instructions.`;
  return null;
}

export function draftDirty(): boolean {
  const d = agentDraft();
  if (!d) return false;
  const a = editedAgent();
  if (!a) return true;
  return (
    d.name.trim() !== a.name ||
    d.handle !== a.handle ||
    d.description.trim() !== a.description ||
    d.instructions.trim() !== a.instructions.trim() ||
    d.color !== a.color ||
    d.tools.slice().sort().join(",") !== a.tools.slice().sort().join(",")
  );
}

export const canAddAgent = () => spaceAgents(spaceSettingsFor()).length < MAX_SPACE_AGENTS;

export async function saveAgent(): Promise<void> {
  const api = apiClient();
  const sid = spaceSettingsFor();
  const d = agentDraft();
  const e = editing();
  if (!api || !sid || !d || !e || !isSettingsOwner() || savingAgent() || draftProblem()) return;
  const live = guardFor(sid);
  batch(() => {
    savingAgent.set(true);
    agentError.set(null);
    spaceNotice.set(null);
  });
  try {
    let saved: SpaceAgent;
    if (e.mode === "new") {
      saved = await api.createAgent(sid, {
        name: d.name.trim(),
        handle: d.handle,
        description: d.description.trim(),
        instructions: d.instructions.trim(),
        tools: d.tools,
        color: d.color,
      });
      if (!live()) return;
      adoptAgentOrReload(saved);
      const picture = pendingPicture();
      if (picture) {
        const withPicture = await api.setAgentAvatar(sid, saved.id, picture.bytes, picture.mime);
        if (!live()) return;
        if (withPicture) {
          saved = withPicture;
          adoptAgentOrReload(saved);
          if (saved.avatarVersion) showPicture(sid, saved.id, saved.avatarVersion, picture.bytes, picture.mime);
        }
      }
    } else {
      const a = editedAgent();
      if (!a) return;
      const patch: AgentPatch = {};
      if (d.name.trim() !== a.name) patch.name = d.name.trim();
      if (d.handle !== a.handle) patch.handle = d.handle;
      if (d.description.trim() !== a.description) patch.description = d.description.trim();
      if (d.instructions.trim() !== a.instructions.trim()) patch.instructions = d.instructions.trim();
      if (d.color !== a.color) patch.color = d.color;
      if (d.tools.slice().sort().join(",") !== a.tools.slice().sort().join(",")) patch.tools = d.tools;
      saved = Object.keys(patch).length ? await api.updateAgent(sid, a.id, patch) : a;
      if (!live()) return;
      adoptAgentOrReload(saved);
    }
    invalidateSpaceAgents(sid);
    batch(() => {
      editing.set(null);
      agentDraft.set(null);
      pendingPicture.set(null);
      spaceNotice.set({ ok: true, message: e.mode === "new" ? `Added ${saved.name} (@${saved.handle}).` : `Saved ${saved.name}.` });
    });
  } catch (error) {
    if (!live() || lost(error, sid)) return;
    agentError.set(reason(error, "Couldn't save the agent"));
    void ensureSpaceAgents(sid, true);
  } finally {
    if (live()) savingAgent.set(false);
  }
}

function adoptAgentOrReload(a: SpaceAgent) {
  if (agentList(a.spaceId)) adoptAgent(a);
  else void ensureSpaceAgents(a.spaceId, true);
}

export async function makeDefaultAgent(agentId: string): Promise<void> {
  const api = apiClient();
  const sid = spaceSettingsFor();
  if (!api || !sid || !isSettingsOwner() || agentBusy()) return;
  const live = guardFor(sid);
  batch(() => {
    agentBusy.set(`default:${agentId}`);
    spaceNotice.set(null);
  });
  try {
    const saved = await api.updateAgent(sid, agentId, { isDefault: true });
    if (!live()) return;
    adoptAgentOrReload(saved);
    invalidateSpaceAgents(sid);
    spaceNotice.set({ ok: true, message: `${saved.name} is now the default when no other agent is picked.` });
  } catch (error) {
    if (!live() || lost(error, sid)) return;
    spaceNotice.set({ ok: false, message: reason(error, "Couldn't change the default agent") });
  } finally {
    if (live()) agentBusy.set(null);
  }
}

export async function deleteAgentNow(agentId: string): Promise<void> {
  const api = apiClient();
  const sid = spaceSettingsFor();
  const a = spaceAgents(sid).find((x) => x.id === agentId);
  if (!api || !sid || !a || !isSettingsOwner() || agentBusy()) return;
  const live = guardFor(sid);
  batch(() => {
    agentBusy.set(`delete:${agentId}`);
    spaceNotice.set(null);
  });
  try {
    const { defaultAgentId } = await api.deleteAgent(sid, agentId);
    if (!live()) return;
    dropAgent(sid, agentId, defaultAgentId);
    invalidateSpaceAgents(sid);
    if (editing()?.mode === "edit" && (editing() as { id: string }).id === agentId) cancelAgentEdit();
    // The server drops everyone's model choice for it; drop ours from what's shown too.
    const p = prefs();
    if (p && p.agentModels[agentId]) {
      const { [agentId]: _gone, ...rest } = p.agentModels;
      prefs.set({ ...p, agentModels: rest });
    }
    spaceNotice.set({ ok: true, message: `Deleted ${a.name}.` });
  } catch (error) {
    if (!live() || lost(error, sid)) return;
    spaceNotice.set({
      ok: false,
      message: error instanceof ApiError && error.status === 409 ? "A Space needs at least one agent. Add another before deleting this one." : reason(error, `Couldn't delete ${a.name}`),
    });
  } finally {
    if (live()) {
      agentBusy.set(null);
      confirmDeleteAgent.set(null);
    }
  }
}

/** Picks a PNG or JPEG with the system's file picker; checked by its bytes. Null when cancelled or refused (the reason is in agentError). */
async function pickPicture(live: () => boolean): Promise<{ bytes: Uint8Array; mime: AvatarMime } | null> {
  let picked;
  try {
    picked = await SystemPicker.pickFiles({ multiple: false, filters: [{ name: "Pictures (PNG, JPEG)", extensions: [...AVATAR_EXTENSIONS] }], maxSize: MAX_AVATAR_BYTES });
  } catch (error) {
    if (live()) agentError.set(`The file picker couldn't open: ${String((error as Error)?.message ?? error)}`);
    return null;
  }
  if (!live() || picked.length === 0) return null;
  const file = picked[0]!;
  if (file.status === "tooLarge" || !file.file || file.size > MAX_AVATAR_BYTES) {
    agentError.set(`That picture is too large. Use a PNG or JPEG up to ${MAX_AVATAR_BYTES / (1024 * 1024)} MB.`);
    return null;
  }
  const bytes = new Uint8Array(await file.file.arrayBuffer());
  if (!live()) return null;
  const mime = sniffAvatar(bytes);
  if (!mime) {
    agentError.set("That file isn't a PNG or JPEG picture.");
    return null;
  }
  return { bytes, mime };
}

/** Chooses the edited agent's picture: sent at once for an existing agent, after creating for a new one. */
export async function chooseAgentPicture(): Promise<void> {
  const api = apiClient();
  const sid = spaceSettingsFor();
  const e = editing();
  if (!api || !sid || !e || !isSettingsOwner() || agentBusy()) return;
  const live = guardFor(sid);
  const busyKey = `picture:${e.mode === "edit" ? e.id : "new"}`;
  batch(() => {
    agentBusy.set(busyKey);
    agentError.set(null);
  });
  try {
    const picture = await pickPicture(live);
    if (!picture || !live() || !isSettingsOwner()) return;
    if (e.mode === "new") {
      pendingPicture.set(picture);
      return;
    }
    const saved = await api.setAgentAvatar(sid, e.id, picture.bytes, picture.mime);
    if (!live()) return;
    if (saved) {
      adoptAgentOrReload(saved);
      if (saved.avatarVersion) showPicture(sid, saved.id, saved.avatarVersion, picture.bytes, picture.mime);
    } else {
      await ensureSpaceAgents(sid, true);
    }
    invalidateSpaceAgents(sid);
    spaceNotice.set({ ok: true, message: "Picture updated." });
  } catch (error) {
    if (!live() || lost(error, sid)) return;
    agentError.set(reason(error, "Couldn't update the picture"));
  } finally {
    if (live()) agentBusy.set(null);
  }
}

export async function removeAgentPicture(): Promise<void> {
  const api = apiClient();
  const sid = spaceSettingsFor();
  const e = editing();
  if (!api || !sid || !e || !isSettingsOwner() || agentBusy()) return;
  if (e.mode === "new") {
    pendingPicture.set(null);
    return;
  }
  const a = editedAgent();
  if (!a?.avatarVersion) return;
  const live = guardFor(sid);
  batch(() => {
    agentBusy.set(`picture:${e.id}`);
    agentError.set(null);
  });
  try {
    await api.removeAgentAvatar(sid, e.id);
    if (!live()) return;
    adoptAgentOrReload({ ...a, avatarVersion: null });
    invalidateSpaceAgents(sid);
    spaceNotice.set({ ok: true, message: "Picture removed. The initial shows instead." });
  } catch (error) {
    if (!live() || lost(error, sid)) return;
    agentError.set(reason(error, "Couldn't remove the picture"));
  } finally {
    if (live()) agentBusy.set(null);
  }
}

// ---------------------------------------------------------------- your preferences

export async function loadPreferences(): Promise<void> {
  const api = apiClient();
  const sid = spaceSettingsFor();
  if (!api || !sid) return;
  const live = guardFor(sid);
  batch(() => {
    prefsError.set(null);
    catalogError.set(null);
  });
  void ensureSpaceAgents(sid);
  const [p, c] = await Promise.allSettled([api.spacePrefs(sid), api.availableCatalog(sid)]);
  if (!live()) return;
  if (p.status === "fulfilled") {
    prefs.set(p.value);
    noteClassifierPrefs(sid, p.value.classifierAvailable && p.value.classifyUnaddressed);
  }
  else if (!lost(p.reason, sid)) prefsError.set(reason(p.reason, "Couldn't load your preferences for this Space"));
  if (!live()) return;
  if (c.status === "fulfilled") catalog.set(c.value);
  else if (!lost(c.reason, sid)) catalogError.set(reason(c.reason, "Couldn't load the models you can use"));
}

async function putPrefs(next: SpacePrefs, busy: string, what: string): Promise<boolean> {
  const api = apiClient();
  const sid = spaceSettingsFor();
  if (!api || !sid || prefsBusy()) return false;
  const live = guardFor(sid);
  batch(() => {
    prefsBusy.set(busy);
    spaceNotice.set(null);
  });
  try {
    const saved = await api.putSpacePrefs(sid, next);
    if (!live()) return false;
    prefs.set(saved);
    noteClassifierPrefs(sid, saved.classifierAvailable && saved.classifyUnaddressed);
    return true;
  } catch (error) {
    if (!live() || lost(error, sid)) return false;
    if (busy === "classify" && error instanceof ApiError && error.status === 409) {
      spaceNotice.set({ ok: false, message: "You don't have a System One connection you can use in this Space. Add one in Settings › Model connections first." });
      const p = prefs();
      if (p) prefs.set({ ...p, classifierAvailable: false });
    } else {
      spaceNotice.set({ ok: false, message: reason(error, what) });
    }
    return false;
  } finally {
    if (live()) prefsBusy.set(null);
  }
}

const base = (p: SpacePrefsView): SpacePrefs => ({ classifyUnaddressed: p.classifyUnaddressed, defaultModel: p.defaultModel, agentModels: p.agentModels, routing: p.routing });

export function setClassifyUnaddressed(on: boolean) {
  const p = prefs();
  if (p) void putPrefs({ ...base(p), classifyUnaddressed: on }, "classify", "Couldn't change who picks up your messages");
}

export function setSpaceRouting(on: boolean) {
  const p = prefs();
  if (p) void putPrefs({ ...base(p), routing: on }, "routing", "Couldn't change model routing for this Space");
}

export function setSpaceDefaultModel(sel: ModelSelection | null) {
  const p = prefs();
  if (p) void putPrefs({ ...base(p), defaultModel: sel }, "default", "Couldn't save your model for this Space");
}

export function setAgentModel(agentId: string, sel: ModelSelection | null) {
  const p = prefs();
  if (!p) return;
  const agentModels = { ...p.agentModels };
  if (sel) agentModels[agentId] = sel;
  else delete agentModels[agentId];
  void putPrefs({ ...base(p), agentModels }, agentId, "Couldn't save your model for this agent");
}

onSessionEnd(() => {
  closeSpaceSettings();
});
