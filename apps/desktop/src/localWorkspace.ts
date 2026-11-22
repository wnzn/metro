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
// - The only thing remembered is which of the two the user last opened
//   (`metro.workspace`, not secret). At launch, "local" opens the local
//   workspace again; "remote" (or nothing) shows the connect screen as before.

import { batch, computed, signal, Storage } from "nsh";
import { BridgeError } from "./device/bridge";
import { desktopBridge } from "./device/state";
import { connect, connectedTo, connectError, status } from "./store";

const WORKSPACE_KEY = "metro.workspace";

export type WorkspaceChoice = "local" | "remote";

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
let restored = false;

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
  batch(() => {
    localOpening.set(true);
    localError.set(null);
    connectError.set(null);
  });
  try {
    const opened = await bridge.openLocalWorkspace();
    // The user chose something else meanwhile (a remote server): drop this.
    if (mine !== seq || status().phase !== "signedOut") return;
    localUrl.set(opened.serverUrl);
    await connect(opened.serverUrl, opened.token, { remember: false });
    if (mine !== seq) return;
    if (status().phase === "online" || status().phase === "retrying") {
      localAvailability.set("available");
      void remember("local");
    } else {
      // `connect` explains on the connect screen; show it with this choice instead.
      const why = connectError();
      batch(() => {
        localError.set(why ?? "Couldn't sign in to the local workspace.");
        connectError.set(null);
      });
    }
  } catch (error) {
    if (mine !== seq) return;
    localError.set(
      error instanceof BridgeError
        ? error.code === "local_unavailable"
          ? `This Metro install can't host a local workspace: ${error.message}`
          : error.message
        : "Couldn't open the local workspace.",
    );
    if (error instanceof BridgeError && error.code === "local_unavailable") localAvailability.set("unavailable");
  } finally {
    if (mine === seq) localOpening.set(false);
  }
}

/** The user chose a remote server: an open in flight no longer signs in. */
export function cancelLocalOpen() {
  if (!localOpening()) return;
  seq++;
  localOpening.set(false);
}

/** Records the workspace the user signed in to, for the next launch. */
export async function remember(choice: WorkspaceChoice): Promise<void> {
  try {
    await Storage.setItem(WORKSPACE_KEY, choice);
  } catch {
    /* storage unavailable: nothing is restored next time */
  }
}

/**
 * Once per launch: finds out whether a local workspace can be offered, and
 * opens it again if it is what the user last opened. Never runs after a
 * sign-out in the same launch.
 */
export async function restoreWorkspace(): Promise<void> {
  if (restored) return;
  restored = true;
  if (!desktopBridge()) return;
  let choice: string | null = null;
  try {
    choice = await Storage.getItem(WORKSPACE_KEY);
  } catch {
    choice = null;
  }
  const availability = await checkLocalWorkspace();
  if (choice === "local" && availability === "available" && status().phase === "signedOut") await openLocalWorkspace();
}
