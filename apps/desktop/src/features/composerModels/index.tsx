import { effect, signal, untracked } from "nsh";
import { activeSpaceId, apiClient, mainPlace, me, onSessionEnd, sessionGuard } from "../../store";
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
  const current = () => catalog.current(scope());
  const selected = () => catalog.selected(current());
  const pick = (key: string | null, level?: string) => {
    const s = current();
    if (s) catalog.select(s, key, level);
  };
  effect(() => {
    scope();
    me();
    untracked(() => { open.set(false); refresh(); });
  });
  return (
    <Column>
      <ComposerModelTrigger label={`Model: ${selected()?.label ?? (current()?.pin ? "Check selection" : "Default")}${current()?.pin?.reasoningEffort ? ` · ${current()?.pin?.reasoningEffort}` : ""}`}
        onPress={() => { refresh(); open.set(true); }} />
      <Modal size="compact" open={open()} title="Model for agent turns" onClose={() => open.set(false)}>
        <Column pad={3} gap={2}>
          <Label variant="strong">Model for agent turns</Label>
          <Label variant="caption">Default uses your normal routing.</Label>
          <If when={current()?.phase !== "ready"}>
            <Label>{current()?.phase === "failed" ? "Could not load available models. Reopen to retry." : "Loading available models…"}</Label>
          </If>
          <If when={current()?.phase === "ready"}>
            <Picker testID="composer-model-options" accessibilityLabel="Model" searchable
              items={[{ value: "", label: "Default" }, ...(current()?.targets ?? []).map(t => ({ value: t.key, label: t.label, detail: t.detail }))]}
              placeholder="Choose a model" value={selected()?.key ?? (current()?.pin ? null : "")}
              onChange={k => { if (k !== null) pick(k || null); }} />
            <If when={selected()?.detail}>{d => <Label variant="caption">{d()}</Label>}</If>
            <If when={choiceForTurn().error}>{e => <Label color="warning">{e()}</Label>}</If>
            <If when={selected()?.levels.length}>
              <Label variant="strong">Thinking level</Label>
              <Picker testID="composer-thinking-options" accessibilityLabel="Thinking level"
                items={[{ value: "", label: "Provider default" }, ...(selected()?.levels ?? []).map(l => ({ value: l, label: l }))]}
                value={current()?.pin?.reasoningEffort ?? ""} placeholder="Choose a level"
                onChange={l => { const t = selected(); if (t && l !== null) pick(t.key, l || undefined); }} />
            </If>
          </If>
          <Action compact label="Done" onPress={() => open.set(false)} />
        </Column>
      </Modal>
    </Column>
  );
}
