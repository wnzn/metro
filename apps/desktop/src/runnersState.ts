// Shared by Settings › Runners and the entry points that offer this computer's runner.
import { cleanup, computed, effect, signal, Storage, untracked } from "nsh";
import { ApiError } from "./api/client";
import type { RegisteredRunner, Space } from "./api/client";
import { beginSpaceRunnerWrite, loadRunners, reconcileOwnedSpaceRunner, runnerBusy, runners, spaceRunners } from "./settings";
import { apiClient, connectedTo, me, onSessionEnd, sessionGuard, setRunnerPaired, spaces } from "./store";
import { helper } from "./device/state";
import { installedBusy, installedRunner, installedRunnerStatusWatchable, loadInstalledRunner, ownedInstalledRunner, readInstalledRunner, recordAttachedFolder } from "./features/installedRunner/installedRunner";
import type { InstalledRunnerState } from "./features/installedRunner/installedRunner";

export const runnerSessionKey = () => JSON.stringify([connectedTo(), me()]);
export const runnerOfferStorageKey = (server: string, userId: string) =>
  `runnerOffer:${encodeURIComponent(server)}:${encodeURIComponent(userId)}`;

const offer = signal<{ key: string; dismissed: boolean } | null>(null);
const dismissedThisLaunch = new Set<string>();
const offerFresh = signal(false);
const watchers = signal(0);
const watching = computed(() => watchers() > 0);
const OFFER_REFRESH_MS = 5000;
const OFFER_REFRESH_GRACE_MS = 2000;
const attachFeedback = signal<{ message: string; incomplete: boolean } | null>(null);
export const runnerAttachFeedback = () => attachFeedback();
export const runnerAttachBusy = signal(false);

const bridgeSessionMatches = computed(() => {
  const h = helper();
  return h.phase === "ready" && !!h.status.session && h.status.session.accountId === me() &&
    h.status.session.serverUrl.replace(/\/+$/, "") === connectedTo();
});

export const installedRunnerChoice = computed(() => {
  const runner = installedRunner()?.runner;
  const owned = ownedInstalledRunner();
  return bridgeSessionMatches() && runner && runner.id === owned?.id && owned.attachIfEmpty ? runner : null;
});

const installedChoiceId = computed(() => installedRunnerChoice()?.id ?? null);
const reconnectingInstalled = computed(() => installedBusy() === "reconnect");
let installedChoiceEpoch = 0;
effect(() => {
  installedChoiceId();
  // Starting a reconnect can rebind even a runner with the same id.
  reconnectingInstalled();
  installedChoiceEpoch++;
});

/** Capture the exact choice at submission; a lost or different choice cannot regain consent. */
export function snapshotInstalledRunner(displayedId: string) {
  if (installedChoiceId() !== displayedId || reconnectingInstalled()) return null;
  const epoch = installedChoiceEpoch;
  const key = runnerSessionKey();
  const sessionOK = sessionGuard();
  return {
    id: displayedId,
    isCurrent: () => sessionOK() && runnerSessionKey() === key &&
      installedChoiceEpoch === epoch && installedChoiceId() === displayedId && !reconnectingInstalled(),
  };
}

const isEmptyOwned = (s: Space) => s.role === "owner" && s.runnerPaired === false;

export const eligibleSpaces = computed(() => installedRunnerChoice() ? spaces().filter(isEmptyOwned) : []);
const offerKey = () => {
  const server = connectedTo();
  const user = me();
  return server && user ? runnerOfferStorageKey(server, user) : null;
};

export const runnerOfferCount = computed(() => {
  const state = offer();
  return offerFresh() && state && state.key === offerKey() && !state.dismissed && installedRunner()?.state === "online"
    ? eligibleSpaces().length : 0;
});

async function loadOfferDismissal() {
  const key = offerKey();
  if (!key || offer()?.key === key) return;
  const valid = sessionGuard();
  // If reading app data fails, don't present a potentially dismissed offer.
  const value = await Storage.getItem(key).catch(() => "dismissed");
  if (valid() && key === offerKey() && offer()?.key !== key) offer.set({ key, dismissed: value === "dismissed" || dismissedThisLaunch.has(key) });
}

/** Not now, or a confirmed successful attachment. No token or display name is stored. */
export function dismissRunnerOffer() {
  const key = offerKey();
  if (!key || offer()?.key === key && offer()?.dismissed) return;
  dismissedThisLaunch.add(key);
  offer.set({ key, dismissed: true });
  void Storage.setItem(key, "dismissed").catch(() => {});
}

