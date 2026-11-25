// Saved servers and switching between workspaces: the user's own list of
// Metro servers (a name and an address), "On this computer" (the
// desktop-hosted local workspace, src/localWorkspace.ts), and moving between
// them from the account menu without signing out first.
//
// What is kept, and where:
// - The list itself is not secret: a name and a canonical server address
//   (parseServerUrl: https://, or http:// on loopback only, with an optional
//   base path), stored as `metro.savedServers`. No token, ever.
// - Only when the user asks ("Keep me signed in until Metro quits", off by
//   default), a saved server's access token is kept in this run's memory, so
//   switching back needs no second paste. It is used only for the exact
//   address it was entered for, never stored, and dropped by Sign out (of
//   that server), Forget, a refusal (401) or quitting. Otherwise, and after a
//   restart, Metro asks for it again.
// - The local workspace's bearer comes from the launcher each time and is
//   never kept here.
//
// Switching is verify-then-switch: the target is checked first with a client
// of its own (who am I, and the Space list the app starts with), while the
// current session carries on untouched. Only when that works does the app
// leave the current session the normal way (every module drops its session
// state; answers still in flight are ignored) and sign in to the target. A
// check that fails (refused token, unreachable server, no local workspace)
// leaves the user where they were, with the reason on the row they chose.
// Nothing ever falls back to another server, account or the local workspace.

import { batch, computed, signal, Storage, untracked } from "nsh";
import { ApiError } from "./api/client";
import { parseServerUrl } from "./api/serverUrl";
import { hasPendingAttachments } from "./features/attachments/attachmentDraft";
import { commitLocalWorkspace, onLocalWorkspace, prepareLocalWorkspace } from "./localWorkspace";
import { clearForgottenDefault } from "./startupWorkspace";
import { adoptConnection, connectedTo, describe, disconnect, dispatchDraft, dispatching, draft, onSessionEnd, stashedDraftPending, onSessionStart, prepareConnection, sending, serverUrlInput, sessionGuard, status } from "./store";

export const SAVED_SERVERS_KEY = "metro.savedServers";
/** Names are short labels; addresses are checked by parseServerUrl. */
export const MAX_SERVER_NAME = 60;
export const MAX_SAVED_SERVERS = 20;

export type SavedServer = { id: string; name: string; baseUrl: string };

/** Where the user can be: a saved server, or this computer's own workspace. */
export type WorkspaceTarget = { kind: "local" } | { kind: "server"; server: SavedServer };

export const savedServers = signal<SavedServer[]>([]);
/** A switch in progress (its target's label), for the menu and screen readers. */
export const switching = signal<string | null>(null);
/** Why the last save, rename or switch didn't happen. */
export const savedError = signal<string | null>(null);

// ------------------------------------------------------------------ memory-only tokens

/** Tokens of saved servers the user signed in to in this run, by exact address. Never stored. */
const tokens = new Map<string, string>();
/** The current remote session, to keep its token if the user saves it. Cleared when the session ends. */
let current: { baseUrl: string; token: string } | null = null;

onSessionStart(({ baseUrl, token }) => {
  if (untracked(onLocalWorkspace)) return; // the local bearer is the launcher's, never kept
  current = { baseUrl, token };
});
onSessionEnd(() => {
  current = null;
});

/** Bumped when the tokens in memory change, so labels ("signed in this session") follow. */
const tokensVersion = signal(0);
const setToken = (baseUrl: string, token: string) => {
  tokens.set(baseUrl, token);
  tokensVersion.set(untracked(tokensVersion) + 1);
};
const dropToken = (baseUrl: string) => {
  if (tokens.delete(baseUrl)) tokensVersion.set(untracked(tokensVersion) + 1);
};

/** A saved server's token is in memory (switching to it needs no paste). */
export function hasToken(baseUrl: string): boolean {
  tokensVersion();
  return tokens.has(baseUrl);
}

/** The user asked to stay signed in to the current saved server until Metro quits: keep its token in memory. */
export function keepCurrentSignedIn(): boolean {
  if (!current || !isSaved(current.baseUrl) || current.baseUrl !== untracked(connectedTo)) return false;
  setToken(current.baseUrl, current.token);
  return true;
}

// ------------------------------------------------------------------ the list

const isSaved = (baseUrl: string) => untracked(savedServers).some((s) => s.baseUrl === baseUrl);

/** The saved server the session is on now, if any. */
export const activeSaved = computed(() => {
  const url = connectedTo();
  if (!url || onLocalWorkspace()) return null;
  return savedServers().find((s) => s.baseUrl === url) ?? null;
});

/** The default name for an address: its host, and its base path if it has one. */
export function defaultName(baseUrl: string): string {
  try {
    const u = new URL(baseUrl);
    return `${u.host}${u.pathname === "/" ? "" : u.pathname}`;
  } catch {
    return baseUrl;
  }
}

