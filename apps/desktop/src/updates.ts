// Launcher-scoped, not account-scoped. The launcher schedules and verifies
// checks. Polling here only reads the loopback state; it never checks GitHub.
import { platform, signal } from "nsh";
import { readBridgeEnv } from "./device/env";
import type { BridgeEndpoint } from "./device/env";

export const METRO_SOURCE = "https://github.com/wnzn/metro";
export const METRO_RELEASES = `${METRO_SOURCE}/releases`;
export const METRO_LICENSES = `${METRO_SOURCE}/tree/main/packaging/third-party-licenses`;

const versionPattern = /^(0|[1-9]\d{0,19})\.(0|[1-9]\d{0,19})\.(0|[1-9]\d{0,19})$/;
export function stableVersion(value: unknown): value is string {
  return typeof value === "string" && value === versionPattern.exec(value)?.[0];
}
export function newerVersion(candidate: unknown, current: unknown): boolean {
  if (!stableVersion(candidate) || !stableVersion(current)) return false;
  const older = current.split(".");
  for (const [i, part] of candidate.split(".").entries()) {
    if (part === older[i]) continue;
    return part.length > older[i].length || (part.length === older[i].length && part > older[i]);
  }
  return false;
}

/** No URL from a response or release note may pick even a different GitHub page. */
export function officialReleaseUrl(version: unknown, url: unknown): string | undefined {
  if (!stableVersion(version)) return;
  const expected = `${METRO_RELEASES}/tag/v${version}`;
  return url === expected ? expected : undefined;
}

const phases = ["idle", "checking", "downloading", "verifying", "staging", "ready", "applying", "failed"] as const;
const channels = ["release", "preview", "source"] as const;
const installs = ["linux-installer", "windows-script", "windows-msi", "portable", "unsupported"] as const;
const results = ["never", "up_to_date", "available", "error"] as const;
const outcomes = ["updated", "rolled_back", "install_failed", "not_confirmed"] as const;

type Release = { version: string; url?: string; notes?: string; notesTruncated: boolean };
type LastUpdate = { from: string; to: string; outcome: typeof outcomes[number]; at: number; detail?: string };
export type UpdateState = {
  current: { version: string; channel: typeof channels[number]; install: typeof installs[number] };
  phase: typeof phases[number];
  canCheck: boolean;
  canUpdateInApp: boolean;
  reason?: string;
  autoCheck: boolean;
  lastCheckedAt: number | null;
  result: typeof results[number];
  error?: string;
  anomaly?: string;
  persistenceError?: string;
  latest?: Release;
  progress?: number;
  failure?: string;
  busy: { servingConnections: number; localWorkspace: boolean };
  lastUpdate?: LastUpdate;
};

const record = (v: unknown): Record<string, unknown> =>
  typeof v === "object" && v !== null && !Array.isArray(v) ? v as Record<string, unknown> : {};
const oneOf = <T extends string>(v: unknown, choices: readonly T[]): T | undefined => choices.find(choice => choice === v);
const code = (v: unknown) => typeof v === "string" && /^[a-z_]{1,48}$/.test(v) ? v : undefined;
const count = (v: unknown) => typeof v === "number" && Number.isSafeInteger(v) && v >= 0 ? v : 0;
const timestamp = (v: unknown) => typeof v === "number" && count(v) && v <= 8.64e15 ? v : null;

// These are the launcher's fixed, non-sensitive failure details. Never display
// an arbitrary bridge error body, which can also come from an older launcher.
const safeOutcomeDetails = [
  "The installer is busy. Close the other installer and try again.",
  "The staged update could not be verified. Check again before restarting.",
  "The local workspace could not be backed up. The update was not started.",
] as const;

class UpdateBridgeError extends Error {
  constructor(readonly code: string, readonly status = 0) { super(code); }
}

