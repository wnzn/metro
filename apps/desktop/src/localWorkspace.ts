// The desktop-hosted local workspace: a private Metro server that the
// packaged launcher runs on this computer, for one person, with no URL, token
// or terminal. Pages read these signals and call these functions; nothing
// here knows about the UI.
//
// - Only with the packaged launcher (src/device/state.ts `desktopBridge`), and
//   only when it reports a bundled server (`GET /v1/status` → `local`).
// - "Use locally" asks the bridge (`POST /v1/local-workspace/open`)
//   for the server's URL and a session bearer, then signs in with the normal
//   `connect`, in memory. The bearer is never kept here, stored, shown or
//   logged; it changes whenever the local server restarts.
// - Remote servers stay an explicit choice. Opening locally never replaces a
//   session that is signed in or connecting, and a local session never
//   overwrites the remembered remote server URL. Nothing falls back from one
//   to the other: a failure is shown as it is.
// - The explicit startup preference lives in startupWorkspace.ts. Opening a
//   workspace never changes that preference.

import { batch, computed, signal } from "nsh";
import { BridgeError } from "./device/bridge";
import { desktopBridge } from "./device/state";
import { adoptConnection, connectedTo, connectError, describe, prepareConnection, status } from "./store";
import type { PreparedConnection } from "./store";

export type LocalAvailability =
  /** No packaged launcher (`nsh run`, tests, a hand-started UI): nothing to offer. */
  | "absent"
  | "checking"
  | "available"
  /** The launcher runs, but this install has no bundled local server. */
  | "unavailable"
  /** The launcher's helper didn't answer. */
  | "unreachable";

export const localAvailability = signal<LocalAvailability>(desktopBridge() ? "checking" : "absent");
/** Opening the local workspace: the bridge starts it, then the app signs in. */
export const localOpening = signal(false);
/** Why the last attempt to open the local workspace failed, as the helper or server said it. */
export const localError = signal<string | null>(null);
/** The local workspace's URL, once opened in this run (not secret). */
const localUrl = signal<string | null>(null);
/** The current session is the local workspace (for labels: "On this computer"). */
export const onLocalWorkspace = computed(() => {
  const url = connectedTo();
  return url !== null && url === localUrl();
});

let seq = 0;
let opening: AbortController | null = null;

/** Asks the launcher whether it can host a local workspace. */
export async function checkLocalWorkspace(): Promise<LocalAvailability> {
  const bridge = desktopBridge();
  if (!bridge) return "absent";
  try {
    const s = await bridge.status();
    // An older launcher that doesn't report `local` has no local server.
    const next: LocalAvailability = s.local === null || s.local === "unavailable" ? "unavailable" : "available";
    localAvailability.set(next);
    return next;
  } catch {
    localAvailability.set("unreachable");
    return "unreachable";
  }
}

/**
 * Starts (or reuses) the local workspace and signs in to it. Does nothing
 * while a session is signed in or connecting: the user leaves that first.
 */
export async function openLocalWorkspace(): Promise<void> {
  const bridge = desktopBridge();
  if (!bridge || localOpening() || status().phase !== "signedOut") return;
  const mine = ++seq;
  const ac = new AbortController();
  opening = ac;
  batch(() => {
    localOpening.set(true);
    localError.set(null);
    connectError.set(null);
  });
  try {
    const opened = await bridge.openLocalWorkspace();
    // The user chose something else meanwhile (a remote server): drop this.
    if (mine !== seq || status().phase !== "signedOut") return;
    const prepared = await prepareConnection(opened.serverUrl, opened.token, ac.signal);
    if (mine !== seq || status().phase !== "signedOut") return;
    commitLocalWorkspace(prepared);
  } catch (error) {
    if (mine !== seq) return;
    localError.set(
      error instanceof BridgeError
        ? error.code === "local_unavailable"
          ? `This Metro install can't host a local workspace: ${error.message}`
          : error.message
        : describe(error, "Couldn't open the local workspace"),
    );
    if (error instanceof BridgeError && error.code === "local_unavailable") localAvailability.set("unavailable");
  } finally {
    if (mine === seq) {
      localOpening.set(false);
      opening = null;
    }
  }
}

/**
 * Starts (or reuses) the local workspace and returns its address and a
 * session bearer, without signing in: the account switcher checks them with
 * a client of its own before it leaves the current session. The bearer goes
 * straight to `commitLocalWorkspace` and is never kept.
 */
export async function prepareLocalWorkspace(): Promise<{ ok: true; serverUrl: string; token: string } | { ok: false; message: string }> {
  const bridge = desktopBridge();
  if (!bridge || localAvailability() !== "available") return { ok: false, message: "This computer's workspace isn't available in this install." };
  try {
    const opened = await bridge.openLocalWorkspace();
    return { ok: true, serverUrl: opened.serverUrl, token: opened.token };
  } catch (error) {
    if (error instanceof BridgeError && error.code === "local_unavailable") localAvailability.set("unavailable");
    return {
      ok: false,
      message:
        error instanceof BridgeError
          ? error.code === "local_unavailable"
            ? `This Metro install can't host a local workspace: ${error.message}`
            : error.message
          : "Couldn't open the local workspace.",
    };
  }
}

/** Adopt the already verified local session; leave the remembered remote address alone. */
export function commitLocalWorkspace(prepared: PreparedConnection): void {
  localUrl.set(prepared.baseUrl);
  adoptConnection(prepared, { remember: false });
  localAvailability.set("available");
}

/** The user chose a remote server: an open in flight no longer signs in. */
export function cancelLocalOpen() {
  seq++;
  opening?.abort();
  opening = null;
  localOpening.set(false);
}
