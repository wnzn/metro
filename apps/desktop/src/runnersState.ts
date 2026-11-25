// Shared by Settings › Runners and the entry points that offer this computer's runner.
import { cleanup, computed, effect, signal, Storage, untracked } from "nsh";
import { ApiError } from "./api/client";
import type { Client, RegisteredRunner, Space, SpaceRunner } from "./api/client";
import { beginSpaceRunnerWrite, loadRunners, reconcileOwnedSpaceRunner, recordRunnerUpdateAccepted, runnerBusy, runners, snapshotSpaceRunnerWrite, spaceRunners } from "./settings";
import { apiClient, connectedTo, me, onSessionEnd, place, sessionGuard, settingsSection, spaces } from "./store";
import { helper } from "./device/state";
import { installedBusy, installedFolderSnapshot, installedRunner, installedRunnerSeen, installedRunnerStatusWatchable, loadInstalledRunner, ownedInstalledRunner, readInstalledRunner } from "./features/installedRunner/installedRunner";
import type { InstalledRunnerState } from "./features/installedRunner/installedRunner";

export const runnerSessionKey = () => JSON.stringify([connectedTo(), me()]);
export const runnerOfferStorageKey = (server: string, userId: string) =>
  `runnerOffer:${encodeURIComponent(server)}:${encodeURIComponent(userId)}`;

const offer = signal<{ key: string; dismissed: boolean } | null>(null);
const dismissedThisLaunch = new Set<string>();
const offerFresh = signal(false);
const watchers = signal(0);
const watching = computed(() => watchers() > 0);
const folderWatchers = signal(0);
const watchingFolders = computed(() => folderWatchers() > 0);
const RUNNER_REFRESH_MS = 5000;
const FOLDER_REFRESH_BUDGET_MS = 30_000;
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
  // Reconnect can rebind even the same registration. Rendering and submitting
  // use the same eligibility and both must wait for the action to settle.
  if (installedBusy() === "reconnect") return null;
  const runner = installedRunner()?.runner;
  const owned = ownedInstalledRunner();
  return bridgeSessionMatches() && runner && runner.id === owned?.id && owned.attachIfEmpty ? runner : null;
});

const installedChoiceId = computed(() => installedRunnerChoice()?.id ?? null);
let installedChoiceEpoch = 0;
effect(() => {
  installedChoiceId();
  installedChoiceEpoch++;
});