function parseState(value: unknown): UpdateState {
  const v = record(value);
  const current = record(v.current);
  const channel = oneOf(current.channel, channels);
  const install = oneOf(current.install, installs);
  const phase = oneOf(v.phase, phases);
  const result = oneOf(v.result, results);
  if (!stableVersion(current.version) || !channel || !install || !phase || !result
      || typeof v.can_check !== "boolean" || typeof v.can_update_in_app !== "boolean" || typeof v.auto_check !== "boolean") {
    throw new UpdateBridgeError("protocol");
  }
  const canCheck = channel === "release" && install !== "unsupported" && v.can_check;
  // Belt and braces: Windows always stays manual, independent of flags.
  const canUpdateInApp = platform.name === "linux" && canCheck && install === "linux-installer"
    && v.can_update_in_app && v.reason !== "manual_migration_required";
  const latest = record(v.latest);
  const busy = record(v.busy);
  const progress = record(v.progress);
  const total = count(progress.total);
  const last = record(v.last_update);
  const outcome = oneOf(last.outcome, outcomes);
  const at = timestamp(last.at);
  return {
    current: { version: current.version, channel, install },
    phase: canUpdateInApp || phase === "checking" || phase === "failed" ? phase : "idle",
    result, canCheck, canUpdateInApp,
    reason: code(v.reason),
    autoCheck: v.auto_check,
    lastCheckedAt: timestamp(v.last_checked_at),
    error: code(v.error),
    anomaly: code(v.anomaly),
    persistenceError: code(v.persistence_error),
    latest: stableVersion(latest.version) ? {
      version: latest.version,
      url: officialReleaseUrl(latest.version, latest.release_url),
      notes: typeof latest.notes === "string" ? latest.notes.slice(0, 32 * 1024) : undefined,
      notesTruncated: latest.notes_truncated === true || (typeof latest.notes === "string" && latest.notes.length > 32 * 1024),
    } : undefined,
    progress: total ? Math.min(100, 100 * count(progress.done) / total) : undefined,
    failure: code(record(v.failure).code),
    busy: { servingConnections: count(busy.serving_connections), localWorkspace: busy.local_workspace === true },
    lastUpdate: outcome && at !== null && stableVersion(last.from) && stableVersion(last.to)
      ? { from: last.from, to: last.to, outcome, at, detail: oneOf(last.detail, safeOutcomeDetails) } : undefined,
  };
}

function updateBridge(endpoint: BridgeEndpoint) {
  async function call(method: "GET" | "POST" | "PUT", suffix: string, body?: object, timeout = 5_000) {
    let response: Response;
    try {
      response = await fetch(`${endpoint.url}/v1/update${suffix}`, {
        method, body: body === undefined ? undefined : JSON.stringify(body),
        headers: { Accept: "application/json", Authorization: endpoint.authorization(),
          ...(body === undefined ? {} : { "Content-Type": "application/json" }) },
        redirect: "error", signal: AbortSignal.timeout(timeout),
      });
    } catch {
      throw new UpdateBridgeError("unreachable");
    }
    let data: unknown;
    try { data = await response.json(); } catch { /* An older launcher may send no JSON on a 404. */ }
    if (!response.ok) throw new UpdateBridgeError(code(record(record(data).error).code) ?? "unavailable", response.status);
    return data;
  }
  return {
    read: async () => parseState(await call("GET", "")),
    started: () => call("POST", "/started", {}),
    check: async () => parseState(await call("POST", "/check", {}, 240_000)),
    settings: async (auto_check: boolean) => parseState(await call("PUT", "/settings", { auto_check })),
    download: async (version: string) => parseState(await call("POST", "/download", { version })),
    restart: async (version: string, confirm_interrupt: boolean) => parseState(await call("POST", "/apply", { version, confirm_interrupt })),
    cancel: async () => parseState(await call("POST", "/cancel", {})),
  };
}

export function verifiedOffer(s: UpdateState | null): Release | undefined {
  if (!s?.canCheck || s.result !== "available" || s.error || s.anomaly || s.persistenceError
      || !s.latest || !newerVersion(s.latest.version, s.current.version)) return;
  // A failed restart should not immediately re-offer that same version on
  // the strength of the old check. Checking again never downloads anything.
  const last = s.lastUpdate;
  if (last && last.outcome !== "updated" && last.to === s.latest.version && (s.lastCheckedAt ?? 0) <= last.at) return;
  return s.latest;
}

type Command = "check" | "settings" | "download" | "cancel" | "restart";
const activeTransfer = (s: UpdateState | null) => s?.phase === "downloading" || s?.phase === "verifying" || s?.phase === "staging";
const canCancel = (s: UpdateState | null) => s?.phase === "downloading" || s?.phase === "verifying";

