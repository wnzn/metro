import { announce, cleanup, computed, keymap, openUrl, signal } from "nsh";
import type { ViewRef } from "nsh";
import { WEB_OUTCOMES, WEB_PROVIDERS } from "../api/webSearch";
import type { WebProvider } from "../api/webSearch";
import type { LocalWebSearch, WebProviderRow } from "../device/webSearch";
import { connectedTo, me } from "../store";
import {
  Action, Badge, Banner, Column, compactWidth, ConfirmInline, ConnectionItem, Field,
  Label, LearnMore, MoreMenu, PageHeader, PageLayout, ProblemNote, ReorderButtons,
  Row, ScopeLine, SettingRow, SettingsGroup, Toggle,
} from "../ui";
import { createWebSearchSettings, providerSettings } from "../webSearch";
import type { WebSearchSettingsModel } from "../webSearch";
import { webActivityDisclosure, webDeviceSelection, webModelDisclosure, webProviderDisclosure } from "./webSearchPrivacy";

const TERMS: Record<WebProvider, string> = {
  exa: "https://exa.ai/terms", parallel: "https://parallel.ai/terms-of-service",
  tavily: "https://www.tavily.com/terms", firecrawl: "https://www.firecrawl.dev/terms-of-service",
  tinyfish: "https://www.tinyfish.ai/terms-of-service",
};

type Props = { onOpenAbout?: () => void; onOpenPrivacy?: () => void };

/** Register as the `websearch` Settings page. Unmount when it isn't shown. */
export function WebSearchSettings(props: Props) {
  const targets = computed(() => {
    const server = connectedTo();
    const account = me();
    return server && account ? [{ server, account }] : [];
  });
  return (
    <PageLayout testID="websearch-settings" pad={compactWidth() ? 4 : 6} gap={6}>
      <PageHeader title="Web search" scope={{ kind: "computer" }}
        subtitle="This switch and these provider settings apply only to this computer." />
      <If when={targets().length} else={<Unavailable text="Sign in to Metro to set up web search." />}>
        <Each of={targets()} key={(t) => JSON.stringify([t.server, t.account])}>
          {(t) => <WebSearchAccount server={t().server} account={t().account}
            onOpenAbout={props.onOpenAbout} onOpenPrivacy={props.onOpenPrivacy} />}
        </Each>
      </If>
    </PageLayout>
  );
}

