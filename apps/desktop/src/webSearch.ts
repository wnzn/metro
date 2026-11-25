// Page-local state. Only the bridge receives keys; neither this state nor
// the server API ever holds one. Nothing is saved to desktop UI preferences.
import { announce, cleanup, computed, effect, signal, untracked } from "nsh";
import { ApiError } from "./api/client";
import { WEB_OUTCOMES, WEB_PROVIDERS } from "./api/webSearch";
import type { WebProvider } from "./api/webSearch";
import { BridgeError } from "./device/bridge";
import type { Bridge } from "./device/bridge";
import { helper, refreshHelper, withWebSearchBridge } from "./device/state";
import type { LocalWebSearch, WebProviderRow, WebUpdate } from "./device/webSearch";
import { apiClient, connectedTo, me, sessionGuard } from "./store";

// Settings-pages integration owns the route. Activity does not guess one.
export const webSearchSettingsAction = signal<{ open: () => void } | null>(null);
export function registerWebSearchSettingsAction(open: () => void) {
  const action = { open };
  webSearchSettingsAction.set(action);
  return () => { if (webSearchSettingsAction() === action) webSearchSettingsAction.set(null); };
}

type LocalPhase = "loading" | "unavailable" | "secondary" | "unbound" | "offline" | "unsupported" | "error" | "ready";
type Chat = { phase: "loading" | "unsupported" | "error" } | { phase: "ready"; enabled: boolean };
type Message = { tone: "danger" | "success" | "warning"; text: string };

function localProblem(error: unknown, fallback: string): string {
  if (error instanceof BridgeError) {
    if (error.code === "not_sent") return "The test was refused or interrupted by this computer. Check the web search and key settings before trying again.";
    if (error.code === "no_session" || error.code === "session_rejected") return "Metro's helper isn't signed in to this account. Sign in again.";
    if (error.code === "other_instance") return "Use the first Metro window to change web search.";
    if (error.code === "unsupported") return "This Metro server doesn't support web search yet.";
    if (error.code === "unreachable") return "Metro's helper on this computer didn't answer. Refresh to check the current settings.";
    if (error.status === 401) return "The helper didn't accept this window. Reopen the Metro desktop app.";
  }
  return fallback;
}

export function providerSettings(rows: readonly WebProviderRow[], id: WebProvider, change: boolean | -1 | 1): WebUpdate["providers"] {
  const list = rows.map(({ id, enabled }) => ({ id, enabled }));
  const i = list.findIndex((r) => r.id === id);
  if (i < 0) return undefined;
  if (typeof change === "boolean") {
    list[i]!.enabled = change;
  } else {
    const j = i + change;
    if (!list[j]) return undefined;
    [list[i], list[j]] = [list[j]!, list[i]!];
  }
  return list;
}

