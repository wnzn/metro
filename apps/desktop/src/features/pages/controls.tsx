import { cleanup, signal } from "nsh";
import type { TurnModelChoice } from "../../api/client";
import type { Draft, PageDiagnostic } from "../../api/pages";
import { hasSpace, goPages, refreshPages, watchPageScope } from "../../pagesState";
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
function createPageModel(scopeId: () => string | null) {
  const catalog = createComposerCatalog();
  const open = signal(false);
  // Explicit intent lives outside the fetchable catalogue. A refresh, failed
  // request, or lost share can remove options but can't choose Default.
  const intent = signal<{ scope: string | null; choice: TurnModelChoice } | null>(null);
  const pin = () => intent()?.scope === scopeId() ? intent()?.choice : undefined;
  const current = () => catalog.current(scopeId());
  const selected = () => {
    const choice = pin();
    return choice && current()?.targets.find(t => t.choice.connectionId === choice.connectionId && (t.choice.modelId ?? t.modelId) === choice.modelId);
  };
  let alive = true;
  const load = async () => {
    const client = apiClient(), scope = scopeId();
    if (!client || !hasSpace(scope)) return;
    const guard = sessionGuard(scope ?? undefined);
    await catalog.load(scope, () => alive && guard() && scopeId() === scope && hasSpace(scope),
      () => client.availableCatalog(scope).then(result => result.models),
      () => client.providerCatalog().then(result => result.providers.map(({ id, name }) => ({ id, name }))));
  };
  watchPageScope(scopeId, (_, visible, switched) => {
    catalog.clear();
    if (switched) { intent.set(null); open.set(false); }
    if (visible && (open() || pin())) void load();
  });
  cleanup(() => { alive = false; catalog.clear(); });
  const error = () => {
    if (!hasSpace(scopeId())) return "You no longer have access to this Space. Choose a different scope.";
    const choice = pin(), target = selected();
    if (!choice) return "";
    if (current()?.phase === "loading") return "Checking your selected model…";
    if (!target || (choice.reasoningEffort && !target.levels.includes(choice.reasoningEffort)))
      return "The selected model or thinking level is unavailable. Open Model to choose again or explicitly select Default.";
    return "";
  };
  const pick = (key: string, level?: string) => {
    if (!key) { intent.set(null); return; }
    const target = current()?.targets.find(t => t.key === key);
    if (target && (!level || target.levels.includes(level))) {
      intent.set({ scope: scopeId(), choice: { ...target.choice, modelId: target.choice.modelId ?? target.modelId, reasoningEffort: level } });
    }
  };
  return { open, current, selected, pin, pick, error, load, alive: () => alive };
}

function PageModel(props: { model: ReturnType<typeof createPageModel>; disabled?: boolean }) {
  const { open, current, selected, pin, pick, load } = props.model;
  return <Column gap={2}>
    <Row><ComposerModelTrigger model={selected()?.model ?? (pin() ? "Check selection" : null)}
      thinking={pin()?.reasoningEffort ?? "Provider default"} mark={selected()?.provider.mark ?? null}
      accessibilityLabel="Model for building this page" onPress={() => {
        if (props.disabled) return;
        open.set(!open());
        if (open()) void load();
      }} /></Row>
    <If when={open()}>
      <If when={current()?.phase === "ready"} else={<Row gap={2} align="center">
        <Label variant="caption">{current()?.phase === "failed" ? "Could not load available models." : "Loading available models…"}</Label>
        <If when={current()?.phase === "failed"}><Action compact label="Try again" onPress={() => void load()} /></If>
        <If when={current()?.phase === "failed" && pin()}><Action compact label="Use Default model" disabled={props.disabled} onPress={() => pick("")} /></If>
      </Row>}>
        <ModelOptionPicker testID="page-model-options" value={selected()?.key ?? (pin() ? null : "")}
          options={[{ value: "", model: "Default model", title: "Default model", detail: "Your normal routing" },
            ...(current()?.targets ?? []).map(t => ({ value: t.key, model: t.model, modelId: t.modelId, provider: t.provider,
              detail: [t.connection, t.detail].filter(Boolean).join(" · "), title: t.label, thinking: t.levels.join(" · ") }))]}
          onChange={key => { if (!props.disabled) pick(key ?? ""); }} />
        <If when={selected()?.levels.length}>
          <ThinkingChoice testID="page-model-thinking" value={pin()?.reasoningEffort ?? ""}
            options={[{ value: "", label: "Provider default" }, ...(selected()?.levels ?? []).map(level => ({ value: level, label: level }))]}
            onChange={level => { const key = selected()?.key; if (key && !props.disabled) pick(key, level || undefined); }} />
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
  const checking = signal(false);
  const model = createPageModel(() => props.spaceId);
  const submit = async () => {
    const text = prompt().trim(), choice = model.pin(), sid = props.spaceId;
    if (props.busy || checking() || !text || model.error()) return;
    if (!choice) { props.onSubmit(text); return; }
    const grant = sessionGuard(sid ?? undefined);
    checking.set(true);
    try {
      // A share, device or model list can change while this sheet stays open.
      // Verify the explicit choice again; the server then checks at use time.
      await model.load();
      if (model.alive() && grant() && props.spaceId === sid && model.pin() === choice && !model.error()) props.onSubmit(text, choice);
    } finally { if (model.alive()) checking.set(false); }
  };
  return <Column gap={2}>
    <Field testID="page-prompt" accessibilityLabel={props.label ?? "Describe the page you want"} multiline value={prompt()}
      placeholder={props.label ?? "Describe the page you want"} onChange={prompt.set} maxLength={4000} disabled={props.busy || checking()} />
    <PageModel model={model} disabled={props.busy || checking()} />
    <If when={model.error()}>{message => <Label color="warning">{message()}</Label>}</If>
    <Row justify="end"><Action testID="page-generate" label="Generate draft" busyLabel="Starting…" variant="primary"
      disabled={!prompt().trim() || !!model.error()} busy={props.busy || checking()} onPress={() => void submit()} /></Row>
  </Column>;
}
