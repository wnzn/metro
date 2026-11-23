// Startup is a device preference, independent of the current session. Only
// "local" or an id from this device's saved list is stored, never an address
// supplied by a third party, an account id or a credential.
import { batch, signal, Storage } from "nsh";
import { desktopBridge } from "./device/state";
import { cancelLocalOpen, checkLocalWorkspace, openLocalWorkspace } from "./localWorkspace";
import { loadSavedServers, savedServers } from "./savedWorkspaces";
import type { SavedServer } from "./savedWorkspaces";
import { onSessionStart, restorePreferences, serverUrlInput, status } from "./store";

const KEY = "metro.startupServer";
export const startupServerId = signal("local");
export const startupError = signal<string | null>(null);
export const startupOpening = signal(!!desktopBridge());
export const startupPrompt = signal<SavedServer | null>(null);
let loaded: Promise<void> | undefined;
let writes: Promise<unknown> = Promise.resolve();
let started = false;
let manual = false;

function loadChoice(): Promise<void> {
  return loaded ??= Promise.all([Storage.getItem(KEY).catch(() => null), loadSavedServers()])
    .then(([id]) => { startupServerId.set(savedServers().find((s) => s.id === id)?.id ?? "local"); });
}

export function setStartupServer(id: string): Promise<boolean> {
  const next = writes.then(async () => {
    await loadChoice();
    if (id !== "local" && !savedServers().some((s) => s.id === id)) {
      startupError.set("That server is no longer saved.");
      return false;
    }
    try {
      await Storage.setItem(KEY, id);
      batch(() => { startupServerId.set(id); startupError.set(null); });
      return true;
    } catch {
      startupError.set("Couldn’t save the startup server on this computer. Try again.");
      return false;
    }
  });
  writes = next.catch(() => {});
  return next;
}

export async function clearForgottenDefault(id: string): Promise<void> {
  await loadChoice();
  await writes;
  if (startupServerId() === id && !(await setStartupServer("local"))) {
    // The id is no longer in the saved list even if storage is read-only.
    startupServerId.set("local");
  }
}

/** A user's explicit choice wins over even a late helper/status response. */
export function chooseManually(): void {
  manual = true;
  cancelLocalOpen();
  batch(() => { startupOpening.set(false); startupPrompt.set(null); });
}

onSessionStart(() => startupPrompt.set(null));

export async function startWorkspace(): Promise<void> {
  if (started) return;
  started = true;
  const availability = checkLocalWorkspace();
  try {
    await Promise.all([restorePreferences(() => !manual), loadChoice()]);
    if (manual || status().phase !== "signedOut") return;
    const chosen = savedServers().find((s) => s.id === startupServerId());
    if (chosen) {
      // Tokens don't survive a restart. Show just this server's login; there
      // is no reason to contact it (or open the bundled server) yet.
      batch(() => { serverUrlInput.set(chosen.baseUrl); startupPrompt.set(chosen); });
    } else if (await availability === "available" && !manual && status().phase === "signedOut") {
      await openLocalWorkspace();
    }
  } finally {
    startupOpening.set(false);
  }
}
