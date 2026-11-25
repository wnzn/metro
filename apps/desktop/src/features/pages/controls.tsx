import { cleanup, effect, signal, untracked } from "nsh";
import type { TurnModelChoice } from "../../api/client";
import type { Draft, PageDiagnostic } from "../../api/pages";
import { hasSpace, goPages, refreshPages } from "../../pagesState";
import { apiClient, sessionGuard, spaces } from "../../store";
import { Action, Banner, Column, Field, Label, Row } from "../../ui";
import { ComposerModelTrigger, ModelOptionPicker, ThinkingChoice } from "../../ui/composerModelTrigger";
import { createComposerCatalog } from "../composerModels/catalog";

export function scopeName(spaceId?: string | null) {
  return spaceId ? spaces().find(s => s.id === spaceId)?.name ?? "Space" : "Personal";
}

export function showPage(page: { id: string; space_id: string | null }) {
  goPages({ kind: "page", id: page.id, spaceId: page.space_id ?? undefined });
  void refreshPages();
}

export function showDraft(draft: Draft) {
  goPages({ kind: "draft", id: draft.id, spaceId: draft.space_id ?? undefined, returnPage: draft.page_id ?? undefined });
}

export function PagesError(props: { message: string; onRetry?: () => void; testID?: string }) {
  return <If when={props.message}>
    <Banner testID={props.testID ?? "pages-error"} variant="inline" tone="danger" message={props.message}
      action={props.onRetry ? <Action compact label="Try again" onPress={() => props.onRetry?.()} /> : undefined} />
  </If>;
}

export function Diagnostics(props: { items: PageDiagnostic[] }) {
  return <If when={props.items.length}>
    <Column gap={1} testID="page-diagnostics">
      <Label variant="strong">Source diagnostics</Label>
      <Each of={props.items.slice(0, 100)}>{item =>
        <Label color="warning" variant="caption">{`Line ${item().line}, column ${item().column}${item().key ? ` · ${item().key}` : ""}: ${item().message}`}</Label>
      }</Each>
    </Column>
  </If>;
}

/** Same model options, provider grouping and thinking controls as the chat
 * composer, but with a separate per-form catalog and an explicit page scope. */
function PageModel(props: { spaceId: string | null; choice: (result: { choice?: TurnModelChoice; error?: string }) => void }) {
  const catalog = createComposerCatalog();
  const open = signal(false);
  const current = () => catalog.current(props.spaceId);
  const selected = () => catalog.selected(current());
  const load = () => {
    const client = apiClient(), scope = props.spaceId;
    if (!client || !hasSpace(scope)) return;
    const guard = sessionGuard(scope ?? undefined);
    void catalog.load(scope, () => guard() && props.spaceId === scope && hasSpace(scope),
      () => client.availableCatalog(scope).then(result => result.models),
      () => client.providerCatalog().then(result => result.providers.map(({ id, name }) => ({ id, name }))));
  };
  effect(() => {
    const id = props.spaceId;
    const visible = hasSpace(id);
    untracked(() => { catalog.clear(); open.set(false); if (visible) props.choice({}); });
  });
  effect(() => { const choice = catalog.choice(props.spaceId); untracked(() => props.choice(choice)); });
  cleanup(catalog.clear);
  return <Column gap={2}>
    <Row><ComposerModelTrigger model={selected()?.model ?? (current()?.pin ? "Check selection" : null)}
      thinking={current()?.pin?.reasoningEffort ?? "Provider default"} mark={selected()?.provider.mark ?? null}
      accessibilityLabel="Model for building this page" onPress={() => {
        open.set(!open());
        if (open()) load();
      }} /></Row>
    <If when={open()}>
      <If when={current()?.phase === "ready"} else={<Row gap={2} align="center">
        <Label variant="caption">{current()?.phase === "failed" ? "Could not load available models." : "Loading available models…"}</Label>
        <If when={current()?.phase === "failed"}><Action compact label="Try again" onPress={load} /></If>
      </Row>}>
        <ModelOptionPicker testID="page-model-options" value={selected()?.key ?? (current()?.pin ? null : "")}
          options={[{ value: "", model: "Default model", title: "Default model", detail: "Your normal routing" },
            ...(current()?.targets ?? []).map(t => ({ value: t.key, model: t.model, modelId: t.modelId, provider: t.provider,
              detail: [t.connection, t.detail].filter(Boolean).join(" · "), title: t.label, thinking: t.levels.join(" · ") }))]}
          onChange={key => { const s = current(); if (s) catalog.select(s, key || null); }} />
        <If when={selected()?.levels.length}>
          <ThinkingChoice testID="page-model-thinking" value={current()?.pin?.reasoningEffort ?? ""}
            options={[{ value: "", label: "Provider default" }, ...(selected()?.levels ?? []).map(level => ({ value: level, label: level }))]}
            onChange={level => { const s = current(), key = selected()?.key; if (s && key) catalog.select(s, key, level || undefined); }} />
        </If>
      </If>
    </If>
  </Column>;
}

export function DraftPrompt(props: {
  spaceId: string | null; initial?: string; label?: string; busy?: boolean;
  onSubmit: (prompt: string, choice?: TurnModelChoice) => void;
}) {
  const prompt = signal(props.initial ?? "");
  const model = signal<{ choice?: TurnModelChoice; error?: string }>({});
  const submit = () => {
    if (!props.busy && prompt().trim() && !model().error && hasSpace(props.spaceId)) props.onSubmit(prompt().trim(), model().choice);
  };
  return <Column gap={2}>
    <Field testID="page-prompt" accessibilityLabel={props.label ?? "Describe the page you want"} multiline value={prompt()}
      placeholder={props.label ?? "Describe the page you want"} onChange={prompt.set} maxLength={4000} disabled={props.busy} />
    <PageModel spaceId={props.spaceId} choice={model.set} />
    <If when={model().error}>{message => <Label color="warning">{message()}</Label>}</If>
    <Row justify="end"><Action testID="page-generate" label="Generate draft" busyLabel="Starting…" variant="primary"
      disabled={!prompt().trim() || !!model().error || !hasSpace(props.spaceId)} busy={props.busy} onPress={submit} /></Row>
  </Column>;
}