/** Capture the exact choice at submission; a lost or different choice cannot regain consent. */
export function snapshotInstalledRunner(displayedId: string) {
  if (installedChoiceId() !== displayedId) return null;
  const epoch = installedChoiceEpoch;
  const key = runnerSessionKey();
  const sessionOK = sessionGuard();
  return {
    id: displayedId,
    isCurrent: () => sessionOK() && runnerSessionKey() === key &&
      installedChoiceEpoch === epoch && installedChoiceId() === displayedId,
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
  if (!refreshFoldersOnOpen) return;
  folderWatchers.set(untracked(folderWatchers) + 1);
  cleanup(() => folderWatchers.set(untracked(folderWatchers) - 1));
}

type FolderInterest = { baseline: number | null; valid: () => boolean; expired: () => boolean; started: boolean };
const folderInterest = signal<FolderInterest | null>(null);
const folderRetry = signal(0);
const needsFolderRead = (interest: FolderInterest | null) => {
  if (!interest || folderInterest() !== interest || !interest.valid()) return false;
  const snapshot = installedFolderSnapshot();
  return snapshot === null || snapshot === interest.baseline;
};
const wantsFolderRead = (interest: FolderInterest | null) =>
  !!interest && !interest.expired() && needsFolderRead(interest) && bridgeSessionMatches();
const canCheckFolders = () => installedRunnerStatusWatchable() || installedRunnerSeen();
const folderReadReady = computed(() => bridgeSessionMatches() && !runnerAttachBusy() && !installedBusy());

export const installedFolderRefreshExhausted = computed(() => {
  const interest = folderInterest();
  return !!interest?.expired() && needsFolderRead(interest) && canCheckFolders();
});

/** Retry the current Settings scope; never reset a running budget. */
export function retryInstalledFolderRefresh() {
  if (installedFolderRefreshExhausted()) folderRetry.set(folderRetry() + 1);
}

effect(() => {
  if (!watchingFolders() || !offerKey()) return;
  folderRetry();
  const expired = signal(false);
  const interest: FolderInterest = {
    baseline: untracked(installedFolderSnapshot), valid: sessionGuard(), expired, started: false,
  };
  // One budget per mounted Settings scope, not per response, watcher or
  // bridge phase. It also bounds retries when the Sidebar offer is dismissed.
  const timer = setTimeout(() => expired.set(true), FOLDER_REFRESH_BUDGET_MS);
  folderInterest.set(interest);
  cleanup(() => {
    clearTimeout(timer);
    folderInterest.set(null);
  });
});

effect(() => {
  const interest = folderInterest();
  if (!interest || !folderReadReady()) return;
  // Try on open, joining any Sidebar read already in flight. If this attempt
  // can't verify the candidate, the mounted demand survives for a later read.
  untracked(() => {
    if (interest.started || !wantsFolderRead(interest)) return;
    interest.started = true;
    void loadInstalledRunner(true, () => wantsFolderRead(interest));
  });
});

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

const shouldRefreshRunner = computed(() => shouldWatchOffer() ||
  (canCheckFolders() && folderReadReady() && wantsFolderRead(folderInterest())));

effect(() => {
  if (!shouldWatchOffer()) offerFresh.set(false);
});

effect(() => {
  if (!shouldRefreshRunner()) return;
  const valid = sessionGuard();
  let stopped = false;
  let timer: ReturnType<typeof setTimeout> | undefined;
  let expiry: ReturnType<typeof setTimeout> | undefined;
  const tick = async () => {
    if (stopped || !valid() || !shouldRefreshRunner()) return;
    // A hung bridge (with a longer transport timeout) must not leave an
    // online offer on screen. Keep the last verified Create choice separate.
    if (shouldWatchOffer()) expiry = setTimeout(() => offerFresh.set(false), OFFER_REFRESH_GRACE_MS);
    const interest = folderInterest();
    // Share one cadence. Once Settings has a complete snapshot, Sidebar
    // checks liveness only until the registration changes or is invalidated.
    await loadInstalledRunner(wantsFolderRead(interest), () => wantsFolderRead(interest));
    if (expiry !== undefined) clearTimeout(expiry);
    expiry = undefined;
    if (stopped || !valid() || !shouldRefreshRunner()) return;
    if (shouldWatchOffer()) offerFresh.set(true);
    timer = setTimeout(() => void tick(), RUNNER_REFRESH_MS);
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

type InstalledChoice = NonNullable<ReturnType<typeof snapshotInstalledRunner>>;
type AttachProblem =
  | { kind: "not-attached" | "superseded"; reason: string }
  | { kind: "unconfirmed" };
type CreatedSpaceAttachment = AttachProblem | { kind: "attached"; state: InstalledRunnerState };

const notAttached = (reason: string) => ({ kind: "not-attached" as const, reason });
const newerRunnerAction = {
  kind: "superseded" as const, reason: "A newer runner action superseded this attachment check.",
};
const changedSinceSelection = notAttached("A newer runner action changed this Space after it was selected. Select it again to attach.");

const ownsSpace = (sid: string) => spaces().some((s) => s.id === sid && s.role === "owner");

/** Never publish from the write receipt alone: another action may have won since it was sent. */
async function attachAndReconcile(
  c: Client, sid: string, choice: InstalledChoice, conflict: string, expectedWrite?: () => boolean,
): Promise<AttachProblem | { kind: "attached"; stillVerified: () => boolean }> {
  // The server repeats ownership and emptiness checks atomically. Never fall
  // back to the replacing route, including if the server lacks this endpoint.
  const latest = beginSpaceRunnerWrite(sid, expectedWrite);
  if (!latest) return changedSinceSelection;
  const stillScoped = () => latest() && ownsSpace(sid) && choice.isCurrent();
  const matches = (r: SpaceRunner | null | undefined) =>
    r?.kind === "registered" && r.id === choice.id && r.visibility === "personal";
  let accepted = false;
  try {
    await c.attachRunnerIfEmpty(sid, choice.id);
    accepted = true;
    if (!latest()) return newerRunnerAction;
    const current = await reconcileOwnedSpaceRunner(sid, stillScoped);
    if (!latest()) return newerRunnerAction;
    if (current === undefined) return { kind: "unconfirmed" };
    if (!matches(current)) return { kind: "superseded", reason: "The runner assignment changed." };
    return { kind: "attached", stillVerified: () => stillScoped() && matches(spaceRunners()[sid]) };
  } catch (error) {
    if (!latest()) return newerRunnerAction;
    if (!accepted && error instanceof ApiError && error.kind !== "protocol") {
      if (error.status === 409) return notAttached(conflict);
      if (error.status === 404 || error.status === 405) {
        return notAttached("The safe attachment route or runner is no longer available on this server.");
      }
      if (error.status === 401 || error.status === 403) return notAttached("The server refused permission to attach the runner.");
      if (error.status === 400 || error.status === 422) return notAttached("The server rejected the attachment request.");
    }
    return { kind: "unconfirmed" };
  }
}

/** Called only after Create Space succeeds, with the id returned by that creation. */
export async function attachCreatedSpaceRunner(
  space: Space,
  choice: InstalledChoice,
  stillConsented: () => boolean,
): Promise<CreatedSpaceAttachment> {
  const c = apiClient();
  const sid = space.id;
  const spaceOK = sessionGuard(sid);
  const consentChanged = notAttached("The Create Space choice or context changed before attachment.");
  const runnerChanged = notAttached("This computer's runner is no longer available for this account.");
  const notOwner = notAttached("You no longer own this Space.");
  const alreadyPaired = notAttached("This Space already has a runner.");
  const unsupported = notAttached("This server doesn't support safe attachment to empty Spaces.");
  if (!c) return consentChanged;
  if (!choice.isCurrent()) return runnerChanged;
  if (!stillConsented()) return consentChanged;
  if (!spaceOK() || space.role !== "owner" || !ownsSpace(sid)) return notOwner;
  const checks = await Promise.all([readInstalledRunner(), c.listRunnerInventory(), c.listSpaces()]).catch(() => null);
  if (!choice.isCurrent()) return runnerChanged;
  if (!stillConsented()) return consentChanged;
  if (!spaceOK() || !ownsSpace(sid)) return notOwner;
  if (!checks) return notAttached("Metro couldn't verify the runner and the new Space.");
  const [local, inventory, liveSpaces] = checks;
  if (!inventory.attachIfEmpty) return unsupported;
  if (local?.runner?.id !== choice.id ||
      !inventory.runners.some((r) => r.id === choice.id && r.visibility === "personal" && r.canManage)) return runnerChanged;
  const live = liveSpaces.find((s) => s.id === sid);
  if (live?.role !== "owner") return notOwner;
  if (live.runnerPaired !== false) return alreadyPaired;
  const outcome = await attachAndReconcile(c, sid, choice, alreadyPaired.reason);
  if (outcome.kind !== "attached") return outcome;
  if (!outcome.stillVerified()) return { kind: "unconfirmed" };
  dismissRunnerOffer();
  return { kind: "attached", state: local.state };
}

/** Uses the same non-replacing route for every selected Space, one at a time. */
export async function attachMany(spaceIds: readonly string[]) {
  const c = apiClient();
  const runner = installedRunnerChoice();
  const choice = runner && snapshotInstalledRunner(runner.id);
  const selected = new Set(spaceIds);
  const picks = eligibleSpaces().filter((s) => selected.has(s.id))
    .map((s) => ({ id: s.id, name: s.name, expectedWrite: snapshotSpaceRunnerWrite(s.id) }));
  if (!c || !choice || !picks.length || runnerAttachBusy() || installedBusy()) return;
  const sessionOK = sessionGuard();
  const key = runnerSessionKey();
  const valid = () => sessionOK() && key === runnerSessionKey();
  const { id, isCurrent: matches } = choice;
  runnerAttachBusy.set(true);
  attachFeedback.set(null);
  const failures: string[] = [];
  let attached = 0;
  try {
    const [local, inventory, liveSpaces] = await Promise.all([readInstalledRunner(), c.listRunnerInventory(), c.listSpaces()]);
    if (!valid()) return;
    if (!inventory.attachIfEmpty) {
      if (matches()) ownedInstalledRunner.set({ id, attachIfEmpty: false });
      attachFeedback.set({ message: "This server doesn't support attaching only to Spaces without a runner.", incomplete: true });
      return;
    }
    if (!matches() || local?.runner?.id !== id ||
        !inventory.runners.some((r) => r.id === id && r.visibility === "personal" && r.canManage)) {
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
      if (!pick.expectedWrite()) {
        failures.push(`${pick.name}: ${changedSinceSelection.reason}`);
        continue;
      }
      const spaceOK = sessionGuard(s.id);
      try {
        const current = await c.getSpaceRunner(s.id);
        if (!valid()) return;
        if (!spaceOK() || !ownsSpace(s.id)) {
          failures.push(`${s.name}: You no longer own this Space.`);
          continue;
        }
        if (s.runnerPaired || current !== null) {
          failures.push(`${s.name}: Already has a runner.`);
          continue;
        }
        if (!matches()) {
          failures.push(`${s.name}: This computer's runner changed.`);
          continue;
        }
        const result = await attachAndReconcile(c, s.id, choice, "Already has a runner.", pick.expectedWrite);
        if (!valid()) return;
        if (result.kind === "attached" && result.stillVerified()) {
          attached++;
          dismissRunnerOffer();
        } else {
          const detail = result.kind === "not-attached" ? result.reason
            : result.kind === "superseded" ? `${result.reason} Check this Space before retrying.`
            : "The attachment wasn't confirmed. Check this Space before retrying.";
          failures.push(`${s.name}: ${detail}`);
        }
      } catch (error) {
        if (!valid()) return;
        failures.push(`${s.name}: ${actionError(error, "Metro couldn't check whether this Space already has a runner.")}`);
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

// One read-only poll for the visible Runners page while an accepted update
// is pending. Keep the flight across roster changes and page lifetimes;
// the session client aborts it on disconnect, not on a roster mutation.
let runnerRosterPoll: Promise<void> | null = null;
const shouldPollRunnerRoster = computed(() =>
  place() === "settings" && settingsSection() === "runners" && !!runners()?.some((r) => r.update?.pending));

effect(() => {
  if (!shouldPollRunnerRoster()) return;
  const valid = sessionGuard();
  const timer = setInterval(async () => {
    if (!valid() || !shouldPollRunnerRoster() || runnerRosterPoll || runnerBusy() || runnerUpdateBusy()) return;
    const read = loadRunners();
    runnerRosterPoll = read;
    try {
      await read;
    } finally {
      // A cancelled old session must not release a new session's flight.
      if (runnerRosterPoll === read) runnerRosterPoll = null;
    }
  }, RUNNER_REFRESH_MS);
  cleanup(() => clearInterval(timer));
});

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
    recordRunnerUpdateAccepted(id, pending, valid);
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
  runnerRosterPoll = null;
  runnerUpdateBusy.set(null);
  runnerUpdateErrors.set({});
});