function WebSearchAccount(props: Props & { server: string; account: string }) {
  const model = createWebSearchSettings(props.server, props.account);
  const local = () => model.phase() === "ready" ? model.local() : null;
  const unavailable = () => ({
    unavailable: "Open the Metro desktop app to manage web search on this computer.",
    secondary: "Another Metro window serves web search on this computer. Open Web search in that window.",
    unbound: "Metro's helper isn't serving this account on this server. Reopen Metro or sign out and sign in again to reconnect it.",
    offline: model.canRefresh() ? "Metro's helper on this computer isn't answering. Refresh or reopen the desktop app."
      : "Metro's helper on this computer isn't answering. Reopen the desktop app or sign out and sign in again to reconnect it.",
    error: "Metro's helper didn't return usable web search settings. Refresh to try again.",
    unsupported: "", ready: "", loading: "",
  })[model.phase()];
  return (
    <>
      <SettingsGroup title="How it works">
        <SettingRow label="This computer's status">
          <If when={local()} else={
            <Badge label={model.phase() === "loading" ? "Checking" : "Unavailable"} tone={model.phase() === "loading" ? "neutral" : "warning"} />
          }>
            {(state) => <WorkerBadge state={state()} />}
          </If>
        </SettingRow>
        <SettingRow label="Which device is used" description={webDeviceSelection} />
        <SettingRow label="If none of your web devices is online"
          description="The tool can't run. Open Metro with web search enabled on one of your devices. Scheduled prompts can still answer without web data if their model is available." />
        <LearnMore testID="websearch-learn">
          <Label variant="caption">Space owners allow search and page reads separately for each agent. Metro only fetches URLs from a person's message available to the run or from a search result in that run. It doesn't check where DNS or provider redirects lead.</Label>
        </LearnMore>
      </SettingsGroup>
      <If when={model.phase() === "loading"}><Label testID="websearch-loading" variant="caption">Loading web search…</Label></If>
      <If when={unavailable()}>{(text) => <Unavailable text={text()} />}</If>
      <If when={model.phase() === "unsupported"}>
        <ProblemNote text="Update Metro to use web search." testID="websearch-unsupported"
          action={props.onOpenAbout ? <Action label="Open About & updates" onPress={props.onOpenAbout} /> : undefined} />
      </If>
      <If when={model.canRefresh() && ["ready", "offline", "error"].includes(model.phase())}>
        <Row><Action label="Refresh web search" testID="websearch-refresh" disabled={model.busy()} onPress={() => void model.refresh()} /></Row>
      </If>
      <If when={local()}>{(state) => <Computer model={model} state={state()} />}</If>
      <PersonalChat model={model} server={props.server} />
      <SettingsGroup title="Privacy">
        <SettingRow label="What providers see" description={webProviderDisclosure} />
        <SettingRow label="What Metro relays and keeps" description={webActivityDisclosure} />
        <SettingRow label="What the model sees" description={webModelDisclosure} />
        <SettingRow label="Links" description="Don't give agents private links or URLs that contain credentials." />
        <If when={props.onOpenPrivacy}>
          <Row><Action variant="ghost" label="Open About & updates › Privacy" testID="websearch-privacy" onPress={() => props.onOpenPrivacy?.()} /></Row>
        </If>
      </SettingsGroup>
      <If when={model.localMessage()}>
        {(message) => <Banner variant="inline" tone={message().tone} message={message().text} testID="websearch-status" />}
      </If>
    </>
  );
}

function Unavailable(props: { text: string }) {
  return <ProblemNote text={props.text} tone="warning" testID="websearch-unavailable" />;
}

function WorkerBadge(props: { state: LocalWebSearch }) {
  const label = () => {
    const s = props.state;
    if (s.serverUnsupported) return "Unsupported server";
    if (s.settingsError || s.keysError || s.endpointsError) return "Unavailable";
    if (!s.enabled) return "Off";
    if (s.worker === "starting") return "Starting";
    if (s.worker === "online" && s.paired) return "Ready";
    if (s.worker === "offline") return "Offline";
    return "Unavailable";
  };
  return <Badge testID="websearch-worker" label={label()} tone={label() === "Ready" ? "success" : label() === "Off" || label() === "Starting" ? "neutral" : "warning"} />;
}