/** One shared read-only watch, regardless of how many consumers are mounted. */
export function watchInstalledRunner(refreshFoldersOnOpen = false) {
  watchers.set(untracked(watchers) + 1);
  cleanup(() => watchers.set(untracked(watchers) - 1));
  if (refreshFoldersOnOpen) effect(() => {
    if (!offerKey() || !bridgeSessionMatches()) return;
    const valid = sessionGuard();
    let wanted = true;
    cleanup(() => { wanted = false; });
    // A mounted Settings page needs folders, even when joining a Sidebar
    // status read. Closing the page withdraws just this folder demand.
    untracked(() => void loadInstalledRunner(true, () => wanted && valid()));
  });
}

effect(() => {
  if (watching() && offerKey()) untracked(() => void loadOfferDismissal());
});

effect(() => {
  if (!watching() || !offerKey() || !bridgeSessionMatches()) return;
  // Initial read is independent of the offer, so Create can still use the
  // verified runner after dismissal or when there are no existing Spaces.
  untracked(() => void loadInstalledRunner(false));
});

const shouldWatchOffer = computed(() => {
  if (!watching() || !bridgeSessionMatches() || !installedRunnerStatusWatchable() ||
      runnerAttachBusy() || installedBusy() || ownedInstalledRunner()?.attachIfEmpty === false) return false;
  const key = offerKey();
  const state = offer();
  return !!key && state?.key === key && !state.dismissed && spaces().some(isEmptyOwned);
});

effect(() => {
  if (!shouldWatchOffer()) {
    offerFresh.set(false);
    return;
  }
  const valid = sessionGuard();
  let stopped = false;
  let timer: ReturnType<typeof setTimeout> | undefined;
  let expiry: ReturnType<typeof setTimeout> | undefined;
  const tick = async () => {
    if (stopped || !valid() || !shouldWatchOffer()) return;
    // A hung bridge (with a longer transport timeout) must not leave an
    // online offer on screen. Keep the last verified Create choice separate.
    expiry = setTimeout(() => offerFresh.set(false), OFFER_REFRESH_GRACE_MS);
    // Same runner: only ask the bridge for liveness. Don't reload every
    // Space's folders while Sidebar is sitting open.
    await loadInstalledRunner(false);
    if (expiry !== undefined) clearTimeout(expiry);
    expiry = undefined;
    if (stopped || !valid() || !shouldWatchOffer()) return;
    offerFresh.set(true);
    timer = setTimeout(() => void tick(), OFFER_REFRESH_MS);
  };
  cleanup(() => {
    stopped = true;
    if (timer !== undefined) clearTimeout(timer);
    if (expiry !== undefined) clearTimeout(expiry);
  });
  untracked(() => void tick());
});

function actionError(error: unknown, fallback: string) {
  if (!(error instanceof ApiError)) return fallback;
  if (error.status === 401 || error.status === 403) return "You no longer have permission to do this.";
  if (error.status === 404 || error.status === 405) return "The requested action or item is no longer available on this server.";
  if (error.kind === "timeout" || error.kind === "network") return "The server did not confirm the action. Check again before retrying.";
  return fallback;
}

type CreatedSpaceAttachment =
  | { kind: "attached"; state: InstalledRunnerState }
  | { kind: "not-attached"; reason: string }
  | { kind: "unconfirmed" }
  | { kind: "superseded" };

const notAttached = (reason: string): CreatedSpaceAttachment => ({ kind: "not-attached", reason });

