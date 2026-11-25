// Shared by Settings › Runners and the entry points that offer this computer's runner.
import { cleanup, computed, effect, signal, Storage, untracked } from "nsh";
import { ApiError } from "./api/client";
import type { RegisteredRunner, Space } from "./api/client";
import { loadRunners, runnerBusy, runners, spaceRunners } from "./settings";
import { apiClient, connectedTo, me, onSessionEnd, sessionGuard, setRunnerPaired, spaces } from "./store";
import { helper } from "./device/state";
import { installedBusy, installedRunner, loadInstalledRunner, ownedInstalledRunner, readInstalledRunner, recordAttachedFolder } from "./features/installedRunner/installedRunner";

export const runnerSessionKey = () => JSON.stringify([connectedTo(), me()]);
export const runnerOfferStorageKey = (server: string, userId: string) =>
  `runnerOffer:${encodeURIComponent(server)}:${encodeURIComponent(userId)}`;

const offer = signal<{ key: string; dismissed: boolean } | null>(null);
const dismissedThisLaunch = new Set<string>();
const attachFeedback = signal<{ message: string; incomplete: boolean } | null>(null);
export const runnerAttachFeedback = () => attachFeedback();
export const runnerAttachBusy = signal(false);

export const installedRunnerChoice = computed(() => {
  const runner = installedRunner()?.runner;
  const owned = ownedInstalledRunner();
  return runner && runner.id === owned?.id && owned.attachIfEmpty ? runner : null;
});

const isEmptyOwned = (s: Space) => s.role === "owner" && s.runnerPaired === false;

export const eligibleSpaces = computed(() => installedRunnerChoice() ? spaces().filter(isEmptyOwned) : []);
const offerKey = () => {
  const server = connectedTo();
  const user = me();
  return server && user ? runnerOfferStorageKey(server, user) : null;
};

export const runnerOfferCount = computed(() => {
  const state = offer();
  return state && state.key === offerKey() && !state.dismissed && installedRunner()?.state === "online" ? eligibleSpaces().length : 0;
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

/** Read-only on mount and session switch. U10 can also mount it from Sidebar. */
export function watchInstalledRunner() {
  effect(() => {
    connectedTo();
    me();
    const phase = helper().phase;
    untracked(() => {
      if (!offerKey()) return;
      void loadOfferDismissal();
      // The launcher only reports its installed runner after its session
      // has been bound. Sidebar can mount before that asynchronous handshake.
      if (phase === "checking") return;
      const valid = sessionGuard();
      let stopped = false;
      let timer: ReturnType<typeof setTimeout> | undefined;
      const read = async (remaining: number) => {
        if (stopped || !valid()) return;
        if (!runnerAttachBusy()) await loadInstalledRunner();
        const state = installedRunner()?.state;
        if (!stopped && valid() && remaining > 0 &&
            (state === "starting" || state === "offline" || state === "idle")) {
          timer = setTimeout(() => void read(remaining - 1), 5000);
        }
      };
      cleanup(() => {
        stopped = true;
        if (timer !== undefined) clearTimeout(timer);
      });
      void read(6);
    });
  });
}

function actionError(error: unknown, fallback: string) {
  if (!(error instanceof ApiError)) return fallback;
  if (error.status === 401 || error.status === 403) return "You no longer have permission to do this.";
  if (error.status === 404 || error.status === 405) return "The requested action or item is no longer available on this server.";
  if (error.kind === "timeout" || error.kind === "network") return "The server did not confirm the action. Check again before retrying.";
  return fallback;
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
        const result = await c.attachRunnerIfEmpty(s.id, runner.id);
        if (!valid()) return;
        attached++;
        dismissRunnerOffer();
        if (!spaceOK() || !spaces().some((space) => space.id === s.id && space.role === "owner")) continue;
        setRunnerPaired(s.id, true);
        spaceRunners.set({ ...spaceRunners(), [s.id]: result });
        if (matches()) recordAttachedFolder({ id: s.id, name: s.name });
      } catch (error) {
        if (!valid()) return;
        if (error instanceof ApiError && error.status === 409) {
          if (spaceOK()) setRunnerPaired(s.id, true);
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
    runnerUpdateErrors.set({ ...runnerUpdateErrors(), [id]: message });
  } finally {
    if (valid()) {
      runnerUpdateBusy.set(null);
      void loadRunners();
    }
  }
}

onSessionEnd(() => {
  offer.set(null);
  attachFeedback.set(null);
  runnerAttachBusy.set(false);
  runnerUpdateBusy.set(null);
  runnerUpdateErrors.set({});
});
