// Settings › Model routing: which of the user's usable models answers which
// kind of request (use-case routing), the opt-in that lets System One start
// the agent from an unaddressed message of theirs, and the embeddings model
// search uses. Every picker offers only models of the right capability that
// the user owns or that someone shared with them. The page says, from what
// is chosen, which endpoint receives which text.

import {
  addCase,
  available,
  availableError,
  draft,
  editCase,
  editRouting,
  findAvailable,
  MAX_CASES,
  MAX_DESCRIPTION,
  MAX_LABEL,
  MIN_DIRECTEDNESS_FLOOR,
  modelsFor,
  removeCase,
  revertRouting,
  routing,
  routingDirty,
  routingError,
  routingNotice,
  routingSaving,
  routingUnsupported,
  saveRouting,
} from "../routing";
import type { AvailableModel, DraftCase, RoutingDraft } from "../routing";
import { providerById } from "../settings";
import type { Capability } from "../settings";
import { openSettings } from "../store";
import { Action, Badge, Banner, Column, Field, Label, PageHeader, Picker, Row, ScrollArea, SettingRow, SettingsGroup, Surface, Toggle } from "../ui";
import type { PickerItem } from "../ui";

const NONE = "";

export function RoutingSettings() {
  return (
    <ScrollArea pad={6} testID="routing-settings">
      <Surface variant="content" gap={6}>
        <PageHeader
          title="Model routing"
          subtitle="Choose which model answers which kind of request, and whether System One may start the agent for you without @agent. Both are off until you turn them on, and they only ever apply to messages you write."
        />
        <Messages />
        <If when={!routingUnsupported()} else={<Unsupported />}>
          <If when={draft()} else={<Label variant="caption" color="muted">{routingError() ? "" : "Loading your model routing…"}</Label>}>
            {(d) => (
              <Column gap={6}>
                <ClassifierGroup draft={d()} />
                <RoutingGroup draft={d()} />
                <AddressingGroup draft={d()} />
                <SearchGroup draft={d()} />
                <DataFlow draft={d()} />
                <SaveBar />
              </Column>
            )}
          </If>
        </If>
      </Surface>
    </ScrollArea>
  );
}

function Unsupported() {
  return (
    <Banner tone="warning" testID="routing-unsupported">
      <Label color="warning">This server doesn't offer model routing. Runs you start use your default chat connection.</Label>
    </Banner>
  );
}

function Messages() {
  return (
    <Column gap={2}>
      <If when={availableError()}>
        {(message) => (
          <Banner tone="danger" testID="available-error">
            <Label color="danger">{message()}</Label>
          </Banner>
        )}
      </If>
      <If when={routingError()}>
        {(message) => (
          <Banner tone="danger" testID="routing-error">
            <Label color="danger">{message()}</Label>
          </Banner>
        )}
      </If>
      <If when={routingNotice()}>
        {(message) => (
          <Label variant="caption" color="muted" testID="routing-notice">
            {message()}
          </Label>
        )}
      </If>
    </Column>
  );
}

// ------------------------------------------------------------------ pickers

const GROUP: Record<AvailableModel["visibility"], string> = {
  personal: "Yours",
  spaces: "Shared with a Space you're in",
  organization: "Shared with the organization",
};

function modelLabel(m: AvailableModel): string {
  const provider = providerById(m.providerId)?.name ?? m.providerId;
  const whose = m.owned ? "" : m.runsOn === "your_own_sign_in" ? `, preset from ${m.ownerId ?? "someone"}` : `, shared by ${m.ownerId ?? "someone"}`;
  const notReady = m.ready === false ? " · sign in with your own account first" : "";
  return `${m.name} · ${m.model ?? "no model chosen yet"} (${provider}${whose})${notReady}`;
}

/**
 * Your own first, then shared ones. Something that can't answer can't be
 * picked: no model chosen, or a preset you have no connection of your own for.
 */
function itemsFor(capability: Capability, first?: PickerItem): PickerItem[] {
  const models = [...modelsFor(capability)].sort((a, b) => Number(b.owned) - Number(a.owned));
  const items: PickerItem[] = models.map((m) => ({
    value: m.connectionId,
    label: modelLabel(m),
    disabled: !m.model || m.ready === false,
    group: m.owned ? GROUP.personal : GROUP[m.visibility === "personal" ? "spaces" : m.visibility],
  }));
  return first ? [first, ...items] : items;
}