/** Called only after Create Space succeeds, with the id returned by that creation. */
export async function attachCreatedSpaceRunner(
  space: Space,
  choice: NonNullable<ReturnType<typeof snapshotInstalledRunner>>,
  stillConsented: () => boolean,
): Promise<CreatedSpaceAttachment> {
  const c = apiClient();
  const sid = space.id;
  const sessionOK = sessionGuard();
  const spaceOK = sessionGuard(sid);
  const ownsSpace = () => spaces().some((s) => s.id === sid && s.role === "owner");
  const consentChanged = notAttached("The Create Space choice changed.");
  const runnerChanged = notAttached("This computer's runner is no longer available for this account.");
  const notOwner = notAttached("You no longer own this Space.");
  const alreadyPaired = notAttached("This Space already has a runner.");
  const unsupported = notAttached("This server doesn't support safe attachment to empty Spaces.");
  if (!c || !stillConsented()) return consentChanged;
  if (!choice.isCurrent()) return runnerChanged;
  if (!spaceOK() || space.role !== "owner" || !ownsSpace()) return notOwner;
  let pendingWrite: (() => boolean) | undefined;
  let accepted = false;
  try {
    const [local, inventory, liveSpaces] = await Promise.all([readInstalledRunner(), c.listRunnerInventory(), c.listSpaces()]);
    if (!stillConsented()) return consentChanged;
    if (!spaceOK() || !ownsSpace()) return notOwner;
    if (!inventory.attachIfEmpty) return unsupported;
    if (!choice.isCurrent() || local?.runner?.id !== choice.id ||
        !inventory.runners.some((r) => r.id === choice.id && r.visibility === "personal" && r.canManage)) return runnerChanged;
    const live = liveSpaces.find((s) => s.id === sid);
    if (live?.role !== "owner") return notOwner;
    if (live.runnerPaired !== false) return alreadyPaired;
    // The server repeats ownership and emptiness checks atomically. No fallback
    // to the replacing route, including for servers missing this endpoint.
    pendingWrite = beginSpaceRunnerWrite(sid);
    const receipt = await c.attachRunnerIfEmpty(sid, choice.id);
    accepted = true;
    if (!pendingWrite()) return { kind: "superseded" };
    // Reconcile the actual assignment instead of replaying a delayed POST
    // response. Folder membership is read authoritatively on Settings open.
    const current = await reconcileOwnedSpaceRunner(sid, () => !!pendingWrite?.() && choice.isCurrent());
    if (current === undefined) return { kind: "unconfirmed" };
    if (!pendingWrite() || !choice.isCurrent() || current?.kind !== "registered" || current.id !== choice.id ||
        current.assignmentRevision !== receipt.assignmentRevision || spaceRunners()[sid] !== current) return { kind: "superseded" };
    if (sessionOK()) dismissRunnerOffer();
    return { kind: "attached", state: local.state };
  } catch (error) {
    if (!pendingWrite) return notAttached("Metro couldn't verify the runner and the new Space.");
    if (!pendingWrite()) return { kind: "superseded" };
    if (!accepted && error instanceof ApiError && error.kind !== "protocol") {
      if (error.status === 409) return alreadyPaired;
      if (error.status === 404 || error.status === 405) {
        return notAttached("The safe attachment route or runner is no longer available on this server.");
      }
      if (error.status === 401 || error.status === 403) return notAttached("The server refused permission to attach the runner.");
      if (error.status === 400 || error.status === 422) return notAttached("The server rejected the attachment request.");
    }
    return { kind: "unconfirmed" };
  }
}

/** Uses the same non-replacing route for every selected Space, one at a time. */
export async function attachMany(spaceIds: readonly string[]) {
  const c = apiClient();
  const runner = installedRunnerChoice();
  const selected = new Set(spaceIds);
  const picks = eligibleSpaces().filter((s) => selected.has(s.id));
  if (!c || !runner || !picks.length || runnerAttachBusy() || installedBusy()) return;
  const sessionOK = sessionGuard();
  const key = runnerSessionKey();
  const valid = () => sessionOK() && key === runnerSessionKey();
  const matches = () => installedRunnerChoice()?.id === runner.id;
  runnerAttachBusy.set(true);
  attachFeedback.set(null);
  const failures: string[] = [];
  let attached = 0;
  try {
    const [local, inventory, liveSpaces] = await Promise.all([readInstalledRunner(), c.listRunnerInventory(), c.listSpaces()]);
    if (!valid()) return;
    if (!inventory.attachIfEmpty) {
      ownedInstalledRunner.set({ id: runner.id, attachIfEmpty: false });
      attachFeedback.set({ message: "This server doesn't support attaching only to Spaces without a runner.", incomplete: true });
      return;
    }
    if (!matches() || local?.runner?.id !== runner.id ||
        !inventory.runners.some((r) => r.id === runner.id && r.visibility === "personal" && r.canManage)) {
      attachFeedback.set({ message: "This computer's runner is no longer available for this account. No Spaces were changed.", incomplete: true });
      return;
    }
    const owned = new Map(liveSpaces.filter((s) => s.role === "owner").map((s) => [s.id, s]));
    for (const pick of picks) {
      if (!valid()) return;
      if (!matches()) {
        failures.push(`${pick.name}: This computer's runner changed.`);
        continue;
      }
      const s = owned.get(pick.id);
      if (!s) {
        failures.push(`${pick.name}: You no longer own this Space.`);
        continue;
      }
      const spaceOK = sessionGuard(s.id);
      let pendingWrite: (() => boolean) | undefined;
      try {
        const current = await c.getSpaceRunner(s.id);
        if (!valid()) return;
        if (!spaceOK() || !spaces().some((space) => space.id === s.id && space.role === "owner")) {
          failures.push(`${s.name}: You no longer own this Space.`);
          continue;
        }
        if (s.runnerPaired || current !== null) {
          setRunnerPaired(s.id, true);
          failures.push(`${s.name}: Already has a runner.`);
          continue;
        }
        if (!matches()) {
          failures.push(`${s.name}: This computer's runner changed.`);
          continue;
        }
        pendingWrite = beginSpaceRunnerWrite(s.id);
        const receipt = await c.attachRunnerIfEmpty(s.id, runner.id);
        if (!valid()) return;
        const currentAssignment = await reconcileOwnedSpaceRunner(s.id, () => valid() && !!pendingWrite?.() && matches());
        if (!valid()) return;
        if (!pendingWrite() || currentAssignment?.kind !== "registered" ||
            currentAssignment.id !== runner.id || currentAssignment.assignmentRevision !== receipt.assignmentRevision ||
            spaceRunners()[s.id] !== currentAssignment) {
          failures.push(`${s.name}: The current assignment could not be confirmed.`);
          continue;
        }
        attached++;
        dismissRunnerOffer();
        if (matches()) recordAttachedFolder({ id: s.id, name: s.name });
      } catch (error) {
        if (!valid()) return;
        if (error instanceof ApiError && error.status === 409) {
          if (pendingWrite?.() && spaceOK()) setRunnerPaired(s.id, true);
          failures.push(`${s.name}: Already has a runner.`);
        } else {
          failures.push(`${s.name}: ${actionError(error, "The attach was not confirmed.")}`);
        }
      }
    }
    if (!valid()) return;
    const count = failures.length ? `${attached} of ${picks.length}` : `${attached}`;
    const result = `Attached to ${count} ${picks.length === 1 ? "Space" : "Spaces"}.`;
    attachFeedback.set({ message: [result, ...failures].join(" "), incomplete: failures.length > 0 });
  } catch (error) {
    if (valid()) attachFeedback.set({ message: actionError(error, "Could not check this computer's runner and your Spaces."), incomplete: true });
  } finally {
    if (valid()) runnerAttachBusy.set(false);
  }
}