function createDesktopUpdates(endpoint: BridgeEndpoint | null) {
  const bridge = endpoint && updateBridge(endpoint);
  const state = signal<UpdateState | null>(null);
  const readProblem = signal<string | null>(bridge ? null : "no_launcher");
  const problem = signal<{ command: Command; code: string } | null>(null);
  const pending = signal<Command | null>(null);
  const cancelling = signal(false);
  const notice = signal<string | null>(null);
  const needsCheck = signal(false);
  const dismissedOutcome = signal("");
  const offer = () => !needsCheck() && !readProblem() ? verifiedOffer(state()) : undefined;
  const outcome = () => {
    const last = state()?.lastUpdate;
    return last && outcomeKey(last) !== dismissedOutcome() ? last : undefined;
  };
  const outcomeKey = (last: LastUpdate) => `${last.from}/${last.to}/${last.at}/${last.outcome}`;
  let started = false;
  let healthSent = false;
  let readInFlight: Promise<void> | undefined;
  let revision = 0;
  let viewers = 0;
  let timer: ReturnType<typeof setTimeout> | undefined;

  function publish(s: UpdateState) {
    state.set(s);
    readProblem.set(null);
    if (cancelling() && !activeTransfer(s)) {
      cancelling.set(false);
      if (s.phase === "idle") notice.set("Download cancelled. No update was installed.");
      if (s.phase === "ready") notice.set("The update was already prepared. You can leave it for later.");
    }
    // The bridge has no trial flag in GET. Only this install kind can be
    // launched in trial mode. POST is a no-op on its normal launches.
    if (started && !healthSent && s.canUpdateInApp) {
      healthSent = true;
      void bridge?.started().catch(() => {}); // A failed signal is not startup success; the watchdog decides.
    }
  }

  function schedule() {
    clearTimeout(timer);
    if (!started || !bridge || readProblem() === "old_launcher" || readProblem() === "update_unavailable") return;
    const s = state();
    if (s && !s.canCheck && !readProblem()) return;
    const delay = activeTransfer(s) || pending() === "check" ? 500 : viewers ? 2_000 : 30_000;
    timer = setTimeout(() => void refresh(), delay);
  }

  function refresh(): Promise<void> {
    if (!bridge) return Promise.resolve();
    if (readInFlight) return readInFlight;
    const observed = revision;
    readInFlight = bridge.read().then(s => {
      if (observed === revision) publish(s);
    }, (e: UpdateBridgeError) => {
      if (observed !== revision) return;
      if (state()?.phase !== "applying") readProblem.set(e.status === 404 ? "old_launcher" : e.code);
    }).finally(() => {
      readInFlight = undefined;
      schedule();
    });
    return readInFlight;
  }

  async function run(command: Command, act: () => Promise<UpdateState>): Promise<string | null> {
    ++revision;
    pending.set(command);
    problem.set(null);
    notice.set(null);
    if (command === "cancel") cancelling.set(true);
    schedule();
    let refusal = null;
    try {
      const s = await act();
      ++revision; // No in-flight GET may overwrite a newer action response.
      if (command === "check" && !s.error && !s.anomaly && s.result !== "error" && s.persistenceError !== "status_io") needsCheck.set(false);
      publish(s);
    } catch (e) {
      ++revision;
      refusal = e instanceof UpdateBridgeError ? e.code : "unavailable";
      if (command === "cancel") cancelling.set(false);
      if (["not_latest", "not_ready", "not_verified", "not_in_app"].includes(refusal)) needsCheck.set(true);
      if (refusal !== "would_interrupt") problem.set({ command, code: refusal });
      // An apply can lose its reply during shutdown; never retry it for the
      // user. A refusal may also be racing with a newer release or busy work.
      await readInFlight;
      const read = refresh();
      if (refusal === "would_interrupt") await read;
    } finally {
      pending.set(null);
      schedule();
    }
    return refusal;
  }

  return {
    state, readProblem, problem, pending, cancelling, notice, offer, outcome,
    dismissOutcome: (last: LastUpdate) => dismissedOutcome.set(outcomeKey(last)),
    refresh,
    /** Called by App only after the UI has a nonempty native layout. */
    start() {
      if (!bridge || started) return;
      started = true;
      if (state()) publish(state()!);
      void refresh();
    },
    /** Faster reads while the About page is open, without owning the lifecycle. */
    watch() {
      ++viewers;
      void refresh();
      return () => { --viewers; schedule(); };
    },
    check() {
      const s = state();
      if (!bridge || !s?.canCheck || readProblem() || pending() || activeTransfer(s) || s.phase === "checking" || s.phase === "applying") return;
      return run("check", bridge.check);
    },
    setAutoCheck(value: boolean) {
      if (!bridge || !state()?.canCheck || readProblem() || pending() || state()?.phase === "applying") return;
      return run("settings", () => bridge.settings(value));
    },
    download(version: string) {
      if (!bridge || !state()?.canUpdateInApp || state()?.phase !== "idle" || pending() || offer()?.version !== version) return;
      return run("download", () => bridge.download(version));
    },
    restart(version: string, confirmInterrupt: boolean) {
      if (!bridge || !state()?.canUpdateInApp || state()?.phase !== "ready" || pending() || offer()?.version !== version) return;
      return run("restart", () => bridge.restart(version, confirmInterrupt));
    },
    cancel() {
      if (!bridge || !state()?.canUpdateInApp || !canCancel(state()) || readProblem() || pending() || cancelling()) return;
      return run("cancel", bridge.cancel);
    },
  };
}

const env = readBridgeEnv();
export const desktopUpdates = createDesktopUpdates(env.present ? env.endpoint : null);
/** Quiet navigation affordance: no error, popup, or notifications. */
export const updateAvailable = () => !!desktopUpdates.offer() &&
  (desktopUpdates.state()?.phase === "idle" || desktopUpdates.state()?.phase === "ready");