function Computer(props: { model: WebSearchSettingsModel; state: LocalWebSearch }) {
  const m = props.model;
  const s = () => props.state;
  const edit = signal<WebProvider | null>(null);
  const removal = signal<WebProvider | null>(null);
  const blocked = () => m.busy() || !!s().settingsError || s().endpointsError;
  const dismiss = () => { edit.set(null); removal.set(null); };
  return (
    <>
      <SettingsGroup title="This computer">
        <SettingRow label="Enable web search on this computer"
          description="Makes this computer available to your permitted agents. It doesn't turn on web search in personal Chat.">
          <Toggle testID="websearch-master" label="Enable web search on this computer"
            checked={s().enabled} disabled={m.busy() || !!s().settingsError ||
              (!s().enabled && (s().endpointsError || s().serverUnsupported))}
            onChange={(enabled) => { dismiss(); void m.update({ enabled }, `Web search is ${enabled ? "on" : "off"} on this computer.`); }} />
        </SettingRow>
        <If when={!s().noticeAcknowledged}>
          <ProblemNote tone="system" testID="websearch-notice"
            text="Web search is on by default on this computer. If Metro selects it, third-party providers it contacts may receive your queries or page URLs. Turn it off here at any time."
            action={<Action label="Got it" testID="websearch-ack" disabled={blocked()}
              onPress={() => void m.update({ notice_acknowledged: true }, "Notice acknowledged.")} />} />
        </If>
        <If when={s().serverUnsupported}><Unavailable text="This Metro server doesn't offer web search yet. Update the server to enable it." /></If>
        <If when={s().enabled && !s().settingsError && !s().serverUnsupported && (s().worker === "offline" || s().worker === "error" || s().worker === "unauthorized" || s().worker === "stopped")}>
          <Unavailable text={s().worker === "unauthorized"
            ? "Metro no longer accepts this computer for web search. Turn web search off and back on to reconnect it."
            : "This computer isn't serving web searches. Check the connection and refresh. If it still isn't ready, turn web search off and back on."} />
        </If>
        <If when={s().offNotReported}><Unavailable text="Web search is off here, but the server couldn't be notified. It will see this computer as offline." /></If>
        <If when={s().endpointsError}><Unavailable text="This computer's provider endpoints couldn't be opened. Reopen Metro after correcting the setup." /></If>
        <If when={s().settingsError === "preferences"}>
          <ProblemNote text="Your saved web search settings can't be read. Web search is off. Resetting restores the default provider order while leaving web search off."
            testID="websearch-settings-error"
            action={<Action label="Turn off and reset settings" testID="websearch-reset-settings"
              disabled={m.busy()} onPress={() => { dismiss(); void m.update({ enabled: false }, "Settings reset. Web search is off on this computer."); }} />} />
        </If>
        <If when={s().settingsError === "pauses"}>
          <ProblemNote text="Saved provider pauses can't be read, so web search is paused. Turn it off without repairing them, or choose “Try again now” on any provider to reset them."
            testID="websearch-pauses-error"
            action={<Action label="Turn off web search" testID="websearch-turn-off-paused" disabled={m.busy()}
              onPress={() => { dismiss(); void m.update({ enabled: false }, "Web search is off on this computer."); }} />} />
        </If>
        <If when={s().settingsError === "unknown"}>
          <ProblemNote text="The helper can't read the local web search settings. Refresh to check again." testID="websearch-settings-error" />
        </If>
        <If when={s().keysError}>
          <ProblemNote text="Your saved web search keys can't be read. Searches and tests aren't sent. Reset the keys, then add them again." testID="websearch-keys-error" />
          <ConfirmInline testID="websearch-reset-keys" triggerLabel="Reset saved keys…" confirmLabel="Reset saved keys"
            message="Delete every saved web search key for this account on this computer? This can't be undone."
            disabled={m.busy() || s().endpointsError} onConfirm={() => { dismiss(); void m.resetKeys(); }} />
        </If>
      </SettingsGroup>
      <SettingsGroup title="Providers" description="Tried in this order. If one hits a limit or is unavailable, the next eligible provider is tried and the run shows what happened.">
        <Each of={s().providers} key={(provider) => provider.id}>
          {(provider, i) => (
            <ProviderRow provider={provider()} index={i()} model={m} state={s()} blocked={blocked()}
              editing={edit() === provider().id} removing={removal() === provider().id}
              setEdit={(open) => { removal.set(null); edit.set(open ? provider().id : null); }}
              setRemoval={(open) => { if (open) edit.set(null); removal.set(open ? provider().id : null); }} />
          )}
        </Each>
        <If when={!s().settingsError && s().providers.every((p) => !p.enabled)}>
          <ProblemNote tone="warning" testID="websearch-all-off" text="All providers are off on this computer. Turn one on to let this computer serve searches." />
        </If>
        <If when={!s().settingsError && !s().keysError && s().providers.some((p) => p.enabled) && s().providers.every((p) => !p.enabled || (!p.keyless && !p.keySaved))}>
          <ProblemNote tone="warning" testID="websearch-no-eligible" text="No enabled provider can search without a key. Add a key or turn on a keyless provider." />
        </If>
        <If when={m.canTest()} else={<Label variant="caption">Update Metro to test providers.</Label>}>
          <Label variant="caption">If setup succeeds, testing sends “Metro web search test” to the selected provider in its current mode. This can use provider credits.</Label>
        </If>
      </SettingsGroup>
    </>
  );
}