/** The server is authoritative, but unknown or inconsistent capabilities aren't actionable. */
export function runnerUpdateTarget(r: RegisteredRunner): string | null {
  const p = r.presence;
  const u = r.update;
  if (!p?.version || !u?.available || !u.latest) return null;
  const from = p.version.split(".").map(Number);
  const to = u.latest.split(".").map(Number);
  const firstDifference = to.findIndex((part, i) => part !== from[i]);
  return firstDifference !== -1 && to[firstDifference]! > from[firstDifference]! ? u.latest : null;
}

export function selfUpdateTarget(r: RegisteredRunner): string | null {
  const p = r.presence;
  const u = r.update;
  return r.canManage && p?.install === "managed" && p.online && p.selfUpdate &&
    u?.action === "self_update" && !u.pending ? runnerUpdateTarget(r) : null;
}

export function pinnedRunnerImage(r: RegisteredRunner): string | null {
  const latest = runnerUpdateTarget(r);
  if (!latest || r.presence?.install !== "container" || r.update?.action !== "container") return null;
  const image = r.update.containerImage;
  const prefix = `ghcr.io/wnzn/metro-runner:v${latest}@sha256:`;
  return image?.startsWith(prefix) && image.length === prefix.length + 64 && /^[0-9a-f]{64}$/.test(image.slice(prefix.length))
    ? image : null;
}

export const runnerUpdateBusy = signal<string | null>(null);
export const runnerUpdateErrors = signal<Record<string, string>>({});

/** Only called from a scoped confirmation. Re-read before sending; never retarget. */
export async function requestRunnerUpdate(id: string, target: string) {
  const c = apiClient();
  const current = runners()?.find((r) => r.id === id);
  if (!c || !current || runnerBusy() || runnerUpdateBusy() || selfUpdateTarget(current) !== target) return;
  if (installedRunner()?.runner?.id === id) return;
  const valid = sessionGuard();
  runnerUpdateBusy.set(id);
  runnerUpdateErrors.set({ ...runnerUpdateErrors(), [id]: "" });
  try {
    const list = await c.listRunners();
    if (!valid()) return;
    const live = list.find((r) => r.id === id);
    if (!live || selfUpdateTarget(live) !== target || installedRunner()?.runner?.id === id) {
      await loadRunners();
      if (!valid()) return;
      runnerUpdateErrors.set({ ...runnerUpdateErrors(), [id]: "This runner is no longer eligible for that update. Nothing was requested." });
      return;
    }
    const pending = await c.requestRunnerUpdate(id, target);
    if (!valid()) return;
    runners.set(runners()?.map((r) => r.id === id && r.update ? { ...r, update: { ...r.update, pending } } : r) ?? null);
  } catch (error) {
    if (!valid()) return;
    const message = actionError(error, error instanceof ApiError && error.status === 409
      ? "The server refused this update. Availability or runner status may have changed."
      : "The server did not confirm this update request.");
    await loadRunners();
    if (!valid()) return;
    runnerUpdateErrors.set({ ...runnerUpdateErrors(), [id]: message });
  } finally {
    if (valid()) runnerUpdateBusy.set(null);
  }
}

onSessionEnd(() => {
  offer.set(null);
  offerFresh.set(false);
  attachFeedback.set(null);
  runnerAttachBusy.set(false);
  runnerUpdateBusy.set(null);
  runnerUpdateErrors.set({});
});