/** The address shown under a name: without the scheme for https, in full otherwise (http is loopback only). */
export function addressLabel(baseUrl: string): string {
  return baseUrl.startsWith("https://") ? baseUrl.slice("https://".length) : baseUrl;
}

function cleanName(name: string, baseUrl: string): string {
  const n = name.replace(/\s+/g, " ").trim().slice(0, MAX_SERVER_NAME);
  return n || defaultName(baseUrl);
}

function newId(): string {
  return `srv-${Date.now().toString(36)}-${Math.floor(Math.random() * 1e9).toString(36)}`;
}

let loading: Promise<void> | null = null;

/**
 * Reads the list, once per run (later calls wait for the same read, so a
 * read can never overwrite a save). Anything malformed, or an address that
 * no longer passes the rules, is left out.
 */
export function loadSavedServers(): Promise<void> {
  loading ??= readSavedServers();
  return loading;
}

async function readSavedServers(): Promise<void> {
  let raw: string | null = null;
  try {
    raw = await Storage.getItem(SAVED_SERVERS_KEY);
  } catch {
    raw = null;
  }
  const out: SavedServer[] = [];
  try {
    const list = raw ? (JSON.parse(raw) as unknown) : [];
    if (Array.isArray(list)) {
      for (const v of list) {
        if (!v || typeof v !== "object") continue;
        const { id, name, baseUrl } = v as Record<string, unknown>;
        if (typeof id !== "string" || !id || id === "local" || out.some((s) => s.id === id) || typeof baseUrl !== "string") continue;
        const parsed = parseServerUrl(baseUrl);
        if (!parsed.ok || parsed.baseUrl !== baseUrl || out.some((s) => s.baseUrl === baseUrl)) continue;
        out.push({ id, name: cleanName(typeof name === "string" ? name : "", baseUrl), baseUrl });
      }
    }
  } catch {
    /* unreadable: start empty */
  }
  savedServers.set(out.slice(0, MAX_SAVED_SERVERS));
}

async function persist(list: SavedServer[]): Promise<void> {
  await loadSavedServers();
  savedServers.set(list);
  try {
    // Only id, name and address: never a token.
    await Storage.setItem(SAVED_SERVERS_KEY, JSON.stringify(list.map(({ id, name, baseUrl }) => ({ id, name, baseUrl }))));
  } catch {
    /* storage unavailable: kept for this run only */
  }
}

/**
 * Saves a server, or renames it when its address is already saved (unless
 * `keepName`: then an already-saved server is left as it is). Returns the
 * saved entry, or null with `savedError` set.
 */
export async function saveServer(urlInput: string, name: string, opts: { keepName?: boolean } = {}): Promise<SavedServer | null> {
  await loadSavedServers();
  const entry = serverCandidate(urlInput, name);
  if (!entry) return null;
  const list = untracked(savedServers);
  const existing = list.find((s) => s.id === entry.id);
  if (existing && opts.keepName) return existing;
  await persist(existing ? list.map((s) => s.id === entry.id ? entry : s) : [...list, entry]);
  savedError.set(null);
  return entry;
}

/** Preview an add or rename without changing the saved list before sign-in. */
export function serverCandidate(urlInput: string, name: string): SavedServer | null {
  const parsed = parseServerUrl(urlInput);
  if (!parsed.ok) {
    savedError.set(parsed.error);
    return null;
  }
  const list = untracked(savedServers);
  const existing = list.find((s) => s.baseUrl === parsed.baseUrl);
  if (existing) return { ...existing, name: cleanName(name, existing.baseUrl) };
  if (list.length >= MAX_SAVED_SERVERS) {
    savedError.set(`You can save up to ${MAX_SAVED_SERVERS} servers. Forget one first.`);
    return null;
  }
  return { id: newId(), name: cleanName(name, parsed.baseUrl), baseUrl: parsed.baseUrl };
}

/**
 * Saves the server the session is on now (not the local workspace). With
 * `keep`, its token stays in memory until Metro quits, for switching back.
 */
export async function saveCurrentServer(name: string, opts: { keep?: boolean; keepName?: boolean } = {}): Promise<SavedServer | null> {
  const url = connectedTo();
  if (!url || onLocalWorkspace()) return null;
  const entry = await saveServer(url, name, { keepName: opts.keepName });
  if (entry && opts.keep) keepCurrentSignedIn();
  return entry;
}

export async function renameServer(id: string, name: string): Promise<void> {
  const list = untracked(savedServers);
  const s = list.find((x) => x.id === id);
  if (!s) return;
  await persist(list.map((x) => (x.id === id ? { ...x, name: cleanName(name, x.baseUrl) } : x)));
}

