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
  parseProbability,
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
import type { JSX } from "nsh";
import type { AvailableModel, DraftCase, RoutingDraft } from "../routing";
import { providerById } from "../settings";
import type { Capability } from "../settings";
import { openSettings } from "../store";
import { Action, ActionBar, Badge, Banner, Column, compactWidth, FactList, Field, Label, LearnMore, PageHeader, PageLayout, PercentField, Picker, Row, SettingRow, SettingsGroup, Toggle } from "../ui";
import type { Fact, PickerItem } from "../ui";

const NONE = "";

// ------------------------------------------------------------------ percentages

/** The "asking the agent" floor as a percentage (50). */
const FLOOR_PERCENT = Math.round(MIN_DIRECTEDNESS_FLOOR * 100);

/**
 * What a confidence field shows for the draft's value, as a whole
 * percentage: a stored 0.65 shows as "65", a typed "65%" as "65". Anything
 * else shows as it was typed, so the field never rewrites the person's input.
 */
export function percentText(value: string): string {
  const t = value.trim();
  if (t.endsWith("%")) return t.slice(0, -1).trim();
  const n = parseProbability(t);
  return n === null ? t : String(Math.round(n * 100));
}

/** What the draft keeps for a typed percentage: "65" becomes "65%", which src/routing.ts reads as 0.65. */
export function fromPercent(typed: string): string {
  const t = typed.replace(/%/g, "").trim();
  return t ? `${t}%` : "";
}

/** Why a typed percentage can't be saved, or null: a whole number from `min` to 100. */
export function percentError(typed: string, min: number): string | null {
  const t = typed.trim();
  return /^[0-9]{1,3}$/.test(t) && Number(t) >= min && Number(t) <= 100 ? null : `Enter a whole number from ${min} to 100.`;
}

/** Both confidence fields hold whole percentages in range. */
function percentsValid(d: RoutingDraft): boolean {
  return percentError(percentText(d.minRouteProbability), 0) === null && percentError(percentText(d.minDirectedness), FLOOR_PERCENT) === null;
}

export function RoutingSettings() {
  return (
    <PageLayout
      testID="routing-settings"
      pad={compactWidth() ? 4 : 6}
      gap={6}
      footer={
        <Column gap={2}>
          <Messages />
          <If when={!routingUnsupported() && draft()}>
            <SaveBar />
          </If>
        </Column>
      }
    >
      <PageHeader
        title="Model routing"
        subtitle="Use-case routing and starting the agent without @agent are off until you turn them on, and apply only to messages you write."
      />
      <If when={!routingUnsupported()} else={<Unsupported />}>
        <If when={draft()} else={<Label variant="caption" color="muted">{routingError() ? "" : "Loading your model routing…"}</Label>}>
          {(d) => (
            <Column gap={6}>
              <ClassifierGroup draft={d()} />
              <RoutingGroup draft={d()} />
              <AddressingGroup draft={d()} />
              <SearchGroup draft={d()} />
              <DataFlow draft={d()} />
            </Column>
          )}
        </If>
      </If>
    </PageLayout>
  );
}

function Unsupported() {
  return (
    <Banner
      variant="inline"
      tone="warning"
      testID="routing-unsupported"
      message="This server doesn't offer model routing. Runs you start use your default chat connection."
    />
  );
}

/** Errors and what the last save did, in the fixed footer beside Save so they're seen. */
function Messages() {
  return (
    <Column gap={2}>
      <If when={availableError()}>{(message) => <Banner variant="inline" tone="danger" testID="available-error" message={message()} />}</If>
      <If when={routingError()}>{(message) => <Banner variant="inline" tone="danger" testID="routing-error" message={message()} />}</If>
      <If when={routingNotice()}>{(message) => <Banner variant="inline" tone="success" testID="routing-notice" message={message()} />}</If>
    </Column>
  );
}

// ------------------------------------------------------------------ pickers

const GROUP: Record<AvailableModel["visibility"], string> = {
  personal: "Yours",
  spaces: "Shared with a Space you're in",
  organization: "Shared with the organization",
};

/** An option's first line: the connection and its model. */
function modelLabel(m: AvailableModel): string {
  return `${m.name} · ${m.model ?? "no model chosen yet"}`;
}

/**
 * An option's second line: its provider, whose it is, and why it can't be
 * picked yet. Never a base URL, key or sign-in: a borrowed model's owner keeps those.
 */