/** A model saved earlier that is no longer available (deleted, or its share revoked). */
function Missing(props: { id: string | null; testID: string }) {
  return (
    <If when={props.id && available() && !findAvailable(props.id)}>
      <Label variant="caption" color="warning" testID={props.testID}>
        The model saved here isn't available to you any more (deleted, or no longer shared with you). Choose another one.
      </Label>
    </If>
  );
}

function ModelPicker(props: {
  testID: string;
  label: string;
  capability: Capability;
  value: string | null;
  onChange: (id: string | null) => void;
  none: string;
}) {
  return (
    <Column gap={1}>
      <Picker
        testID={props.testID}
        accessibilityLabel={props.label}
        items={itemsFor(props.capability, { value: NONE, label: props.none })}
        value={props.value && findAvailable(props.value) ? props.value : NONE}
        onChange={(v) => props.onChange(v ? v : null)}
        placeholder={props.none}
        searchable
      />
      <Missing id={props.value} testID={`${props.testID}-missing`} />
    </Column>
  );
}

function NoneOf(props: { capability: Capability; what: string; testID: string }) {
  return (
    <If when={available() && modelsFor(props.capability).length === 0}>
      <Row gap={2} align="center" wrap testID={props.testID}>
        <Label variant="caption" color="muted">
          {`You have no ${props.what} connection, and none is shared with you. Add one in Model providers.`}
        </Label>
        <Action label="Open Model providers" variant="ghost" onPress={() => openSettings("providers")} />
      </Row>
    </If>
  );
}

// ------------------------------------------------------------------ groups

function ClassifierGroup(props: { draft: RoutingDraft }) {
  return (
    <SettingsGroup
      title="System One classifier"
      description="System One answers typed questions (Noul, choice, score). Use-case routing and the opt-in below use it, and search reranks with it. A chat model never stands in for it."
      testID="routing-classifier-group"
    >
      <SettingRow label="Classifier" description="Only System One connections are offered." stacked>
        <ModelPicker
          testID="routing-classifier"
          label="System One classifier"
          capability="system_one"
          value={props.draft.classifierId}
          onChange={(id) => editRouting({ classifierId: id })}
          none="None"
        />
        <NoneOf capability="system_one" what="System One" testID="routing-no-classifier" />
      </SettingRow>
    </SettingsGroup>
  );
}

function RoutingGroup(props: { draft: RoutingDraft }) {
  const d = () => props.draft;
  return (
    <SettingsGroup
      title="Use-case routing"
      description="When agent work starts for you (@agent, Ask agent, or a start the opt-in below allowed), System One picks which use case the message is, and that use case's model answers. A model you pick for one turn overrides it. With routing off, your default chat connection answers."
      testID="routing-group"
    >
      <SettingRow label="Route by use case" description="Off by default.">
        <Toggle testID="routing-enabled" label="Route by use case" checked={d().enabled} onChange={(on) => editRouting({ enabled: on })} />
      </SettingRow>
      <SettingRow label="Minimum confidence" description="Route only when System One is at least this sure of the use case (0 to 1). Below it, the fallback model answers." stacked>
        <Field
          testID="routing-min-route"
          accessibilityLabel="Minimum confidence to route"
          value={d().minRouteProbability}
          onChange={(v) => editRouting({ minRouteProbability: v })}
          placeholder="0.65"
          maxLength={6}
        />
      </SettingRow>
      <SettingRow label="Fallback answer model" description="Used when System One returns no match or a score below your threshold. If the classifier fails, no answering model is called." stacked>
        <ModelPicker
          testID="routing-default-chat"
          label="Fallback answer model"
          capability="chat"
          value={d().defaultChatId}
          onChange={(id) => editRouting({ defaultChatId: id })}
          none="Your default chat connection"
        />
      </SettingRow>
      <Each of={d().cases} key={(c) => c.uid}>
        {(c, i) => <CaseRow item={c()} index={i()} />}
      </Each>
      <If when={d().cases.length === 0}>
        <SettingRow label="No use cases yet" description="Add one for each kind of request you want a particular model to answer, such as Code review or Quick question." />
      </If>
      <NoneOf capability="chat" what="chat" testID="routing-no-chat" />
      <Row padX={4} padY={3} gap={2} align="center">
        <Action testID="routing-add-case" label="+ Use case" accessibilityLabel="Add a use case" disabled={d().cases.length >= MAX_CASES} onPress={addCase} />
        <Label variant="caption" color="muted">
          {`${d().cases.length} of ${MAX_CASES}`}
        </Label>
      </Row>
    </SettingsGroup>
  );
}