function ProviderRow(props: {
  provider: WebProviderRow; index: number; model: WebSearchSettingsModel; state: LocalWebSearch; blocked: boolean;
  editing: boolean; removing: boolean; setEdit: (open: boolean) => void; setRemoval: (open: boolean) => void;
}) {
  const p = () => props.provider;
  const id = p().id;
  const name = WEB_PROVIDERS[id];
  const m = props.model;
  let menu: ViewRef | undefined;
  const status = () => {
    const row = p();
    const mode = props.state.keysError ? "Key status unavailable" : row.keySaved ? "Your key" : row.keyless ? "No key" : "Key required";
    const pause = row.cooldown;
    const reason = pause?.outcome === "limited" || pause?.outcome === "out_of_credits" ? "Limit reached" :
      pause ? WEB_OUTCOMES[pause.outcome] : null;
    const text = [!row.enabled ? "Off" : "", mode, row.keyRejected ? "Key rejected" : "",
      pause && `${reason} · retry after ${pause.until.slice(0, 10)} ${pause.until.slice(11, 16)} UTC`].filter(Boolean).join(" · ");
    return { text, tone: !row.enabled ? "neutral" as const : row.keyRejected || pause || (!row.keyless && !row.keySaved) ? "warning" as const : "success" as const };
  };
  const change = (value: boolean | -1 | 1) => {
    const providers = providerSettings(props.state.providers, id, value);
    if (providers) void m.update({ providers }, typeof value === "boolean" ? `${name} is ${value ? "on" : "off"}.` :
      `${name} moved to position ${providers.findIndex((item) => item.id === id) + 1} of ${providers.length}.`);
  };
  const closeEditor = () => { props.setEdit(false); menu?.focus(); };
  return (
    <ConnectionItem testID={`websearch-provider:${id}`} name={name} mark={{ letters: name.slice(0, 2), tone: "system" }} status={status()}
      trailing={
        <Row align="center" gap={1} wrap>
          <Toggle testID={`websearch-enabled:${id}`} label={`Use ${name}`} checked={p().enabled} disabled={props.blocked} onChange={change} />
          <ReorderButtons label={name} canMoveUp={props.index > 0} canMoveDown={props.index + 1 < props.state.providers.length}
            disabled={props.blocked} upTestID={`websearch-up:${id}`} downTestID={`websearch-down:${id}`} onMove={change} />
          <MoreMenu testID={`websearch-menu:${id}`} accessibilityLabel={`${name} actions`}
            triggerRef={(node) => { menu = node; }}
            items={[
              { id: `websearch-edit:${id}`, label: p().keySaved ? "Replace key…" : "Add key…",
                disabled: props.blocked || props.state.keysError, onPress: () => props.setEdit(true) },
              { id: `websearch-test:${id}`, label: p().enabled ? "Test provider" : "Enable provider to test",
                disabled: !m.canTest() || props.blocked || !props.state.enabled || !p().enabled || props.state.keysError || (!p().keyless && !p().keySaved),
                onPress: () => void m.test(id) },
              { id: `websearch-clear:${id}`, label: "Try again now",
                disabled: m.busy() || props.state.endpointsError || !p().cooldown && !p().keyRejected && props.state.settingsError !== "pauses",
                onPress: () => void m.clear(id) },
              { id: `websearch-terms:${id}`, label: "Vendor’s terms",
                onPress: () => void openUrl(TERMS[id]).catch(() => announce("Couldn't open the vendor's terms.")) },
              ...(p().keySaved ? [{ id: `websearch-remove:${id}`, label: "Remove key…", destructive: true,
                disabled: props.blocked || props.state.keysError, onPress: () => props.setRemoval(true) }] : []),
            ]} />
        </Row>
      }>
      <If when={props.editing && !props.state.keysError && !props.state.settingsError}>
        <KeyEditor id={id} busy={m.busy()} onCancel={closeEditor} onSave={(key) => { closeEditor(); void m.saveKey(id, key); }} />
      </If>
      <ConfirmInline testID={`websearch-remove-confirm:${id}`} open={props.removing} onOpenChange={props.setRemoval}
        returnFocus={() => menu} confirmLabel="Remove key" disabled={props.blocked || props.state.keysError}
        message={`Remove your ${name} key? ${p().keyless ? "When used, this provider will run without a key." : `${name} can't search without it.`}`}
        onConfirm={() => { props.setRemoval(false); void m.removeKey(id); }} />
    </ConnectionItem>
  );
}

function KeyEditor(props: { id: WebProvider; busy: boolean; onSave: (key: string) => void; onCancel: () => void }) {
  const key = signal("");
  const wipe = () => key.set("");
  let alive = true;
  cleanup(() => { alive = false; wipe(); });
  const cancel = () => { wipe(); props.onCancel(); };
  // Escape also cancels when focus moved from the field to another control.
  cleanup(keymap.bind("escape", cancel, { when: "always", forFocus: true }));
  const save = () => {
    const value = key().trim();
    if (!value || props.busy) return;
    wipe();
    props.onSave(value);
  };
  return (
    <Column gap={2} testID={`websearch-editor:${props.id}`}>
      <Field testID={`websearch-key:${props.id}`} masked placeholder={`${WEB_PROVIDERS[props.id]} API key`}
        inputRef={(node) => {
          // Focus after the menu releases its keyboard session.
          void node.measure().then((layout) => { if (alive && layout && !props.busy) node.focus(); });
        }}
        value={key()} onChange={key.set} onSubmit={save} disabled={props.busy} />
      <Label variant="caption">Keys are saved for this account on this computer. Metro never uploads them to its server or shows them again. A key goes only to its provider, which may charge your account.</Label>
      <Row gap={2} wrap>
        <Action variant="primary" label="Save key" testID={`websearch-save:${props.id}`} disabled={!key().trim() || props.busy} onPress={save} onEscape={cancel} />
        <Action variant="ghost" label="Cancel" testID={`websearch-cancel:${props.id}`} onPress={cancel} onEscape={cancel} />
      </Row>
    </Column>
  );
}

function PersonalChat(props: { model: WebSearchSettingsModel; server: string }) {
  const m = props.model;
  const setting = () => { const pref = m.chat(); return pref.phase === "ready" ? pref : null; };
  const host = () => props.server.replace(/^https?:\/\//, "");
  return (
    <SettingsGroup title="Personal Chat">
      <ScopeLine scope={{ kind: "account", host: host() }} />
      <SettingRow label="Allow web search in personal Chat"
        description="Off by default. Lets your Chat agent search and read pages through your online web devices. It doesn't change the computer switch or Space permissions.">
        <If when={setting()} else={<Badge label={m.chat().phase === "loading" ? "Loading" : "Unavailable"} tone="neutral" />}>
          {(pref) => <Toggle label="Allow web search in personal Chat" testID="websearch-chat-opt-in"
            checked={pref().enabled} disabled={m.chatBusy()} onChange={(on) => void m.setChat(on)} />}
        </If>
      </SettingRow>
      <If when={m.chat().phase === "unsupported"}>
        <ProblemNote tone="warning" text="This Metro server doesn't support web search in personal Chat yet." testID="websearch-chat-unsupported" />
      </If>
      <If when={m.chat().phase === "error"}>
        <ProblemNote text={m.chatError() ?? "Couldn't load the personal Chat setting from this server."} testID="websearch-chat-error"
          action={<Action label="Reload" testID="websearch-chat-reload" onPress={() => void m.readChat()} />} />
      </If>
    </SettingsGroup>
  );
}
