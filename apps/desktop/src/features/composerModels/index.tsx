import { cleanup, effect, signal, untracked } from "nsh";
import { activeSpaceId, apiClient, composerScope, mainPlace, me, onSessionEnd, openSettings, sessionGuard, settingsOpen } from "../../store";
import { Action, Column, Label, Modal } from "../../ui";
import { ComposerModelTrigger, ModelOptionPicker, ThinkingChoice } from "../../ui/composerModelTrigger";
import type { ModelOption } from "../../ui/composerModelTrigger";
import { createComposerCatalog } from "./catalog";
import type { ProviderName, Target } from "./catalog";

const catalog = createComposerCatalog();
const scope = () => mainPlace() === "chat" ? null : mainPlace() === "space" ? activeSpaceId() ?? undefined : undefined;
let registered = false;
export const knownNoSystemOne = () => catalog.knownNoSystemOne(scope());
export const choiceForTurn = () => catalog.choice(scope());

type Client = NonNullable<ReturnType<typeof apiClient>>;
/** The server's provider catalog names groups only; it changes with the server build, so one fetch per client. */
const providerNames = new WeakMap<Client, Promise<readonly ProviderName[]>>();
function namesFor(client: Client) {
  let p = providerNames.get(client);
  if (!p) {
    p = client.providerCatalog().then(c => c.providers.map(({ id, name }) => ({ id, name })));
    p.catch(() => providerNames.delete(client));
    providerNames.set(client, p);
  }
  return p;
}

function refresh() {
  const id = scope(), client = apiClient();
  if (id === undefined || !client) { catalog.clear(); return; }
  const guard = sessionGuard(id ?? undefined);
  void catalog.load(id, () => guard() && scope() === id && apiClient() === client, () => client.availableModels(id), () => namesFor(client));
}

const PROVIDER_DEFAULT = "Provider default";
const levelsText = (t: Target) => t.levels.join(" · ");
function option(t: Target, chosen: boolean, effort: string | undefined): ModelOption {
  return {
    value: t.key, model: t.model, provider: t.provider,
    detail: [t.connection, t.detail].filter(Boolean).join(" · "),
    title: [t.label, t.detail, t.levels.length ? `Thinking ${t.levels.join(" ")}` : ""].filter(Boolean).join(" — "),
    // The chosen row says the level that will be sent; others say what they offer.
    thinking: chosen && t.levels.length ? `Thinking: ${effort ?? PROVIDER_DEFAULT}` : t.levels.length ? `Thinking: ${levelsText(t)}` : undefined,
  };
}

export function ComposerModelControls() {
  if (!registered) { onSessionEnd(catalog.clear); registered = true; }
  const open = signal(false);
  const fetching = signal(false);
  const fetchError = signal("");
  let fetchGeneration = 0;
  const resetFetch = () => { fetchGeneration++; fetching.set(false); fetchError.set(""); };
  const close = () => { resetFetch(); open.set(false); };
  cleanup(close);
  const current = () => catalog.current(scope());
  const selected = () => catalog.selected(current());
  async function fetchModels() {
    const s = current(), t = catalog.selected(s), client = apiClient();
    if (!open() || !s || !t?.fetchFrom || !client || fetching()) return;
    const generation = ++fetchGeneration;
    const valid = () => generation === fetchGeneration && open() && current() === s && apiClient() === client;
    fetchError.set("");
    fetching.set(true);
    try {
      await client.listProviderModels(t.choice.connectionId);
      // Only the server's subsequent scoped catalog authorizes choices.
      if (valid()) refresh();
    } catch {
      if (valid()) fetchError.set(`Could not fetch models from ${t.fetchFrom}. Check that this connection supports listing models and is reachable.`);
    } finally {
      if (generation === fetchGeneration) fetching.set(false);
    }
  }
  const pick = (key: string | null, level?: string) => {
    resetFetch();
    const s = current();
    if (s) catalog.select(s, key, level);
  };
  const triggerLabel = () => {
    const pin = current()?.pin, t = selected();
    if (!pin) return "Model: Default";
    const thinking = `Thinking: ${pin.reasoningEffort ?? PROVIDER_DEFAULT}`;
    return `Model: ${t ? `${t.model} · ${t.connection} · ${t.provider.name}` : "Check selection"} · ${thinking}`;
  };
  const options = (): ModelOption[] => {
    const s = current(), chosen = selected()?.key, effort = s?.pin?.reasoningEffort;
    return [{ value: "", model: "Default", title: "Default — your normal routing", detail: "Your normal routing" },
      ...(s?.targets ?? []).map(t => option(t, t.key === chosen, effort))];
  };
  effect(() => {
    scope();
    composerScope();
    me();
    // Settings can pair a device, change its approved models, remove a share
    // or change the classifier while Chat is still mounted underneath it.
    if (settingsOpen()) { untracked(close); return; }
    untracked(() => { close(); refresh(); });
  });
  return (
    <Column>
      <ComposerModelTrigger model={selected()?.model ?? (current()?.pin ? "Check selection" : null)}
        thinking={current()?.pin ? current()?.pin?.reasoningEffort ?? PROVIDER_DEFAULT : null}
        mark={selected()?.provider.mark ?? null} accessibilityLabel={triggerLabel()}
        onPress={() => { refresh(); open.set(true); }} />
      <Modal size="compact" open={open()} title="Model for agent turns" onClose={close}>
        <Column pad={3} gap={2}>
          <Label variant="strong">Model for agent turns</Label>
          <Label variant="caption">Default uses your normal routing.</Label>
           <If when={current()?.phase !== "ready"}>
             <Label>{current()?.phase === "failed" ? "Could not load available models." : "Loading available models…"}</Label>
             <If when={current()?.phase === "failed"}>
               <Action compact label="Try again" testID="composer-retry-catalog" onPress={refresh} />
             </If>
           </If>
           <If when={current()?.phase === "ready"}>
             <If when={current()?.targets.length === 0}>
               <Label variant="caption">No chat models are ready here. Connect a provider or check its approved models.</Label>
               <Action compact label="Model providers" testID="composer-open-providers" onPress={() => { close(); openSettings("providers"); }} />
             </If>
             <If when={!!current()?.targets.length || !!current()?.pin}><ModelOptionPicker testID="composer-model-options"
              options={options()} value={selected()?.key ?? (current()?.pin ? null : "")}
              onChange={k => pick(k || null)} /></If>
            <If when={selected()?.detail}>{d => <Label variant="caption">{d()}</Label>}</If>
            <If when={selected()?.fetchFrom}>{name =>
              <Action compact variant="ghost" testID="composer-fetch-models" label={`Fetch models from ${name()}`}
                busy={fetching()} busyLabel="Fetching models…" onPress={() => void fetchModels()} />
            }</If>
            <If when={fetchError()}>{message => <Label color="warning">{message()}</Label>}</If>
            <If when={choiceForTurn().error}>{e => <Label color="warning">{e()}</Label>}</If>
            <If when={selected()?.levels.length}>
              <Label variant="strong">Thinking level</Label>
              <ThinkingChoice testID="composer-thinking-options"
                options={[{ value: "", label: PROVIDER_DEFAULT }, ...(selected()?.levels ?? []).map(l => ({ value: l, label: l }))]}
                value={current()?.pin?.reasoningEffort ?? ""}
                onChange={l => { const t = selected(); if (t) pick(t.key, l || undefined); }} />
            </If>
          </If>
          <Action compact label="Done" onPress={close} />
        </Column>
      </Modal>
    </Column>
  );
}
