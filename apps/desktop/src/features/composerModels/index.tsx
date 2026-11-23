import { cleanup, effect, signal, untracked } from "nsh";
import { activeSpaceId, apiClient, composerScope, mainPlace, me, onSessionEnd, openSettings, sessionGuard, settingsOpen } from "../../store";
import { Action, Column, Label, Modal, Picker } from "../../ui";
import { ComposerModelTrigger } from "../../ui/composerModelTrigger";
import { createComposerCatalog } from "./catalog";

const catalog = createComposerCatalog();
const scope = () => mainPlace() === "chat" ? null : mainPlace() === "space" ? activeSpaceId() ?? undefined : undefined;
let registered = false;
export const knownNoSystemOne = () => catalog.knownNoSystemOne(scope());
export const choiceForTurn = () => catalog.choice(scope());

function refresh() {
  const id = scope(), client = apiClient();
  if (id === undefined || !client) { catalog.clear(); return; }
  const guard = sessionGuard(id ?? undefined);
  void catalog.load(id, () => guard() && scope() === id && apiClient() === client, () => client.availableModels(id));
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
      <ComposerModelTrigger label={`Model: ${selected()?.label ?? (current()?.pin ? "Check selection" : "Default")}${current()?.pin?.reasoningEffort ? ` · ${current()?.pin?.reasoningEffort}` : ""}`}
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
             <If when={!!current()?.targets.length || !!current()?.pin}><Picker testID="composer-model-options" accessibilityLabel="Model" searchable
              items={[{ value: "", label: "Default" }, ...(current()?.targets ?? []).map(t => ({ value: t.key, label: t.label, detail: t.detail }))]}
              placeholder="Choose a model" value={selected()?.key ?? (current()?.pin ? null : "")}
               onChange={k => { if (k !== null) pick(k || null); }} /></If>
            <If when={selected()?.detail}>{d => <Label variant="caption">{d()}</Label>}</If>
            <If when={selected()?.fetchFrom}>{name =>
              <Action compact variant="ghost" testID="composer-fetch-models" label={`Fetch models from ${name()}`}
                busy={fetching()} busyLabel="Fetching models…" onPress={() => void fetchModels()} />
            }</If>
            <If when={fetchError()}>{message => <Label color="warning">{message()}</Label>}</If>
            <If when={choiceForTurn().error}>{e => <Label color="warning">{e()}</Label>}</If>
            <If when={selected()?.levels.length}>
              <Label variant="strong">Thinking level</Label>
              <Picker testID="composer-thinking-options" accessibilityLabel="Thinking level"
                items={[{ value: "", label: "Provider default" }, ...(selected()?.levels ?? []).map(l => ({ value: l, label: l }))]}
                value={current()?.pin?.reasoningEffort ?? ""} placeholder="Choose a level"
                onChange={l => { const t = selected(); if (t && l !== null) pick(t.key, l || undefined); }} />
            </If>
          </If>
          <Action compact label="Done" onPress={close} />
        </Column>
      </Modal>
    </Column>
  );
}