function CaseRow(props: { item: DraftCase; index: number }) {
  const c = () => props.item;
  const n = () => props.index;
  return (
    <SettingRow label={c().label.trim() || `Use case ${n() + 1}`} description="System One reads the name and description to tell use cases apart." stacked testID={`case:${n()}`}>
      <Column gap={2}>
        <Field testID={`case-label:${n()}`} accessibilityLabel={`Name of use case ${n() + 1}`} value={c().label} onChange={(v) => editCase(c().uid, { label: v })} placeholder="Name, such as Code review" maxLength={MAX_LABEL} />
        <Field
          testID={`case-description:${n()}`}
          accessibilityLabel={`Description of use case ${n() + 1}`}
          value={c().description}
          onChange={(v) => editCase(c().uid, { description: v })}
          placeholder="What kind of request it is, in a sentence or two"
          multiline
          maxLength={MAX_DESCRIPTION}
        />
        <Row gap={2} align="center" wrap>
          <Label variant="caption" color="muted">
            Answered by
          </Label>
          <Picker
            testID={`case-model:${n()}`}
            accessibilityLabel={`Model that answers use case ${n() + 1}`}
            items={itemsFor("chat")}
            value={c().connectionId && findAvailable(c().connectionId) ? c().connectionId : null}
            onChange={(v) => editCase(c().uid, { connectionId: v })}
            placeholder="Choose a chat model"
            searchable
          />
          <Action testID={`case-remove:${n()}`} label="Remove" accessibilityLabel={`Remove use case ${n() + 1}`} variant="ghost" onPress={() => removeCase(c().uid)} />
        </Row>
        <Missing id={c().connectionId} testID={`case-missing:${n()}`} />
      </Column>
    </SettingRow>
  );
}

function AddressingGroup(props: { draft: RoutingDraft }) {
  const d = () => props.draft;
  return (
    <SettingsGroup
      title="Start the agent without @agent (opt in)"
      description="Explicit recipients always decide. A message that mentions a person only (@bob) never starts the agent and is never classified. @agent and Ask agent start it with no classification. Only messages you write that mention nobody are judged, and only while this is on."
      testID="addressing-group"
    >
      <SettingRow label="Classify my unaddressed messages" description="Off by default. In rooms and in your Chat. A start it allows runs as you, with your connections.">
        <Toggle testID="routing-infer" label="Classify my unaddressed messages" checked={d().inferAddressing} onChange={(on) => editRouting({ inferAddressing: on })} />
      </SettingRow>
      <SettingRow label="Minimum confidence" description={`Start the agent only when System One is at least this sure the message asks it (${MIN_DIRECTEDNESS_FLOOR} to 1). Below it, the message stays ordinary conversation.`} stacked>
        <Field
          testID="routing-min-directed"
          accessibilityLabel="Minimum confidence that a message asks the agent"
          value={d().minDirectedness}
          onChange={(v) => editRouting({ minDirectedness: v })}
          placeholder="0.85"
          maxLength={6}
        />
      </SettingRow>
    </SettingsGroup>
  );
}

function SearchGroup(props: { draft: RoutingDraft }) {
  return (
    <SettingsGroup title="Search" description="Search finds messages by meaning in your Chat and in the Spaces you're in now. It needs an embeddings model." testID="search-model-group">
      <SettingRow label="Embeddings model" description="Only embeddings connections are offered: a cloud API, an endpoint your operator approved, or one on your device." stacked>
        <ModelPicker
          testID="routing-embedding"
          label="Embeddings model"
          capability="embeddings"
          value={props.draft.embeddingId}
          onChange={(id) => editRouting({ embeddingId: id })}
          none="None (search is off)"
        />
        <NoneOf capability="embeddings" what="embeddings" testID="routing-no-embedding" />
      </SettingRow>
    </SettingsGroup>
  );
}