function modelDetail(m: AvailableModel): string {
  const provider = providerById(m.providerId)?.name ?? m.providerId;
  const whose = m.owned ? "Yours" : m.runsOn === "your_own_sign_in" ? `Preset from ${m.ownerId ?? "someone"}` : `Shared by ${m.ownerId ?? "someone"}`;
  const notReady = m.ready === false ? " · Sign in with your own account first" : "";
  return `${provider} · ${whose}${notReady}`;
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
    detail: modelDetail(m),
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

/** A section's Learn more, as the first row of its card. */
function SectionMore(props: { testID: string; children?: JSX.Element }) {
  return (
    <Column padX={4} padY={2}>
      <LearnMore testID={props.testID}>{props.children}</LearnMore>
    </Column>
  );
}

function ClassifierGroup(props: { draft: RoutingDraft }) {
  return (
    <SettingsGroup
      title="System One classifier"
      description="Routing, the opt-in below and search use this System One connection; a chat model never stands in for it."
      testID="routing-classifier-group"
    >
      <SectionMore testID="classifier-learn-more">
        <FactList facts={[{ label: "Answers", value: "Typed questions: Noul, choice and score." }, { label: "Used by", value: "Use-case routing, the opt-in below, and search, which reranks with it." }]} />
      </SectionMore>
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
      description="When agent work starts for you, System One picks the use case and that use case's model answers."
      testID="routing-group"
    >
      <SectionMore testID="routing-learn-more">
        <FactList
          facts={[
            { label: "Starts on", value: "@agent, Ask agent, or a start the opt-in below allowed." },
            { label: "Override", value: "A model you pick for one turn overrides it." },
            { label: "When off", value: "Your default chat connection answers." },
          ]}
        />
      </SectionMore>
      <SettingRow label="Route by use case" description="Off by default.">
        <Toggle testID="routing-enabled" label="Route by use case" checked={d().enabled} onChange={(on) => editRouting({ enabled: on })} />
      </SettingRow>
      <SettingRow label="Minimum confidence" description="Route only when System One is at least this sure of the use case, 0 to 100%. Below it, the fallback model answers." stacked>
        <PercentField
          testID="routing-min-route"
          accessibilityLabel="Minimum confidence to route"
          value={percentText(d().minRouteProbability)}
          onChange={(v) => editRouting({ minRouteProbability: fromPercent(v) })}
          error={percentError(percentText(d().minRouteProbability), 0)}
          placeholder="65"
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
          <Action testID={`case-remove:${n()}`} label="Remove" accessibilityLabel={`Remove use case ${n() + 1}`} variant="destructive" edgeAligned onPress={() => removeCase(c().uid)} />
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
      description="While this is on, System One judges only messages you write that mention nobody."
      testID="addressing-group"
    >
      <SectionMore testID="addressing-learn-more">
        <FactList
          facts={[
            { label: "Recipients", value: "Explicit recipients always decide." },
            { label: "@person only", value: "A message that mentions a person only (@bob) never starts the agent and is never classified." },
            { label: "@agent", value: "@agent and Ask agent start it with no classification." },
          ]}
        />
      </SectionMore>
      <SettingRow label="Classify my unaddressed messages" description="Off by default. In rooms and in your Chat. A start it allows runs as you, with your connections.">
        <Toggle testID="routing-infer" label="Classify my unaddressed messages" checked={d().inferAddressing} onChange={(on) => editRouting({ inferAddressing: on })} />
      </SettingRow>
      <SettingRow
        label="Minimum confidence"
        description={`Start the agent only when System One is at least this sure the message asks it, ${FLOOR_PERCENT} to 100%. Below it, the message stays ordinary conversation.`}
        stacked
      >
        <PercentField
          testID="routing-min-directed"
          accessibilityLabel="Minimum confidence that a message asks the agent"
          value={percentText(d().minDirectedness)}
          onChange={(v) => editRouting({ minDirectedness: fromPercent(v) })}
          error={percentError(percentText(d().minDirectedness), FLOOR_PERCENT)}
          placeholder="85"
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

/**
 * Who receives text sent to `m`, as short rows: which connection gets it,
 * where it runs, and who else can see it or pays. Nothing is dropped: a
 * shared model always says its owner can see the requests (and may pay).
 */
export function endpointFacts(m: AvailableModel | null, to = "To"): Fact[] {
  if (!m) return [{ label: to, value: "Nobody (none chosen)" }];
  const name = providerById(m.providerId)?.name ?? m.providerId;
  const owner = m.ownerId ?? "its owner";
  const facts: Fact[] = [{ label: to, value: `“${m.name}” (${name})` }];
  if (m.runsOn === "your_own_sign_in") {
    facts.push({ label: "Runs on", value: `Your own sign-in. ${owner} shared only the preset` }, { label: "Who sees it", value: `Not ${owner}: they see none of it` });
    return facts;
  }
  const device = m.execution === "device";
  if (m.owned || m.runsOn === "your_connection") {
    facts.push({ label: "Runs on", value: device ? "Your own metro-connector" : "The Metro server calls it with your settings" });
    return facts;
  }
  facts.push(
    { label: "Runs on", value: device ? `${owner}'s computer, shared by ${owner}` : `${owner}'s connection, shared by ${owner}` },
    { label: "Who sees it", value: `${owner} can see these requests` },
  );
  if (m.billing === "owner") facts.push({ label: "Who pays", value: `${owner} may pay for them` });
  return facts;
}

/** One flow of text: whether it's on, and when it is, what goes where. */
function FlowRow(props: { label: string; on: boolean; off: string; facts: Fact[]; testID: string }) {
  return (
    <SettingRow label={props.label} description={props.on ? undefined : props.off} stacked testID={props.testID}>
      <Column gap={2}>
        <Row>
          <Badge tone={props.on ? "agent" : "neutral"} label={props.on ? "On" : "Off"} />
        </Row>
        <If when={props.on}>
          <FactList facts={props.facts} testID={`${props.testID}-facts`} />
        </If>
      </Column>
    </SettingRow>
  );
}

function DataFlow(props: { draft: RoutingDraft }) {
  const d = () => props.draft;
  const classifier = () => findAvailable(d().classifierId);
  const embedding = () => findAvailable(d().embeddingId);
  return (
    <SettingsGroup title="Where your text goes" description="From what is chosen above. Nothing here is sent until you save." testID="routing-dataflow">
      <FlowRow
        label="Use-case routing"
        on={d().enabled}
        off="Off: nothing is sent to pick a model. Your default chat connection answers."
        testID="dataflow-routing"
        facts={[
          { label: "When", value: "Agent work starts for you" },
          { label: "Sends", value: "That message's text, and your use-case names and descriptions" },
          ...endpointFacts(classifier()),
          { label: "Then", value: "The model it picks answers as usual" },
        ]}
      />
      <FlowRow
        label="Unaddressed messages"
        on={d().inferAddressing}
        off="Off: your unaddressed messages are ordinary conversation and are sent to no model."
        testID="dataflow-addressing"
        facts={[{ label: "Sends", value: "Each message you write that mentions nobody and isn't sent with Ask agent" }, ...endpointFacts(classifier())]}
      />
      <FlowRow
        label="Search"
        on={!!d().embeddingId}
        off="Off: no embeddings model is chosen, so nothing is indexed."
        testID="dataflow-search"
        facts={[
          { label: "Sends", value: "Your queries, and the text of messages you can read: your Chat, and the rooms and threads of Spaces you're in now, other members' messages there included" },
          ...endpointFacts(embedding()),
          ...(d().classifierId
            ? [{ label: "Reranks", value: "Each query and its top results (at most 10) also go to the classifier" }, ...endpointFacts(classifier(), "Classifier")]
            : [{ label: "Reranks", value: "No: results keep their embedding order" }]),
        ]}
      />
      <SettingRow
        label="Never sent"
        description="Messages that mention only people, and other members' messages for routing or classification. Your settings apply to messages you write, and never to anyone else's."
        testID="dataflow-never"
      />
    </SettingsGroup>
  );
}

function SaveBar() {
  const invalid = () => {
    const d = draft();
    return !!d && !percentsValid(d);
  };
  return (
    <ActionBar justify="between" testID="routing-footer">
      <Column grow>
        <If
          when={invalid()}
          else={
            <If when={routingDirty()}>
              <Label variant="caption" color="muted" testID="routing-unsaved">
                Unsaved changes. Nothing is sent until you save.
              </Label>
            </If>
          }
        >
          <Label variant="caption" color="danger" testID="routing-invalid">
            Fix the confidence above: a whole number of percent.
          </Label>
        </If>
      </Column>
      <Row gap={2}>
        <Action testID="routing-revert" label="Revert" variant="ghost" disabled={!routing() || !routingDirty() || routingSaving()} onPress={revertRouting} />
        <Action
          testID="routing-save"
          label="Save routing"
          busyLabel="Saving…"
          busy={routingSaving()}
          disabled={!routingDirty() || invalid()}
          variant="primary"
          onPress={() => void saveRouting()}
        />
      </Row>
    </ActionBar>
  );
}
