// "Add a model to start chatting": the one card someone sees while nothing
// can answer them, that is, they have no chat connection of their own, none
// is shared with them, and the server runs no operator model for them
// (GET /v1/models/available `operator_model`). It offers the main ways in,
// from what this server's catalog actually offers. When the operator model
// exists, nothing shows: the picker lists it and Chat works.
//
// It loads what it needs itself, so it can be mounted anywhere (Model
// providers, the empty Chat). It renders nothing until it knows a model is
// missing, and never on a failed load.

import { effect, signal, untracked } from "nsh";
import { catalog, connections, loadProviders, providerById } from "../settings";
import type { CatalogProvider } from "../settings";
import { readStartupModels } from "../startupModelRequests";
import { apiClient, me, onSessionEnd, openSettings, sessionGuard, settingsOpen } from "../store";
import { Column, Label, SettingsGroup } from "../ui";
import { MethodRow } from "../ui/providers";
import { packaged } from "../device/state";
import { CAPABILITY_TONE, methodDetail, vendorOf, wayLabel } from "./providerGroups";
import { startAdding } from "./ProvidersSettings";

/** Whether the signed-in user can chat with any model, as last loaded; null while unknown. */
const readiness = signal<{ hasChat: boolean; operator: boolean } | null>(null);
let seq = 0;
onSessionEnd(() => {
  seq++;
  readiness.set(null);
});

/** Loads whether any chat model can answer me (mine, shared with me, or the operator's). */
async function loadChatReadiness(fresh: boolean): Promise<boolean> {
  const c = apiClient();
  if (!c) return false;
  const n = ++seq;
  const ok = sessionGuard();
  try {
    const answer = await readStartupModels(c, null, fresh);
    if (!ok() || n !== seq) return false;
    const hasChat = answer.models.some((m) => m.capability === "chat" && (m.owned || m.ready !== false));
    readiness.set({ hasChat, operator: !!answer.operatorModel });
    return true;
  } catch {
    // Unknown: show nothing rather than a wrong prompt.
    return false;
  }
}

/** True only when it's known that nothing can answer: no chat model and no operator model. */
export const needsChatModel = () => {
  const r = readiness();
  const own = connections();
  // A chat connection of my own, just added, counts at once.
  if (own?.some((c) => (c.capability ?? providerById(c.providerId)?.capability ?? "chat") === "chat")) return false;
  return !!r && !r.hasChat && !r.operator;
};

type Choice = { id: string; title: string; detail: string };

/** The main ways in this server offers: Sign in with ChatGPT, your own endpoint, an API key. */
function choices(providers: readonly CatalogProvider[]): Choice[] {
  const chat = providers.filter((p) => p.capability === "chat");
  const out: Choice[] = [];
  const opts = { packaged: packaged() };
  const oauth = chat.find((p) => p.id === "device-openai-oauth");
  if (oauth) out.push({ id: oauth.id, title: "Sign in with ChatGPT", detail: methodDetail(oauth, opts) });
  const endpoint = chat.find((p) => p.id === "device-openai-compatible") ?? chat.find((p) => p.allowCustomUrl);
  if (endpoint) out.push({ id: endpoint.id, title: "Your own endpoint", detail: methodDetail(endpoint, opts) });
  const keyed = [...new Set(chat.filter((p) => p.execution === "server" && p.requiresKey).map((p) => vendorOf(p).name))];
  if (keyed.length) {
    const names = keyed.length > 3 ? `${keyed.slice(0, 3).join(", ")} and more` : keyed.join(", ");
    out.push({ id: "chat", title: "An API key", detail: `${names}. Stored encrypted on the server, never shown again.` });
  }
  // Anything else this server offers for chat, if none of the main ways is there.
  if (out.length === 0) for (const p of chat.slice(0, 3)) out.push({ id: p.id, title: wayLabel(p, opts), detail: methodDetail(p, opts) });
  return out;
}

/**
 * The onboarding card. Mount it where an empty Chat or Model providers
 * renders; it shows only while `needsChatModel()`. Choosing a way opens
 * Settings › Model providers on that way's form.
 */
export function ModelOnboarding(props: { testID?: string }) {
  effect(() => {
    me();
    const editing = settingsOpen();
    let disposed = false;
    untracked(() => void loadChatReadiness(editing).then(loaded => {
      if (!disposed && loaded && needsChatModel() && !catalog()) void loadProviders();
    }));
    return () => { disposed = true; };
  });
  const list = () => choices(catalog()?.providers ?? []);
  const choose = (id: string) => {
    startAdding(id);
    openSettings("providers");
  };
  return (
    <If when={needsChatModel()}>
      <SettingsGroup testID={props.testID ?? "model-onboarding"} title="Add a model to start chatting" description="Nothing can answer you yet. Connect a model you already use; it stays yours until you share it.">
        <If when={list().length > 0} else={<Column padX={4} padY={3}><Label variant="caption" color="muted">{catalog() ? "This server offers no chat providers. Ask its operator." : "Loading the ways to connect…"}</Label></Column>}>
          <Column padX={2} padY={2}>
            <Each of={list()} key={(c) => c.id}>
              {(c) => (
                <MethodRow
                  testID={`${props.testID && props.testID !== "model-onboarding" ? props.testID : "onboarding"}:${c().id}`}
                  label={c().title}
                  detail={c().detail}
                  capability={{ label: "Chat", tone: CAPABILITY_TONE.chat }}
                  accessibilityLabel={`${c().title}: ${c().detail}`}
                  onPress={() => choose(c().id)}
                />
              )}
            </Each>
          </Column>
        </If>
      </SettingsGroup>
    </If>
  );
}