export function createWebSearchSettings(server: string, account: string) {
  const sameSession = sessionGuard();
  const client = apiClient();
  let mounted = true;
  cleanup(() => { mounted = false; });
  const current = () => mounted && sameSession() && connectedTo() === server && me() === account;
  const binding = computed(() => {
    const h = helper();
    switch (h.phase) {
      case "absent": return "unavailable";
      case "checking": return "loading";
      case "secondary": return "secondary";
      case "unbound": return "unbound";
      case "unreachable": return "offline";
      case "ready": return h.status.session?.accountId === account &&
        h.status.session.serverUrl.replace(/\/+$/, "") === server.replace(/\/+$/, "") ? "ready" : "unbound";
    }
  });
  const local = signal<LocalWebSearch | null>(null);
  const loaded = signal<LocalPhase>("loading");
  const phase = computed<LocalPhase>(() => binding() === "ready" ? loaded() : binding());
  const localBusy = signal(false);
  const busy = computed(() => localBusy() || phase() === "loading");
  const localMessage = signal<Message | null>(null);
  const chat = signal<Chat>({ phase: "loading" });
  const chatBusy = signal(false);
  const chatError = signal<string | null>(null);

  const say = (text: string, tone: Message["tone"] = "success") => {
    localMessage.set({ text, tone });
    announce(text);
  };
  const readLocal = async () => {
    try {
      const data = await withWebSearchBridge(server, account, (b) => {
        if (!current()) throw new BridgeError("no_session", "The web search settings page was closed.");
        return b.webSearch();
      });
      if (!current() || binding() !== "ready") return false;
      local.set(data);
      loaded.set("ready");
      return true;
    } catch (error) {
      if (!current() || binding() !== "ready") return false;
      local.set(null);
      loaded.set(error instanceof BridgeError && error.status === 404 ? "unsupported" :
        error instanceof BridgeError && error.code === "unreachable" ? "offline" :
        error instanceof BridgeError && error.code === "no_session" ? "unbound" : "error");
      return false;
    }
  };

  effect(() => {
    const ready = binding() === "ready";
    untracked(() => {
      local.set(null);
      localMessage.set(null);
      loaded.set("loading");
      if (ready && current()) void readLocal();
    });
  });

  // The bridge starts workers asynchronously. Observe startup only while the
  // page is open; a manual refresh handles later changes and cooldowns.
  const starting = computed(() => local()?.worker === "starting" && phase() === "ready" && !busy());
  effect(() => {
    if (!starting()) return;
    let stopped = false;
    let timer: number | undefined;
    const tick = async (left: number) => {
      if (stopped || !current() || localBusy()) return;
      if (await readLocal() && !stopped && left > 1 && local()?.worker === "starting") {
        timer = setTimeout(() => void tick(left - 1), 1500);
      }
    };
    timer = setTimeout(() => void tick(10), 1500);
    cleanup(() => { stopped = true; if (timer !== undefined) clearTimeout(timer); });
  });

  async function changeLocal(
    job: (b: Bridge) => Promise<LocalWebSearch | WebProviderRow>,
    success: (answer: LocalWebSearch | WebProviderRow) => Message,
    fallback: string,
  ) {
    if (!current() || phase() !== "ready" || busy()) return;
    localBusy.set(true);
    localMessage.set(null);
    try {
      const answer = await withWebSearchBridge(server, account, (b) => {
        if (!current()) throw new BridgeError("no_session", "The web search settings page was closed.");
        return job(b);
      });
      if (!current() || binding() !== "ready") return;
      if ("providers" in answer) local.set(answer);
      else {
        // Cooldown clearing can restart the worker or repair the whole state
        // file. Read the full view instead of pretending only one row changed.
        if (!await readLocal() || !current()) return;
      }
      const msg = success(answer);
      say(msg.text, msg.tone);
    } catch (error) {
      if (!current() || binding() !== "ready") return;
      // Some writes can succeed before the worker fails to start. Read back;
      // never retry the write, send a key again, or restore optimistic values.
      await readLocal();
      if (current() && binding() === "ready") say(localProblem(error, fallback), "danger");
    } finally {
      if (current()) localBusy.set(false);
    }
  }

  async function refresh() {
    if (!current() || busy()) return;
    localMessage.set(null);
    if (binding() === "ready") {
      localBusy.set(true);
      try { await readLocal(); } finally { if (current()) localBusy.set(false); }
    } else {
      await refreshHelper(); // this does not bind or move to another server
    }
  }

  async function readChat() {
    if (!client || !current() || chatBusy()) return;
    chat.set({ phase: "loading" });
    chatError.set(null);
    try {
      const enabled = await client.webSearchChatPref();
      if (current()) chat.set(enabled === null ? { phase: "error" } : { phase: "ready", enabled });
    } catch (error) {
      if (current()) chat.set({ phase: error instanceof ApiError && error.status === 404 ? "unsupported" : "error" });
    }
  }
  void readChat();

  async function setChat(enabled: boolean) {
    if (!client || !current() || chatBusy() || chat().phase !== "ready") return;
    chatBusy.set(true);
    chatError.set(null);
    try {
      const confirmed = await client.setWebSearchChatPref(enabled);
      if (!current()) return;
      if (confirmed !== enabled) throw new Error("unconfirmed");
      chat.set({ phase: "ready", enabled: confirmed });
      announce(`Web search in personal Chat is ${confirmed ? "on" : "off"}.`);
    } catch {
      if (current()) {
        const message = "Metro couldn't confirm the personal Chat change. Reload the setting to check what was saved.";
        chat.set({ phase: "error" });
        chatError.set(message);
        announce(message);
      }
    } finally {
      if (current()) chatBusy.set(false);
    }
  }

  const ok = (text: string): Message => ({ tone: "success", text });
  return {
    phase, local, busy, localMessage, chat, chatBusy, chatError, refresh, readChat, setChat,
    update: (data: WebUpdate, text: string) => changeLocal((b) => b.updateWebSearch(data), () => ok(text), "Metro couldn't confirm the web search change. Check the current settings before trying again."),
    saveKey: (id: WebProvider, key: string) => changeLocal((b) => b.setWebSearchKey(id, key),
      () => ok(`Saved your ${WEB_PROVIDERS[id]} key on this computer.`), `Couldn't save the ${WEB_PROVIDERS[id]} key. Check it and enter it again.`),
    removeKey: (id: WebProvider) => changeLocal((b) => b.removeWebSearchKey(id),
      () => ok(`Removed your ${WEB_PROVIDERS[id]} key.`), `Couldn't remove the ${WEB_PROVIDERS[id]} key.`),
    resetKeys: () => changeLocal((b) => b.resetWebSearchKeys(), () => ok("Saved web search keys removed."), "Couldn't reset the saved keys."),
    clear: (id: WebProvider) => changeLocal((b) => b.clearWebSearchCooldown(id),
      () => ok(`${WEB_PROVIDERS[id]} is no longer paused or marked as having a rejected key.`), "Couldn't clear the saved provider pause."),
    test: (id: WebProvider) => changeLocal((b) => b.testWebSearchProvider(id), (row) => {
      const result = "test" in row ? row.test : null;
      return { tone: result === "ok" ? "success" : "warning", text: `${WEB_PROVIDERS[id]} test: ${result ? WEB_OUTCOMES[result] : "no result"}.` };
    }, `Couldn't test ${WEB_PROVIDERS[id]}.`),
  };
}

export type WebSearchSettingsModel = ReturnType<typeof createWebSearchSettings>;