/** Removes a saved server and its token from memory. The session on it, if any, carries on until the user leaves it. */
export async function forgetServer(id: string): Promise<void> {
  const list = untracked(savedServers);
  const s = list.find((x) => x.id === id);
  if (!s) return;
  dropToken(s.baseUrl);
  await persist(list.filter((x) => x.id !== id));
  await clearForgottenDefault(id);
}

// ------------------------------------------------------------------ switching and signing out

// Other parts of the app with unsent work of their own (files waiting to be
// attached, for instance) register a check here; switching asks first while
// any says yes.
const unsentChecks: (() => boolean)[] = [];
/** Registers "the user has something not sent yet" for the switch to ask about. */
export function registerUnsentCheck(fn: () => boolean): void {
  unsentChecks.push(fn);
}

/** Something would be lost by leaving: text not sent (a room or Chat message in any place, a task), a send in flight, or another registered check. */
export const unsentDraft = computed(
  () => draft().trim().length > 0 || stashedDraftPending() || dispatchDraft().trim().length > 0 || sending() !== null || dispatching() || hasPendingAttachments() || unsentChecks.some((fn) => fn()),
);

let switchSeq = 0;
let verifying: AbortController | null = null;

const isTarget = (t: WorkspaceTarget) => (t.kind === "local" ? untracked(onLocalWorkspace) : untracked(connectedTo) === t.server.baseUrl && !untracked(onLocalWorkspace));

/** The target is where the session is now. */
export function isActive(t: WorkspaceTarget): boolean {
  return isTarget(t);
}

export function cancelSwitch(): void {
  switchSeq++;
  verifying?.abort();
  verifying = null;
  switching.set(null);
}

/**
 * Moves to `target` in one step, verify-then-switch. A saved server needs
 * its token in memory, or `tokenInput` (typed just now). Returns whether the
 * target is now signed in; when not, the current session is as it was and
 * `savedError` says why.
 */
export async function switchTo(target: WorkspaceTarget, tokenInput?: string, opts: { keep?: boolean } = {}): Promise<boolean> {
  const source = sessionGuard();
  const fromSignedOut = status().phase === "signedOut";
  if (isTarget(target) && !fromSignedOut && tokenInput === undefined) return true;
  const mine = ++switchSeq;
  verifying?.abort();
  const ac = new AbortController();
  verifying = ac;
  const stale = () => mine !== switchSeq || !source() || (!fromSignedOut && status().phase === "signedOut");
  const name = target.kind === "local" ? "On this computer" : target.server.name;
  batch(() => {
    savedError.set(null);
    switching.set(name);
  });
  try {
    if (target.kind === "local") {
      const prepared = await prepareLocalWorkspace();
      if (stale()) return false;
      if (!prepared.ok) {
        savedError.set(`Couldn't open ${name}: ${prepared.message}`);
        return false;
      }
      const checked = await prepareConnection(prepared.serverUrl, prepared.token, ac.signal);
      if (stale()) return false;
      commitLocalWorkspace(checked);
      return true;
    }
    const s = target.server;
    const token = tokenInput === undefined ? tokens.get(s.baseUrl) : tokenInput.trim();
    if (!token) {
      savedError.set(`Enter your access token for ${s.name}.`);
      return false;
    }
    if (/\s/.test(token)) {
      savedError.set("The access token contains spaces or line breaks; paste only the token itself.");
      return false;
    }
    const checked = await prepareConnection(s.baseUrl, token, ac.signal);
    if (stale()) return false;
    batch(() => {
      adoptConnection(checked);
      serverUrlInput.set(checked.baseUrl);
    });
    if (opts.keep) setToken(s.baseUrl, token);
    else if (tokenInput !== undefined) dropToken(s.baseUrl);
    return true;
  } catch (error) {
    if (stale()) return false;
    const refused = error instanceof ApiError && error.kind === "auth";
    if (refused && target.kind === "server" && (tokenInput === undefined || tokens.get(target.server.baseUrl) === tokenInput.trim())) dropToken(target.server.baseUrl);
    const message = refused ? "The access token was refused." : describe(error, "Couldn't reach it");
    savedError.set(target.kind === "local" ? `Couldn't open ${name}: ${message}` : `Couldn't switch to ${name}: ${message}`);
    return false;
  } finally {
    if (mine === switchSeq) {
      switching.set(null);
      verifying = null;
    }
  }
}

/**
 * Sign out: ends the session and forgets this server's token in memory, so
 * coming back asks for it. (Switching keeps it; Forget drops the server too.)
 */
export function signOut(): void {
  const url = connectedTo();
  if (url && !untracked(onLocalWorkspace)) dropToken(url);
  batch(() => {
    cancelSwitch();
    savedError.set(null);
  });
  disconnect();
}

/** Test and sign-out hygiene: every token in memory goes. */
export function forgetAllTokens(): void {
  tokens.clear();
  tokensVersion.set(untracked(tokensVersion) + 1);
  current = null;
}