// ------------------------------------------------------------------ where text goes

/** Who receives text sent to `m`, in words: the server, the user's own device, or someone else's. */
export function endpointText(m: AvailableModel | null): string {
  if (!m) return "nobody (none chosen)";
  const name = providerById(m.providerId)?.name ?? m.providerId;
  const device = m.execution === "device";
  const owner = m.ownerId ?? "its owner";
  if (m.runsOn === "your_own_sign_in") {
    return `“${m.name}”, a preset ${owner} shared, which runs on your own sign-in (${name}). ${owner} sees none of it`;
  }
  if (m.owned || m.runsOn === "your_connection") {
    return device ? `“${m.name}”, which runs in your own metro-connector (${name})` : `“${m.name}” (${name}), which the Metro server calls with your settings`;
  }
  const pays = m.billing === "owner" ? ` and ${owner} may pay for them` : "";
  return device
    ? `“${m.name}”, shared by ${owner}, which runs on ${owner}'s computer (${name}). ${owner} can see these requests${pays}`
    : `“${m.name}” (${name}), shared by ${owner}. ${owner} can see the requests sent through it${pays}`;
}

function DataFlow(props: { draft: RoutingDraft }) {
  const d = () => props.draft;
  const classifier = () => findAvailable(d().classifierId);
  const embedding = () => findAvailable(d().embeddingId);
  return (
    <SettingsGroup title="Where your text goes" description="From what is chosen above. Nothing here is sent until you save." testID="routing-dataflow">
      <SettingRow
        label="Use-case routing"
        description={
          d().enabled
            ? `When agent work starts for you, that message's text and your use-case names and descriptions go to ${endpointText(classifier())}. The model it picks then answers as usual.`
            : "Off: nothing is sent to pick a model. Your default chat connection answers."
        }
        testID="dataflow-routing"
      >
        <Badge tone={d().enabled ? "agent" : "neutral"} label={d().enabled ? "On" : "Off"} />
      </SettingRow>
      <SettingRow
        label="Unaddressed messages"
        description={
          d().inferAddressing
            ? `Each message you write that mentions nobody and isn't sent with Ask agent goes to ${endpointText(classifier())}.`
            : "Off: your unaddressed messages are ordinary conversation and are sent to no model."
        }
        testID="dataflow-addressing"
      >
        <Badge tone={d().inferAddressing ? "agent" : "neutral"} label={d().inferAddressing ? "On" : "Off"} />
      </SettingRow>
      <SettingRow
        label="Search"
        description={
          d().embeddingId
            ? `To index and search, your queries and the text of messages you can read (your Chat, and the rooms and threads of Spaces you're in now, other members' messages there included) go to ${endpointText(embedding())}.${d().classifierId ? ` Each query and its top results (at most 10) also go to ${endpointText(classifier())} to rerank them.` : " Results keep their embedding order: no classifier reranks them."}`
            : "Off: no embeddings model is chosen, so nothing is indexed."
        }
        testID="dataflow-search"
      >
        <Badge tone={d().embeddingId ? "agent" : "neutral"} label={d().embeddingId ? "On" : "Off"} />
      </SettingRow>
      <SettingRow
        label="Never sent"
        description="Messages that mention only people, and other members' messages for routing or classification. Your settings apply to messages you write, and never to anyone else's."
        testID="dataflow-never"
      />
    </SettingsGroup>
  );
}

function SaveBar() {
  return (
    <Row gap={2} justify="end" wrap>
      <Action testID="routing-revert" label="Revert" variant="ghost" disabled={!routing() || !routingDirty() || routingSaving()} onPress={revertRouting} />
      <Action testID="routing-save" label="Save routing" busyLabel="Saving…" busy={routingSaving()} disabled={!routingDirty()} variant="primary" onPress={() => void saveRouting()} />
    </Row>
  );
}
