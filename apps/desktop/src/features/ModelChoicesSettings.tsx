// Settings › Model choices: which of the user's usable models answers which
// kind of request (use-case routing), confidence for Space classification,
// and the embeddings model
// search uses. Every picker offers only models of the right capability that
// the user owns or that someone shared with them. The page says, from what
// is chosen, which endpoint receives which text.

import {
  addCase,
  available,
  availableError,
  availableLoading,
  draft,
  editCase,
  editRouting,
  findAvailable,
  loadAvailable,
  loadRouting,
  MAX_CASES,
  MAX_DESCRIPTION,
  MAX_LABEL,
  MIN_DIRECTEDNESS_FLOOR,
  modelChoiceUsable,
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
import { providerById, settingsAccountScope } from "../settings";
import type { Capability } from "../settings";
import { openSettings, settingsExtensionAvailable } from "../store";
import { Action, ActionBar, Badge, Banner, Column, compactWidth, FactList, Field, Label, LearnMore, ModelPicker, PageHeader, PageLayout, PercentField, ProblemNote, Row, SettingRow, SettingsGroup, Toggle } from "../ui";
import type { Fact, ModelOption } from "../ui";
import { vendorOf } from "./providerGroups";
import { MediaModelSettings, MediaModelFeedback, MediaModelSaveBar } from "./library/LibraryView";

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

const catalogReady = () => !!available() && !availableLoading() && !availableError();

export function ModelChoicesSettings() {
  return (
    <PageLayout
      testID="routing-settings"
      pad={compactWidth() ? 4 : 6}
      gap={6}
      footer={
        <Column gap={2}>
          <Messages />
          <MediaModelFeedback />
          <MediaModelSaveBar separate />
          <If when={!routingUnsupported() && draft()}>
            <SaveBar />
          </If>
        </Column>
      }
    >
      <PageHeader title="Model choices" subtitle="Choose how Metro routes requests, searches messages and indexes files." scope={settingsAccountScope()} />
      <If when={!routingUnsupported()}>
        <If when={draft()} else={<If when={!routingError()}><SettingsGroup><SettingRow label="Loading your model choices…" /></SettingsGroup></If>}>
          {(d) => (
            <Column gap={6}>
              <If when={availableLoading()}><SettingsGroup><SettingRow label="Loading the models you can use…" /></SettingsGroup></If>
              <ClassifierGroup draft={d()} />
              <RoutingGroup draft={d()} />
              <AddressingGroup draft={d()} />
              <SearchGroup draft={d()} />
            </Column>
          )}
        </If>
      </If>
      <MediaModelSettings separate canReveal={!availableLoading() && !!(draft() || routingUnsupported() || routingError())} />
      <If when={routingUnsupported() ? null : draft()}>
        {(d) => <DataFlow draft={d()} />}
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
      <If when={availableError()}>{(message) => <Banner variant="inline" tone="danger" testID="available-error" message={message()}
        action={<Action label="Try again" onPress={() => void loadAvailable()} />} />}</If>
      <If when={routingUnsupported()}><Unsupported /></If>
      <If when={routingError()}>{(message) => <Banner variant="inline" tone="danger" testID="routing-error" message={message()}
        action={!draft() ? <Action label="Try again" onPress={() => void loadRouting()} /> : undefined} />}</If>
      <If when={routingNotice()}>{(message) => <Banner variant="inline" tone="success" testID="routing-notice" message={message()} />}</If>
    </Column>
  );
}

// ------------------------------------------------------------------ pickers

/** The default model of a usable connection, by the name pickers show, else its id. */
function defaultModelOf(m: AvailableModel): string | null {
  const d = m.chatModels?.find((x) => x.isDefault) ?? m.chatModels?.find((x) => x.id === m.model);
  return d?.displayName ?? m.model;
}

/**
 * An option's muted line: its model, whose it is, and why it can't be picked
 * yet. Never a base URL, key or sign-in: a borrowed model's owner keeps those.
 * A shared one says its owner sees the requests.
 */
function modelDetail(m: AvailableModel): string {
  const whose = m.owned
    ? "Yours"
    : m.runsOn === "your_own_sign_in"
      ? `Preset from ${m.ownerId ?? "someone"} · runs on your own sign-in`
      : `Shared by ${m.ownerId ?? "someone"}, who can see these requests`;
  const scope = !m.owned && m.visibility === "spaces" ? " · Shared Spaces only, not personal Chat" : "";
  const model = defaultModelOf(m);
  const notReady = !m.model ? " · Add a model to it first"
    : m.ready === false && m.runsOn === "your_own_sign_in" ? " · Sign in with your own account first"
    : modelChoiceUsable(m, m.capability) ? "" : " · Not available now";
  return [model, whose].filter(Boolean).join(" · ") + scope + notReady;
}

/**
 * Your own first, then shared ones, grouped by provider. Something that
 * can't answer can't be picked: no model, or a preset you have no connection
 * of your own for.
 */
function optionsFor(capability: Capability, none?: string): ModelOption[] {
  const models = [...modelsFor(capability)].sort((a, b) => Number(b.owned) - Number(a.owned));
  const options: ModelOption[] = models.map((m) => {
    const v = vendorOf({ id: m.providerId, name: providerById(m.providerId)?.name ?? m.providerId });
    return {
      value: m.connectionId,
      model: m.name,
      detail: modelDetail(m),
      title: [m.name, defaultModelOf(m), m.model, v.name, m.owned ? "yours" : m.ownerId].filter(Boolean).join(" · "),
      provider: v,
      disabled: !modelChoiceUsable(m, capability),
    };
  });
  return none ? [{ value: NONE, model: none, title: none }, ...options] : options;
}

function unavailable(id: string | null, capability: Capability): boolean {
  if (!id || !catalogReady()) return false;
  const model = findAvailable(id);
  return !model || !modelChoiceUsable(model, capability);
}

function RoutingModelPicker(props: {
  testID: string;
  label: string;
  capability: Capability;
  value: string | null;
  onChange: (id: string | null) => void;
  none?: string;
  placeholder?: string;
  missingTestID?: string;
}) {
  const missing = () => unavailable(props.value, props.capability);
  const pinned = () => props.value !== null;
  return (
    <Column gap={1}>
      <ModelPicker
        testID={props.testID}
        rememberAs="routing"
        accessibilityLabel={props.label}
        options={catalogReady() ? optionsFor(props.capability, props.none) : []}
        value={pinned() ? (missing() ? null : props.value) : props.none ? NONE : null}
        onChange={(v) => props.onChange(v ? v : null)}
        placeholder={pinned() ? (missing() ? "Unavailable selection" : "Checking saved selection…") : props.placeholder ?? props.none ?? "Choose a model"}
        attention={missing()}
        disabled={!catalogReady() || routingSaving()}
      />
      <If when={missing()}>
        <ProblemNote tone="warning" testID={props.missingTestID ?? `${props.testID}-missing`}
          text={props.none
            ? "This selection is unavailable. Metro won't replace it automatically; pick another model or clear the selection and save."
            : "This selection is unavailable. Metro won't replace it automatically; choose another model or remove this use case."}
          action={props.none ? <Action label="Clear selection" testID={`${props.testID}-clear`} variant="ghost" disabled={routingSaving()} onPress={() => props.onChange(null)} /> : undefined}
        />
      </If>
    </Column>
  );
}

function NoneOf(props: { capability: Capability; what: string; testID: string }) {
  return (
    <If when={catalogReady() && modelsFor(props.capability).length === 0}>
      <Row gap={2} align="center" wrap testID={props.testID}>
        <Label variant="caption" color="muted">
          {`You have no ${props.what} connection, and none is shared with you.`}
        </Label>
        <Action label="Open Model connections" variant="ghost" onPress={() => openSettings("connections")} />
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
      title="System One"
      description="Used by routing, Space classification and search. A chat model never stands in for it."
      testID="routing-classifier-group"
    >
      <SectionMore testID="classifier-learn-more">
        <FactList facts={[{ label: "Answers", value: "Typed questions: Noul, choice and score." }, { label: "Used by", value: "Use-case routing, Space classification when enabled, and search, which reranks with it." }]} />
      </SectionMore>
      <SettingRow label="System One connection" stacked>
        <RoutingModelPicker
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
      description="System One picks a use case; that use case's model answers."
      testID="routing-group"
    >
      <SectionMore testID="routing-learn-more">
        <FactList
          facts={[
            { label: "Applies to", value: "Agent runs you start when use-case routing is on and no model choice overrides it." },
            { label: "Override", value: "A model you pick for one turn overrides it. Models chosen for a Space or agent also take priority there." },
            { label: "When off", value: "Your default chat connection answers." },
            { label: "Use cases", value: "System One reads each name and description to tell them apart." },
          ]}
        />
      </SectionMore>
      <SettingRow label="Route by use case" description="Send eligible agent requests to System One to pick an answering model. Off by default.">
        <Toggle testID="routing-enabled" label="Route by use case" checked={d().enabled} disabled={routingSaving()} onChange={(on) => editRouting({ enabled: on })} />
      </SettingRow>
      <SettingRow label="Space-only connections" description="A model shared only with a Space can answer there, not in personal Chat." />
      <SettingRow label="Minimum confidence" description="0 to 100%. Below it, the fallback model answers." stacked>
        <PercentField
          testID="routing-min-route"
          accessibilityLabel="Minimum confidence to route"
          value={percentText(d().minRouteProbability)}
          onChange={(v) => editRouting({ minRouteProbability: fromPercent(v) })}
          error={percentError(percentText(d().minRouteProbability), 0)}
          disabled={routingSaving()}
          placeholder="65"
        />
      </SettingRow>
      <SettingRow label="Fallback answer model" description="Answers when there's no match or the confidence is below the minimum. If System One fails, no model answers." stacked>
        <RoutingModelPicker
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
        <SettingRow label="No use cases yet" description="Add one per kind of request, such as Code review." />
      </If>
      <NoneOf capability="chat" what="chat" testID="routing-no-chat" />
      <Row padX={4} padY={3} gap={2} align="center">
        <Action testID="routing-add-case" label="Add use case" disabled={d().cases.length >= MAX_CASES || routingSaving()} onPress={addCase} />
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
    <SettingRow label={c().label.trim() || `Use case ${n() + 1}`} description={`Name: up to ${MAX_LABEL} characters; description: up to ${MAX_DESCRIPTION}.`} stacked testID={`case:${n()}`}>
      <Column gap={2}>
        <Field testID={`case-label:${n()}`} accessibilityLabel={`Name of use case ${n() + 1}`} value={c().label} onChange={(v) => editCase(c().uid, { label: v })} placeholder="Name, such as Code review" maxLength={MAX_LABEL} disabled={routingSaving()} />
        <Field
          testID={`case-description:${n()}`}
          accessibilityLabel={`Description of use case ${n() + 1}`}
          value={c().description}
          onChange={(v) => editCase(c().uid, { description: v })}
          placeholder="What kind of request it is, in a sentence or two"
          multiline
          maxLength={MAX_DESCRIPTION}
          disabled={routingSaving()}
        />
        <Row gap={2} align="center" wrap>
          <Label variant="caption" color="muted">
            Answered by
          </Label>
          <Column grow>
            <RoutingModelPicker
              testID={`case-model:${n()}`}
              label={`Model that answers use case ${n() + 1}`}
              capability="chat"
              value={c().connectionId}
              onChange={(v) => editCase(c().uid, { connectionId: v })}
              placeholder="Choose a chat model"
              missingTestID={`case-missing:${n()}`}
            />
          </Column>
          <Action testID={`case-remove:${n()}`} label="Remove" accessibilityLabel={`Remove use case ${n() + 1}`} variant="destructive" edgeAligned disabled={routingSaving()} onPress={() => removeCase(c().uid)} />
        </Row>
      </Column>
    </SettingRow>
  );
}

function AddressingGroup(props: { draft: RoutingDraft }) {
  const d = () => props.draft;
  return (
    <SettingsGroup title="Space classification" description="Turn classification on or off in each Space's preferences. Personal Chat sends directly to your agent." testID="addressing-group">
      <SettingRow
        label="Minimum confidence"
        description={`${FLOOR_PERCENT} to 100%. Lower values let an agent decide more often whether to reply, react, or stay quiet.`}
        stacked
      >
        <PercentField
          testID="routing-min-directed"
          accessibilityLabel="Minimum confidence for unaddressed Space messages"
          value={percentText(d().minDirectedness)}
          onChange={(v) => editRouting({ minDirectedness: fromPercent(v) })}
          error={percentError(percentText(d().minDirectedness), FLOOR_PERCENT)}
          disabled={routingSaving()}
          placeholder="35"
        />
      </SettingRow>
    </SettingsGroup>
  );
}

function SearchGroup(props: { draft: RoutingDraft }) {
  return (
    <SettingsGroup title="Search" description="Finds messages by meaning in your Chat and the Spaces you're in. Needs an embeddings model." testID="search-model-group">
      <SettingRow label="Embeddings model" stacked>
        <RoutingModelPicker
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
  const flow = (id: string | null, cap: Capability, label?: string): Fact[] => {
    if (!id) return endpointFacts(null, label);
    if (!catalogReady()) return [{ label: label ?? "To", value: "Availability not confirmed" }];
    const model = findAvailable(id);
    if (!model || !modelChoiceUsable(model, cap)) return [{ label: label ?? "To", value: "The selected model is unavailable; no other model takes its place" }];
    return endpointFacts(model, label);
  };
  return (
    <SettingsGroup title="Where your text goes" description="Draft changes take effect only after you save." testID="routing-dataflow">
      <SettingRow label="Send in personal Chat" description="Send starts your Chat agent directly; System One never decides whether to start it." testID="dataflow-chat-send" />
      <SettingRow label="Save note in personal Chat" description="Save note keeps the message in your Chat without starting an agent or routing a request." testID="dataflow-chat-note" />
      <FlowRow
        label="Use-case routing"
        on={d().enabled}
        off="Off: nothing is sent to pick a model. Your default chat connection answers."
        testID="dataflow-routing"
        facts={[
          { label: "When", value: "Agent work starts for you, routing is enabled there, and no model choice overrides it" },
          { label: "Sends", value: "That message's text, and your use-case names and descriptions" },
          ...flow(d().classifierId, "system_one"),
          { label: "Then", value: "The model it picks answers as usual" },
        ]}
      />
      <SettingRow
        label="Space classification"
        description="Where enabled, System One gets your unaddressed message, the Space name and description, the room name, and up to three recent messages or agent replies from that room. It also receives your requests to the generic agent, without the recent messages. This never applies to personal Chat."
        testID="dataflow-addressing"
      />
      <FlowRow
        label="Search"
        on={!!d().embeddingId}
        off="Off: no embeddings model is chosen, so nothing is indexed."
        testID="dataflow-search"
        facts={[
          { label: "Sends", value: "Your queries, and the text of messages you can read: your Chat, and the rooms and threads of Spaces you're in now, other members' messages there included" },
          ...flow(d().embeddingId, "embeddings"),
          ...(d().classifierId
            ? [{ label: "Reranks", value: "Each query and its top results (at most 10) also go to the classifier" }, ...flow(d().classifierId, "system_one", "Classifier")]
            : [{ label: "Reranks", value: "No: results keep their embedding order" }]),
        ]}
      />
      <SettingRow
        label="Messages addressed to people"
        description="Messages addressed only to people don't start routing or classification. Recent room messages can be sent as classification context. Search can include messages you can read."
        testID="dataflow-never"
      />
      <If when={settingsExtensionAvailable("websearch")}>
        <SettingRow label="Web search" stacked testID="dataflow-websearch"
          description="For runs you start, Metro relays each permitted web request through your most recently seen online web device on this server, regardless of the model connection. Contacted providers may receive queries, page URLs and options like keywords or page focus. Metro keeps activity; the model may see limited web results for that run.">
          <Action label="Open Web search" variant="ghost" onPress={() => openSettings("websearch")} />
        </SettingRow>
      </If>
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
              <Label variant="caption" color="muted" testID="routing-unsaved">Unsaved model choices</Label>
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
          label="Save routing & search"
          busyLabel="Saving…"
          busy={routingSaving()}
          disabled={!routingDirty() || invalid() || !catalogReady()}
          variant="primary"
          onPress={() => void saveRouting()}
        />
      </Row>
    </ActionBar>
  );
}
