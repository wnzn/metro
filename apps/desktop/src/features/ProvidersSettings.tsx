// Settings › Model connections: the user's personal model connections. The
// providers offered come from the server (GET /v1/provider-catalog), never
// from a list in this app. Device connections (preferred) run in the user's
// own metro-connector, so this form never asks for their vendor key or
// password; server-stored API profiles take a key that is sent once and
// never shown again.
//
// Layout
// - The list: the title with one primary action, Add connection; then one
//   group per capability (Chat, System One, Embeddings), each a list of
//   compact rows: the provider's mark, the name with a "Default" tag, one
//   status line with a coloured dot, and a ⋯ menu. A problem shows as one
//   plain sentence under its row with at most one fix.
// - Details: a sub-view (← Model connections) per connection, opened from its
//   row or ⋯ › Details: Models, Device or Endpoint, Key, Sharing and Danger zone, and
//   technical text only under a closed Technical details.
// - Add connection opens one picker that lists every provider's ways to
//   connect at once, searchable and filterable by capability. Choosing a row
//   asks only for what that way needs. Providers that list models are saved
//   with every listed model on; custom endpoints take one or more models (and
//   their thinking options) as they are added. Device connections list their
//   models by themselves once paired (src/settings.ts).
//
// What must be said is said where it applies: at the key field, on the way's
// row, and in Sharing (the owner of the device or endpoint sees the requests
// and may pay for them). Feedback shows in the page's fixed footer.

import { cleanup, effect, signal } from "nsh";
import {
  closeSharing,
  isSubscription,
  openSharing,
  saveSharing,
  setShareVisibility,
  shareOf,
  shareBusy,
  shareDraft,
  shareError,
  shareNotice,
  shares,
  sharingOpen,
  toggleShareSpace,
} from "../routing";
import type { ShareVisibility } from "../routing";
import {
  addConnectionModel,
  CAPABILITY_LABEL,
  CAPABILITY_USE,
  catalog,
  catalogError,
  connectionCapability,
  connectionModels,
  connections,
  connectionsError,
  connectionsErrorDetail,
  createConnection,
  defaultModelName,
  deviceConfirmUnpair,
  deviceStatuses,
  deleteConnection,
  dismissEndpointCheck,
  endpointChecks,
  endpointNeedsReload,
  fetchModels,
  isAllowedBaseUrl,
  isDeviceConnection,
  loadProviders,
  modelListings,
  patchConnectionModel,
  providerBusy,
  providerById,
  providerConfirmDelete,
  providerNotice,
  reloadEndpoint,
  removeConnectionModel,
  revealed,
  rotateDeviceToken,
  settingsAccountScope,
  unpairDevice,
  updateConnection,
  updateEndpoint,
} from "../settings";
import type { Capability, CatalogProvider, ConnectionModel, ModelThinking, NewConnectionModel, ProviderConnection } from "../settings";
import { onSessionEnd, onTransientClear, sessionGuard, spaces } from "../store";
import {
  Action,
  ActionBar,
  Badge,
  Banner,
  BrandMark,
  Chip,
  Column,
  compactWidth,
  Disclosure,
  FactList,
  Field,
  Label,
  PageHeader,
  PageLayout,
  Row,
  SegmentedControl,
  SettingRow,
  SettingsGroup,
  ScopeLine,
  Toggle,
} from "../ui";
import type { Tone } from "../ui";
import { BackLink, ConnectionHeading, ConnectionItem, EditableName, MethodRow, ModelItem, MoreMenu, PickerPanel, ProblemNote, VendorHeading } from "../ui/providers";
import type { MoreMenuItem } from "../ui/providers";
import { ModelOnboarding, needsChatModel } from "./ModelOnboarding";
import { CAPABILITY_TONE, describePath, methodDetail, pickerGroups, vendorOf, wayLabel } from "./providerGroups";
import type { VendorGroup } from "./providerGroups";
import { localConnections, packaged } from "../device/state";
import { deviceStatusText, TokenReveal } from "./TokenReveal";

const SHARE_OPTIONS = [
  { value: "personal", label: "Only me" },
  { value: "spaces", label: "Selected Spaces" },
  { value: "organization", label: "Organization" },
] as const;

const SHARE_TEXT: Record<Exclude<ShareVisibility, "personal">, string> = { spaces: "Shared with Spaces", organization: "Shared with organization" };

/** The connection whose details are open, or null for the list. Dropped at sign-out. */
export const detailsOf = signal<string | null>(null);
onSessionEnd(() => detailsOf.set(null));

/** The add flow: closed, the picker open, or the form of one chosen way (a catalog id). */
type Adding = { step: "closed" } | { step: "picking" } | { step: "form"; providerId: string };
const adding = signal<Adding>({ step: "closed" });
const pickQuery = signal("");
const pickCapability = signal<Capability | "all">("all");
const pickVendor = signal<string | null>(null);
onSessionEnd(() => adding.set({ step: "closed" }));

/**
 * Opens the picker afresh, on `capability` when given. Pressing Add
 * connection while it's open changes nothing (its search and filter stay).
 */
function openPicker(capability?: Capability) {
  if (adding().step === "picking" && (capability === undefined || pickCapability() === capability)) return;
  pickQuery.set("");
  pickCapability.set(capability ?? "all");
  pickVendor.set(null);
  detailsOf.set(null);
  adding.set({ step: "picking" });
}
const closeAdding = () => adding.set({ step: "closed" });

/**
 * Starts adding a connection from elsewhere (the onboarding card): a catalog
 * id opens that way's form; "chat" opens the picker on chat ways. The caller
 * opens Settings › Model connections.
 */
export function startAdding(way: string | "chat") {
  detailsOf.set(null);
  if (way === "chat") openPicker("chat");
  else adding.set({ step: "form", providerId: way });
}

const openDetails = (id: string) => {
  closeAdding();
  detailsOf.set(id);
};

export function ProvidersSettings() {
  const device = () => revealed()?.kind === "device";
  const endpointFeedback = signal<EndpointFeedback | null>(null);
  const currentFeedback = () => {
    const id = detailsOf();
    if (!id) return null;
    return endpointCheckFeedback(id) ?? (endpointFeedback()?.id === id ? endpointFeedback() : null);
  };
  const hasMessages = () => !device() && !!(catalogError() || connectionsError() || providerNotice() || currentFeedback());
  const open = () => {
    const id = detailsOf();
    return id ? connections()?.find((c) => c.id === id) ?? null : null;
  };
  return (
    <PageLayout testID="providers-settings" pad={compactWidth() ? 4 : 6} gap={6} footer={hasMessages() ? <Messages endpoint={currentFeedback()} /> : undefined}>
      <If
        when={device()}
        else={
          <If
            when={open()}
            else={
              <Column gap={6}>
                <PageHeader title="Model connections" subtitle="Add and manage the connections Metro can use for your work." scope={settingsAccountScope()}>
                  <Action testID="provider-picker" label="Add connection" icon="plus" variant="primary" accessibilityLabel="Add a model connection" onPress={() => openPicker()} />
                </PageHeader>
                <AddConnection />
                <If when={adding().step === "closed"}>
                  <ModelOnboarding />
                </If>
                <ConnectionList />
              </Column>
            }
          >
            {(c) => (
              <Each of={[c().id]} key={(x) => x}>
                {() => <ConnectionDetails connection={c()} endpointFeedback={currentFeedback()} setEndpointFeedback={endpointFeedback.set} />}
              </Each>
            )}
          </If>
        }
      >
        <TokenReveal />
      </If>
    </PageLayout>
  );
}

type EndpointFeedback = {
  id: string; message: string; tone: "success" | "warning" | "danger"; detail?: string;
  action?: { label: string; run: () => void; busy?: () => boolean };
};

function endpointCheckFeedback(id: string): EndpointFeedback | null {
  const check = endpointChecks()[id];
  if (!check) return null;
  switch (check.phase) {
    case "unknown":
    case "reload-failed":
    case "not-settled": return {
      id, tone: "warning", detail: check.detail,
      message: check.phase === "reload-failed"
        ? "The reload didn't confirm the current endpoint. Try reloading again."
        : check.refusal === "conflict"
          ? "The connection changed while you were saving. Your change was not saved. Reload the connection and review it before making a new change."
          : check.refusal === "missing"
            ? "The connection was not available to edit. Your change was not saved. Reload the connection."
            : check.phase === "not-settled"
              ? "The server hasn't reported the proposed endpoint. The first test may still be running. Reload again; Metro won't send another change while this one is unresolved."
              : "Metro couldn't confirm the endpoint change. Reload the connection before changing it again.",
      action: { label: "Reload connection", run: () => void reloadEndpoint(id), busy: () => providerBusy() === `endpoint-reload:${id}` },
    };
    case "saved": {
      const connection = connections()?.find((c) => c.id === id);
      return { id, tone: "success", message: connection && providerById(connection.providerId)?.listsModels
        ? "The server confirmed the new endpoint was saved. Use Refresh models to update the list."
        : "The server confirmed the new endpoint was saved." };
    }
    case "review":
      return { id, tone: "warning", message: "Connection reloaded. Your previous change was not saved. Review the current endpoint before making a new change." };
  }
}

/** Errors and what the last change did, in the page's fixed footer. Technical text only on request. */
function Messages(props: { endpoint: EndpointFeedback | null }) {
  const technical = signal(false);
  return (
    <Column gap={2}>
      <If when={props.endpoint}>
        {(result) => <Banner variant="inline" tone={result().tone} testID="endpoint-feedback" message={result().message}
          action={result().action ? <Action label={result().action!.label} busyLabel="Reloading…" busy={result().action!.busy?.()}
            onPress={() => result().action?.run()} /> : undefined} />}
      </If>
      <If when={catalogError()}>{(message) => <Banner variant="inline" tone="danger" testID="catalog-error" message={message()}
        action={<Action label="Try again" onPress={() => void loadProviders()} />} />}</If>
      <If when={connectionsError()}>
        {(message) => (
          <Column gap={1}>
            <Banner variant="inline" tone="danger" testID="providers-error" message={message()}
              action={!connections() ? <Action label="Try again" onPress={() => void loadProviders()} /> : undefined} />
            <If when={connectionsErrorDetail()}>
              {(detail) => (
                <Disclosure testID="providers-error-technical" title="Technical details" open={technical()} onToggle={technical.set}>
                  <Column selectable>
                    <Label variant="mono" color="muted">
                      {detail()}
                    </Label>
                  </Column>
                </Disclosure>
              )}
            </If>
          </Column>
        )}
      </If>
      <If when={providerNotice()}>{(message) => <Banner variant="inline" tone="success" testID="providers-notice" message={message()} />}</If>
    </Column>
  );
}

// ------------------------------------------------------------------ status, problems and actions

/** "chat", "System One", "embeddings" inside a sentence. */
const capabilityWord = (c: Capability) => (c === "chat" ? "chat" : c === "system_one" ? CAPABILITY_LABEL[c] : CAPABILITY_LABEL[c].toLowerCase());

/** What a capability's group is for, under its heading. */
const GROUP_PURPOSE: Record<Capability, string> = {
  chat: "Answers and tool turns of the runs you start.",
  system_one: "Typed decisions: routing, opt-in classification and search rerank.",
  embeddings: "Indexing and searching the messages you can read.",
};

const shareStateOf = (c: ProviderConnection): Exclude<ShareVisibility, "personal"> | null => {
  const v = c.visibility ?? shareOf(c.id);
  return v && v !== "personal" ? v : null;
};

const enabledCount = (c: ProviderConnection) => c.models.filter((m) => m.enabled).length;
const countText = (n: number, noun: string) => `${n} ${noun}${n === 1 ? "" : "s"}`;

type DeviceView = "checking" | "unknown" | "unpaired" | "online" | "offline";
function deviceView(c: ProviderConnection): DeviceView {
  const st = deviceStatuses()[c.id];
  if (!st) return "checking";
  if ("error" in st) return "unknown";
  if (!st.paired) return "unpaired";
  return st.online ? "online" : "offline";
}

/** Where a device connection is served from, in two words. */
function servedFrom(c: ProviderConnection): string {
  if (!packaged()) return "metro-connector";
  return localConnections()[c.id] ? "this computer" : "another computer";
}

/** The row's one status line: a dot and a short text. */
export function connectionStatus(c: ProviderConnection): { tone: Tone; text: string } {
  const parts: string[] = [];
  let tone: Tone = "neutral";
  if (isDeviceConnection(c)) {
    const v = deviceView(c);
    if (v === "checking") parts.push("Checking…");
    else if (v === "unknown") (tone = "warning"), parts.push("Status unknown");
    else if (v === "unpaired") parts.push(packaged() ? "Not paired on this computer" : "Not paired");
    else if (v === "online") (tone = "success"), parts.push(`Online · ${servedFrom(c)}`);
    else (tone = "warning"), parts.push(`Offline · ${servedFrom(c)}`);
  } else {
    const p = providerById(c.providerId);
    if (p?.requiresKey && !c.hasKey) (tone = "warning"), parts.push("No key");
    else (tone = "success"), parts.push(c.hasKey ? "Key saved" : "Ready");
  }
  const n = enabledCount(c);
  if (n > 0) parts.push(n === 1 ? defaultModelName(c) ?? countText(n, "model") : countText(n, "model"));
  else if (c.model) parts.push(defaultModelName(c) ?? c.model);
  else parts.push("No models yet");
  const shared = shareStateOf(c);
  if (shared) parts.push(SHARE_TEXT[shared]);
  return { tone, text: parts.join(" · ") };
}

type Problem = { text: string; tone: Tone; fix?: { label: string; busy?: boolean; run: () => void } };

/** What, if anything, keeps a connection from working: one plain sentence and at most one fix. */
export function connectionProblem(c: ProviderConnection): Problem | null {
  const listing = modelListings()[c.id];
  const device = isDeviceConnection(c);
  if (device) {
    const v = deviceView(c);
    if (v === "unpaired") {
      return packaged()
        ? { text: "This device isn't set up here yet.", tone: "warning", fix: { label: "Set up here", run: () => void rotateDeviceToken(c.id) } }
        : { text: "No device is paired yet.", tone: "warning", fix: { label: "New device token", run: () => void rotateDeviceToken(c.id) } };
    }
    if (v === "offline") {
      const tail = c.isDefault && connectionCapability(c) === "chat" ? " Runs you start fail until it's back; Metro never switches to another connection." : "";
      if (packaged() && localConnections()[c.id]) return { text: `This computer isn't serving it right now.${tail}`, tone: "warning", fix: { label: "Open setup", run: () => void rotateDeviceToken(c.id) } };
      return { text: packaged() ? `The computer it's paired with is offline.${tail}` : `metro-connector isn't running. Start it with metro-connector run.${tail}`, tone: "warning" };
    }
  }
  if (listing?.state === "error") return { text: listing.message, tone: listing.offline ? "warning" : "danger", fix: { label: "Try again", run: () => void fetchModels(c.id) } };
  const p = providerById(c.providerId);
  if (!device && p?.requiresKey && !c.hasKey) return { text: "It needs an API key.", tone: "warning", fix: { label: "Add key", run: () => openDetails(c.id) } };
  return null;
}

/** The ⋯ menu of a connection, in the list and on its details. */
function menuItems(c: ProviderConnection, inDetails: boolean): MoreMenuItem[] {
  const cap = connectionCapability(c);
  const device = isDeviceConnection(c);
  const provider = providerById(c.providerId);
  const busy = !!providerBusy();
  const st = deviceStatuses()[c.id];
  const paired = !!st && !("error" in st) && st.paired;
  const items: MoreMenuItem[] = [];
  if (!c.isDefault) {
    items.push({
      id: `connection-make-default:${c.id}`,
      label: `Make default for ${capabilityWord(cap)}`,
      disabled: busy,
      onPress: () =>
        void updateConnection(
          c.id,
          { is_default: true },
          `“${c.name}” is now your default ${capabilityWord(cap)} connection, for ${CAPABILITY_USE[cap]}. Your defaults for other capabilities are unchanged.`,
        ),
    });
  }
  if (device || provider?.listsModels !== false) {
    items.push({ id: `connection-models:${c.id}`, label: device ? "Test connection" : "Refresh models", disabled: modelListings()[c.id]?.state === "loading", onPress: () => void fetchModels(c.id) });
  }
  items.push({
    id: `connection-sharing:${c.id}`,
    label: "Sharing…",
    disabled: busy,
    onPress: () => {
      openDetails(c.id);
      void openSharing(c.id);
    },
  });
  if (!inDetails) items.push({ id: `connection-details-open:${c.id}`, label: "Details…", onPress: () => openDetails(c.id) });
  if (device && paired) {
    items.push({
      id: `connection-unpair:${c.id}`,
      label: "Unpair…",
      destructive: true,
      disabled: busy,
      onPress: () => {
        openDetails(c.id);
        deviceConfirmUnpair.set(c.id);
      },
    });
  }
  items.push({
    id: `connection-delete:${c.id}`,
    label: "Delete…",
    destructive: true,
    disabled: busy,
    onPress: () => {
      openDetails(c.id);
      providerConfirmDelete.set(c.id);
    },
  });
  return items;
}

const markOf = (c: ProviderConnection) => vendorOf(providerById(c.providerId) ?? { id: c.providerId, name: c.providerId }).mark;

// ------------------------------------------------------------------ the list

/** The user's connections by capability, in the server's order; capabilities with none are left out. */
function capabilityGroups(list: readonly ProviderConnection[]): { capability: Capability; items: ProviderConnection[] }[] {
  return (["chat", "system_one", "embeddings"] as const)
    .map((capability) => ({ capability, items: list.filter((c) => connectionCapability(c) === capability) }))
    .filter((g) => g.items.length > 0);
}

function ConnectionList() {
  return (
    <Column gap={3} testID="connection-list">
      <If when={connections()} else={
        <If when={!connectionsError()}>
          <SettingsGroup><SettingRow label="Loading your connections…" /></SettingsGroup>
        </If>
      }>
        {(list) => (
          <Column gap={6}>
            <If when={list().length === 0 && !needsChatModel()}>
              <SettingsGroup testID="connections-empty">
                <SettingRow label="No connections yet" description="Until you add one, Ask agent uses what the server operator configured, if anything." />
              </SettingsGroup>
            </If>
            <Each of={capabilityGroups(list())} key={(g) => g.capability}>
              {(g) => (
                <SettingsGroup title={CAPABILITY_LABEL[g().capability]} description={GROUP_PURPOSE[g().capability]} testID={`connection-group:${g().capability}`}>
                  <Each of={g().items} key={(c) => c.id}>
                    {(c) => <ConnectionRow connection={c()} />}
                  </Each>
                </SettingsGroup>
              )}
            </Each>
          </Column>
        )}
      </If>
    </Column>
  );
}

/** One saved connection: one compact row, its ⋯ menu, and at most one problem with its fix. */
function ConnectionRow(props: { connection: ProviderConnection }) {
  const c = () => props.connection;
  return (
    <ConnectionItem
      testID={`connection:${c().id}`}
      mark={markOf(c())}
      name={c().name}
      isDefault={c().isDefault}
      status={connectionStatus(c())}
      onPress={() => openDetails(c().id)}
      trailing={<MoreMenu testID={`connection-menu:${c().id}`} accessibilityLabel={`More for ${c().name}`} items={menuItems(c(), false)} />}
    >
      <If when={connectionProblem(c())}>
        {(p) => (
          <ProblemNote
            testID={`connection-problem:${c().id}`}
            text={p().text}
            tone={p().tone}
            action={p().fix ? <Action compact testID={`connection-fix:${c().id}`} label={p().fix!.label} onPress={p().fix!.run} /> : undefined}
          />
        )}
      </If>
    </ConnectionItem>
  );
}

// ------------------------------------------------------------------ details

function ConnectionDetails(props: {
  connection: ProviderConnection;
  endpointFeedback: EndpointFeedback | null;
  setEndpointFeedback: (value: EndpointFeedback | null) => void;
}) {
  const c = () => props.connection;
  const device = () => isDeviceConnection(c());
  const provider = () => providerById(c().providerId);
  const technical = signal(false);
  const back = () => {
    if (sharingOpen() === c().id) closeSharing();
    providerConfirmDelete.set(null);
    deviceConfirmUnpair.set(null);
    props.setEndpointFeedback(null);
    detailsOf.set(null);
  };
  const how = () => {
    const p = provider();
    if (!p) return c().providerId;
    const d = describePath(p, { packaged: packaged() });
    return `${vendorOf(p).name} · ${d.method}${d.variant ? ` (${d.variant})` : ""}`;
  };
  const listingDetail = () => {
    const l = modelListings()[c().id];
    return l?.state === "error" ? l.detail ?? null : null;
  };
  return (
    <Column gap={6} testID={`connection-details:${c().id}`}>
      <Column gap={3}>
        <BackLink testID="connection-back" label="Model connections" onPress={back} />
        <ConnectionHeading testID={`connection-heading:${c().id}`} mark={markOf(c())} name={c().name} isDefault={c().isDefault} status={connectionStatus(c())}>
          <MoreMenu testID={`connection-menu:${c().id}`} accessibilityLabel={`More for ${c().name}`} items={menuItems(c(), true)} />
        </ConnectionHeading>
        <Label variant="caption" color="muted">
          {`${how()} · ${CAPABILITY_LABEL[connectionCapability(c())]}`}
        </Label>
        <If when={settingsAccountScope()}>{(scope) => <ScopeLine scope={scope()} />}</If>
        <If when={connectionProblem(c())}>
          {(p) => (
            <ProblemNote
              tone={p().tone}
              testID={`connection-problem:${c().id}`}
              text={p().text}
              action={p().fix ? <Action compact testID={`connection-fix:${c().id}`} label={p().fix!.label} onPress={p().fix!.run} /> : undefined}
            />
          )}
        </If>
      </Column>

      <ModelsSection connection={c()} />
      <If when={device()}>
        <DeviceSection connection={c()} />
      </If>
      <If when={!device() && provider()?.allowCustomUrl}>
        <EndpointSection connection={c()} feedback={props.setEndpointFeedback} />
      </If>
      <If when={!device() && (provider()?.requiresKey || c().hasKey)}>
        <KeySection connection={c()} />
      </If>
      <SharingSection connection={c()} />
      <DangerZone connection={c()} />

      <Disclosure testID={`technical:${c().id}`} title="Technical details" open={technical()} onToggle={technical.set}>
        <FactList
          facts={[
            { label: "Provider", value: c().providerId },
            ...(!provider()?.allowCustomUrl && c().baseUrl ? [{ label: "Endpoint", value: c().baseUrl! }] : []),
            { label: "Connection ID", value: c().id },
            ...(listingDetail() ? [{ label: "Last model listing", value: listingDetail()! }] : []),
            ...(connectionsErrorDetail() ? [{ label: "Last error", value: connectionsErrorDetail()! }] : []),
            ...(props.endpointFeedback?.detail ? [{ label: "Last endpoint change", value: props.endpointFeedback.detail }] : []),
          ]}
        />
      </Disclosure>
    </Column>
  );
}

const ENDPOINT_NOT_APPROVED = "That endpoint isn't on the server's approved list. Pick an approved URL exactly, or ask the operator to add it.";

function EndpointSection(props: { connection: ProviderConnection; feedback: (value: EndpointFeedback | null) => void }) {
  const c = () => props.connection;
  const id = c().id;
  const approved = () => catalog()?.allowedBaseUrls ?? [];
  const current = () => c().baseUrl ?? providerById(c().providerId)?.defaultBaseUrl ?? null;
  const canChange = () => !!c().endpointRevision && !catalogError() && approved().some((url) => url !== current());
  const check = () => endpointNeedsReload(id);
  const busy = () => providerBusy() === `endpoint:${id}` || providerBusy() === `endpoint-refresh:${id}`;
  const editing = signal(check());
  const value = signal(endpointChecks()[id]?.to ?? "");
  const revision = signal<string | null>(null);
  const error = signal<string | null>(null);
  let mounted = true;
  cleanup(() => { mounted = false; });
  const change = () => {
    const displayed = c().endpointRevision;
    if (check() || !displayed) return;
    dismissEndpointCheck(id);
    value.set(current() ?? "");
    revision.set(displayed);
    error.set(null);
    props.feedback(null);
    editing.set(true);
  };
  const edit = (url: string) => {
    dismissEndpointCheck(id);
    value.set(url);
    error.set(null);
    props.feedback(null);
  };
  const submit = async () => {
    const expected = revision();
    if (!mounted || !editing() || !expected || providerBusy() || check()) return;
    const url = value();
    props.feedback(null);
    error.set(null);
    if (!isAllowedBaseUrl(url)) {
      error.set(ENDPOINT_NOT_APPROVED);
      return;
    }
    const outcome = await updateEndpoint(id, url, expected);
    if (!mounted) return;
    if (outcome.kind === "not-approved") {
      error.set(ENDPOINT_NOT_APPROVED);
    } else if (outcome.kind === "refused") {
      props.feedback({
        id, tone: "danger", message: outcome.message, detail: outcome.detail,
        action: outcome.retry ? { label: "Try again", run: () => void submit() } : undefined,
      });
    } else if (outcome.kind === "changed") {
      editing.set(false);
      const listing = outcome.listing;
      const refreshed = listing?.state === "ok";
      const refreshFailed = listing?.state === "error";
      props.feedback({
        id,
        tone: refreshFailed ? "warning" : "success",
        message: refreshed
          ? `Endpoint changed. Models listed again: ${refreshText(listing)}`
          : refreshFailed
            ? "Endpoint changed. Metro couldn't refresh the model list; use Refresh models to try again."
            : "Endpoint changed.",
      });
    }
  };
  effect(() => {
    const result = endpointChecks()[id];
    if (result?.phase === "saved" || result?.phase === "review") {
      value.set("");
      editing.set(false);
    }
  });
  return (
    <SettingsGroup title="Endpoint" description="Only endpoints the operator approved. Metro tests the new one before it switches." testID={`endpoint-section:${id}`}>
      <SettingRow label={check() ? "Last known URL" : "Current URL"} stacked>
        <Row align="center" gap={3} wrap>
          <Column selectable grow testID={`endpoint-current:${id}`}>
            <Label variant="mono">{current() ?? "No endpoint set"}</Label>
          </Column>
          <If when={!editing()}>
            <Action label="Change…" testID={`endpoint-change:${id}`} disabled={!canChange() || !!providerBusy() || !!check()} onPress={change} />
          </If>
        </Row>
        <If when={!editing() && !canChange() && !catalogError()}>
          <Label variant="caption" color="muted">{c().endpointRevision
            ? "No other approved endpoints. Ask your server operator to add one."
            : "This server doesn't support guarded endpoint changes."}</Label>
        </If>
      </SettingRow>
      <If when={editing()}>
        <If when={(c().visibility ?? shareOf(id)) === "spaces" || (c().visibility ?? shareOf(id)) === "organization"}>
          <Banner variant="inline" tone="warning" testID={`endpoint-shared:${id}`} message="People you share this with will send their requests to the new endpoint." />
        </If>
        <SettingRow label="New URL" stacked>
          <Column gap={2}>
            <Row gap={2} wrap>
              <Each of={approved()} key={(url) => url}>
                {(url) => <Chip label={url()} selected={value() === url()} disabled={!!providerBusy() || !!check()} onPress={() => edit(url())} />}
              </Each>
            </Row>
            <Field testID={`endpoint-input:${id}`} accessibilityLabel="Approved endpoint URL" value={value()} onChange={edit} disabled={!!providerBusy() || !!check()} />
            <If when={error()}>
              {(message) => <ProblemNote tone="danger" testID={`endpoint-error:${id}`} text={message()} />}
            </If>
            <Label>The test sends this connection’s API key, if configured, to the new URL before saving. The endpoint can receive the key even if the save fails. Only test an endpoint you trust with the key.</Label>
            <ActionBar>
              <Action label="Cancel" variant="ghost" disabled={busy() || check()} onPress={() => { dismissEndpointCheck(id); editing.set(false); props.feedback(null); }} />
              <Action label="Test and save" variant="primary" testID={`endpoint-save:${id}`} busy={busy()}
                busyLabel={providerBusy() === `endpoint-refresh:${id}` ? "Refreshing models…" : "Testing and saving…"}
                disabled={!revision() || !value() || value() === current() || !!check() || catalogError() !== null || (!!providerBusy() && !busy())} onPress={() => void submit()} />
            </ActionBar>
          </Column>
        </SettingRow>
      </If>
    </SettingsGroup>
  );
}

/** "+2 new, 1 no longer listed", "Up to date", after a refresh. */
function refreshText(l: { models: string[]; added?: number; unavailable?: number }): string {
  const parts: string[] = [];
  if (l.added) parts.push(`+${l.added} new`);
  if (l.unavailable) parts.push(`${l.unavailable} no longer listed`);
  if (parts.length) return `${parts.join(", ")}.`;
  return l.models.length ? `Up to date: ${countText(l.models.length, "model")} listed.` : "The provider listed no models. Add one by hand.";
}

function ModelsSection(props: { connection: ProviderConnection }) {
  const c = () => props.connection;
  const device = () => isDeviceConnection(c());
  const lists = () => device() || providerById(c().providerId)?.listsModels !== false;
  const listing = () => modelListings()[c().id];
  const models = () => connectionModels(c());
  const addOpen = signal(false);
  return (
    <SettingsGroup
      title="Models"
      description={lists() ? "Every model the provider lists is on; switch off the ones you don't want offered." : "The models you added by hand."}
      testID={`models-section:${c().id}`}
    >
      <If when={models().length === 0 && listing()?.state !== "loading"}>
        <SettingRow label={lists() ? "No models yet" : "No models yet: add the one you want"} description={device() ? "They're listed once its device is online." : undefined} />
      </If>
      <Each of={models()} key={(m) => m.modelId}>
        {(m) => <ModelRow connection={c()} model={m()} />}
      </Each>
      <Row padX={4} padY={3} gap={2} align="center" wrap>
        <If when={lists()}>
          <Action
            testID={`models-refresh:${c().id}`}
            label="Refresh models"
            busyLabel="Listing…"
            busy={listing()?.state === "loading"}
            disabled={!!providerBusy() || endpointNeedsReload(c().id)}
            compact
            onPress={() => void fetchModels(c().id)}
          />
        </If>
        <Action testID={`model-add-open:${c().id}`} label="Add model" icon="plus" variant="ghost" compact expanded={addOpen()} onPress={() => addOpen.set(!addOpen())} />
        <Column grow>
          <If when={listing()?.state === "ok" ? (listing() as { models: string[]; added?: number; unavailable?: number }) : null}>
            {(l) => (
              <Label variant="caption" color="muted" testID={`models-refresh-result:${c().id}`}>
                {refreshText(l())}
              </Label>
            )}
          </If>
        </Column>
      </Row>
      <If when={addOpen()}>
        <Column padX={4} padY={3}>
          <ModelDraftForm
            testID={`model-add:${c().id}`}
            thinking={c().customThinking}
            submitLabel="Add model"
            busy={providerBusy() === `model:${c().id}`}
            onCancel={() => addOpen.set(false)}
            onSubmit={async (draft) => {
              const ok = await addConnectionModel(c().id, draft);
              if (ok) addOpen.set(false);
              return ok;
            }}
          />
        </Column>
      </If>
    </SettingsGroup>
  );
}

function ModelRow(props: { connection: ProviderConnection; model: ConnectionModel }) {
  const c = () => props.connection;
  const m = () => props.model;
  const name = signal(m().customName ?? "");
  /** The name is being edited in place (pencil or a press on it): Return saves, Escape cancels. */
  const editingName = signal(false);
  const editingThinking = signal(false);
  const busy = () => !!providerBusy();
  const key = () => `${c().id}:${m().modelId}`;
  const changed = () => name().trim() !== (m().customName ?? "");
  const startEditName = () => {
    name.set(m().customName ?? "");
    editingName.set(true);
  };
  const cancelName = () => {
    name.set(m().customName ?? "");
    editingName.set(false);
  };
  const saveName = async () => {
    if (!changed()) {
      editingName.set(false);
      return;
    }
    const ok = await patchConnectionModel(c().id, { model_id: m().modelId, display_name: name().trim() || null });
    if (ok) editingName.set(false);
  };
  const thinkingText = () => {
    const t = m().thinking;
    return t && t.efforts.length ? `Thinking: ${t.efforts.join(" · ")}` : m().reasoningEfforts.length ? `Thinking: ${m().reasoningEfforts.join(" · ")}` : null;
  };
  return (
    <Column>
      <ModelItem
        testID={`model-row:${key()}`}
        enabled={m().enabled}
        modelId={m().modelId}
        control={
          <Toggle
            testID={`model-enabled:${key()}`}
            label={m().isDefault ? `${m().displayName} is the default model and stays on` : `Offer ${m().displayName}`}
            checked={m().enabled}
            disabled={busy() || m().isDefault}
            onChange={(on) => void patchConnectionModel(c().id, { model_id: m().modelId, enabled: on })}
          />
        }
        name={
          <If
            when={editingName()}
            else={
              <EditableName
                testID={`model-name-edit:${key()}`}
                name={m().displayName}
                accessibilityLabel={`Rename ${m().displayName} (${m().modelId})`}
                disabled={busy()}
                onEdit={startEditName}
              />
            }
          >
            <Row gap={1} align="center">
              <Field
                testID={`model-name:${key()}`}
                accessibilityLabel={`Name shown for ${m().modelId}. Return saves, Escape cancels; empty uses the listed name`}
                value={name()}
                onChange={name.set}
                placeholder={m().listedName ?? m().modelId}
                maxLength={128}
                grow
                autoFocus
                onSubmit={() => void saveName()}
                onCancel={cancelName}
              />
              <Action testID={`model-name-save:${key()}`} label="Save" compact disabled={busy()} onPress={() => void saveName()} />
              <Action testID={`model-name-cancel:${key()}`} label="Cancel" variant="ghost" compact onPress={cancelName} />
            </Row>
          </If>
        }
        tags={
          <>
            <If when={m().isDefault}>
              <Badge testID={`model-default-tag:${key()}`} tone="neutral" variant="outline" label="Default" />
            </If>
            <If when={!m().available}>
              <Badge testID={`model-unavailable:${key()}`} tone="neutral" variant="outline" label="Unavailable" />
            </If>
            <If when={thinkingText()}>
              {(t) => (
                <Label variant="caption" color="muted">
                  {t()}
                </Label>
              )}
            </If>
          </>
        }
        actions={
          <>
            <If when={!m().isDefault && m().enabled}>
              <Action
                testID={`model-default:${key()}`}
                label="Make default"
                variant="ghost"
                compact
                disabled={busy()}
                onPress={() => void patchConnectionModel(c().id, { model_id: m().modelId, default: true }, `${m().displayName} is now the default model of “${c().name}”.`)}
              />
            </If>
            <If when={c().customThinking}>
              <Action testID={`model-thinking:${key()}`} label="Thinking…" variant="ghost" compact expanded={editingThinking()} onPress={() => editingThinking.set(!editingThinking())} />
            </If>
            <If when={m().source === "manual" && !m().isDefault}>
              <Action
                testID={`model-remove:${key()}`}
                label="Remove"
                variant="destructive"
                compact
                accessibilityLabel={`Remove ${m().displayName}`}
                disabled={busy()}
                onPress={() => void removeConnectionModel(c().id, m().modelId)}
              />
            </If>
          </>
        }
      />
      <If when={editingThinking()}>
        <Column padX={4} padY={2}>
          <ThinkingForm
            testID={`model-thinking-form:${key()}`}
            initial={m().thinking}
            busy={providerBusy() === `model:${c().id}`}
            onCancel={() => editingThinking.set(false)}
            onSave={async (t) => {
              const ok = await patchConnectionModel(c().id, { model_id: m().modelId, thinking: t.efforts.length && t.wire !== "none" ? t : null }, `Saved the thinking options of ${m().displayName}.`);
              if (ok) editingThinking.set(false);
            }}
          />
        </Column>
      </If>
    </Column>
  );
}

const WIRE_OPTIONS = [
  { value: "none", label: "Don't send" },
  { value: "reasoning_effort", label: "reasoning_effort" },
  { value: "reasoning", label: "reasoning.effort" },
] as const;

const WIRE_HELP: Record<ModelThinking["wire"], string> = {
  none: "No thinking level is sent: the model uses its own setting.",
  reasoning_effort: "Sends the chosen level as \"reasoning_effort\": \"<level>\" at the top level of the request. Check which field your server documents.",
  reasoning: "Sends the chosen level as \"reasoning\": { \"effort\": \"<level>\" }. Check which field your server documents.",
};

/** Lowercase letters, digits, _ and -, at most 32: what the server accepts as a thinking level. */
const cleanEffort = (v: string) => v.trim().toLowerCase();
const effortValid = (v: string) => /^[a-z0-9_-]{1,32}$/.test(v);

/**
 * A self-hosted model's thinking options: the levels a turn may pick (chips
 * you add and remove) and how the chosen one is sent, in plain words.
 */
function ThinkingFields(props: { testID: string; value: ModelThinking; onChange: (t: ModelThinking) => void }) {
  const next = signal("");
  const error = signal<string | null>(null);
  const add = () => {
    const v = cleanEffort(next());
    if (!v) return;
    if (!effortValid(v)) return error.set("Use lowercase letters, digits, _ or -, at most 32.");
    if (props.value.efforts.includes(v)) return error.set("That level is already there.");
    if (props.value.efforts.length >= 8) return error.set("A model offers at most 8 levels.");
    error.set(null);
    next.set("");
    props.onChange({ efforts: [...props.value.efforts, v], wire: props.value.wire === "none" ? "reasoning_effort" : props.value.wire });
  };
  return (
    <Column gap={2} testID={props.testID}>
      <Label variant="caption" color="muted">
        Thinking levels a turn can pick (such as low, medium, high). Press a level to remove it.
      </Label>
      <Row gap={1} wrap align="center">
        <Each of={props.value.efforts} key={(e) => e}>
          {(e) => (
            <Chip
              testID={`${props.testID}-effort:${e()}`}
              label={`${e()} ×`}
              selected
              onPress={() => {
                const efforts = props.value.efforts.filter((x) => x !== e());
                props.onChange({ efforts, wire: efforts.length ? props.value.wire : "none" });
              }}
            />
          )}
        </Each>
        <Field testID={`${props.testID}-effort-input`} accessibilityLabel="Add a thinking level" value={next()} onChange={next.set} placeholder="Add a level" maxLength={32} onSubmit={add} keepFocusOnSubmit />
        <Action testID={`${props.testID}-effort-add`} label="Add" compact disabled={!next().trim()} onPress={add} />
      </Row>
      <If when={error()}>
        {(e) => (
          <Label variant="caption" color="danger">
            {e()}
          </Label>
        )}
      </If>
      <If when={props.value.efforts.length > 0}>
        <Column gap={1}>
          <Label variant="caption" color="muted">
            How the chosen level is sent
          </Label>
          <SegmentedControl
            testID={`${props.testID}-wire`}
            accessibilityLabel="How the chosen thinking level is sent"
            options={WIRE_OPTIONS.filter((o) => o.value !== "none")}
            value={props.value.wire === "none" ? "reasoning_effort" : props.value.wire}
            onChange={(wire) => props.onChange({ ...props.value, wire })}
          />
          <Label variant="caption" color="muted">
            {WIRE_HELP[props.value.wire === "none" ? "reasoning_effort" : props.value.wire]}
          </Label>
        </Column>
      </If>
      <If when={props.value.efforts.length === 0}>
        <Label variant="caption" color="muted">
          {WIRE_HELP.none}
        </Label>
      </If>
    </Column>
  );
}

function ThinkingForm(props: { testID: string; initial: ModelThinking | null; busy: boolean; onSave: (t: ModelThinking) => void; onCancel: () => void }) {
  const value = signal<ModelThinking>(props.initial ?? { efforts: [], wire: "none" });
  return (
    <Column gap={2}>
      <ThinkingFields testID={props.testID} value={value()} onChange={value.set} />
      <ActionBar>
        <Action label="Cancel" variant="ghost" compact onPress={props.onCancel} />
        <Action testID={`${props.testID}-save`} label="Save thinking" busyLabel="Saving…" busy={props.busy} variant="primary" compact onPress={() => props.onSave(value())} />
      </ActionBar>
    </Column>
  );
}

/**
 * One model to add by hand: its id as the provider documents it, the name
 * pickers show (optional) and, for self-hosted endpoints, its thinking options.
 */
function ModelDraftForm(props: {
  testID: string;
  thinking: boolean;
  submitLabel: string;
  busy?: boolean;
  onSubmit: (draft: NewConnectionModel) => Promise<boolean> | boolean;
  onCancel?: () => void;
}) {
  const id = signal("");
  const name = signal("");
  const thinking = signal<ModelThinking>({ efforts: [], wire: "none" });
  const showThinking = signal(false);
  const submit = async () => {
    const model_id = id().trim();
    if (!model_id) return;
    const draft: NewConnectionModel = { model_id };
    if (name().trim()) draft.display_name = name().trim();
    const t = thinking();
    if (t.efforts.length && t.wire !== "none") draft.thinking = t;
    if (await props.onSubmit(draft)) {
      id.set("");
      name.set("");
      thinking.set({ efforts: [], wire: "none" });
      showThinking.set(false);
    }
  };
  return (
    <Column gap={2} testID={props.testID}>
      <Row gap={2} wrap>
        <Field testID={`${props.testID}-id`} accessibilityLabel="Model ID" value={id()} onChange={id.set} placeholder="Model ID, as the provider documents it" maxLength={200} grow onSubmit={() => void submit()} />
        <Field testID={`${props.testID}-name`} accessibilityLabel="Name shown in pickers (optional)" value={name()} onChange={name.set} placeholder="Name shown (optional)" maxLength={128} grow onSubmit={() => void submit()} />
      </Row>
      <If when={props.thinking}>
        <Disclosure testID={`${props.testID}-thinking`} title="Thinking options" summary="Optional" open={showThinking()} onToggle={showThinking.set}>
          <ThinkingFields testID={`${props.testID}-thinking-fields`} value={thinking()} onChange={thinking.set} />
        </Disclosure>
      </If>
      <ActionBar>
        <If when={props.onCancel}>
          <Action label="Cancel" variant="ghost" compact onPress={() => props.onCancel?.()} />
        </If>
        <Action testID={`${props.testID}-save`} label={props.submitLabel} busyLabel="Adding…" busy={props.busy} compact disabled={!id().trim()} onPress={() => void submit()} />
      </ActionBar>
    </Column>
  );
}

function DeviceSection(props: { connection: ProviderConnection }) {
  const c = () => props.connection;
  const v = () => deviceView(c());
  const listing = () => modelListings()[c().id];
  const busy = () => providerBusy();
  return (
    <SettingsGroup title="Device" description="Runs on your own computer, with your own sign-in or key. The server never receives them." testID={`device-section:${c().id}`}>
      <SettingRow label="Status" description={deviceStatusText(c().id)} testID={`device-status-text:${c().id}`} />
      <If when={v() === "online" || v() === "offline"}>
        <SettingRow label="Served from" description={servedFrom(c()) === "metro-connector" ? "Your metro-connector" : servedFrom(c()) === "this computer" ? "This computer" : "Another computer"} />
      </If>
      <Row padX={4} padY={3} gap={2} wrap>
        <Action
          testID={`connection-test:${c().id}`}
          label="Test connection"
          busyLabel="Testing…"
          busy={listing()?.state === "loading"}
          compact
          onPress={() => void fetchModels(c().id)}
        />
        <Action
          testID={`connection-token:${c().id}`}
          label={packaged() ? (localConnections()[c().id] ? "This computer…" : "Set up here") : "New device token"}
          busyLabel={packaged() ? "Opening…" : "Issuing…"}
          busy={busy() === `token:${c().id}`}
          disabled={!!busy()}
          compact
          onPress={() => void rotateDeviceToken(c().id)}
        />
      </Row>
    </SettingsGroup>
  );
}

function KeySection(props: { connection: ProviderConnection }) {
  const c = () => props.connection;
  const replacing = signal(false);
  const newKey = signal("");
  const busy = () => providerBusy();
  const saveKey = async () => {
    const key = newKey();
    newKey.set(""); // never keep it, whatever the outcome
    if (!key.trim()) return;
    if (await updateConnection(c().id, { api_key: key.trim() }, `Replaced the key of “${c().name}”. It is never shown again.`)) replacing.set(false);
  };
  return (
    <SettingsGroup title="Key" testID={`key-section:${c().id}`}>
      <SettingRow label={c().hasKey ? "Key saved" : "No key yet"} description="Stored encrypted on the server · never shown again">
        <Action
          testID={`connection-replace-key:${c().id}`}
          label={replacing() ? "Cancel" : c().hasKey ? "Replace key…" : "Add key…"}
          variant="ghost"
          compact
          onPress={() => {
            newKey.set("");
            replacing.set(!replacing());
          }}
        />
      </SettingRow>
      <If when={replacing()}>
        <Row padX={4} padY={3} gap={2} align="center">
          <Field
            testID={`connection-key-input:${c().id}`}
            accessibilityLabel={`New API key for ${c().name}`}
            value={newKey()}
            onChange={newKey.set}
            placeholder="New API key"
            masked
            grow
            onSubmit={() => void saveKey()}
          />
          <Action testID={`connection-key-save:${c().id}`} label="Save key" busyLabel="Saving…" busy={busy() === `patch:${c().id}`} variant="primary" onPress={() => void saveKey()} />
        </Row>
      </If>
    </SettingsGroup>
  );
}

function SharingSection(props: { connection: ProviderConnection }) {
  const c = () => props.connection;
  const summary = () => {
    const v = shareStateOf(c());
    if (!v) return "Only you can use it.";
    return v === "organization" ? "Everyone signed in to this server can use it. You see their requests and may pay for them." : "Members of the Spaces you chose can use it there. You see their requests and may pay for them.";
  };
  return (
    <SettingsGroup title="Sharing" testID={`sharing-section:${c().id}`}>
      <If
        when={sharingOpen() === c().id}
        else={
          <SettingRow label={shareStateOf(c()) ? SHARE_TEXT[shareStateOf(c())!] : "Private"} description={summary()}>
            <Action testID={`connection-sharing-open:${c().id}`} label="Manage sharing…" compact disabled={!!providerBusy()} onPress={() => void openSharing(c().id)} />
          </SettingRow>
        }
      >
        <Column padX={4} padY={3}>
          <SharingPanel connection={c()} />
        </Column>
      </If>
    </SettingsGroup>
  );
}

function DangerZone(props: { connection: ProviderConnection }) {
  const c = () => props.connection;
  const device = () => isDeviceConnection(c());
  const busy = () => providerBusy();
  const paired = () => {
    const st = deviceStatuses()[c().id];
    return !!st && !("error" in st) && st.paired;
  };
  return (
    <SettingsGroup title="Danger zone" testID={`danger-section:${c().id}`}>
      <If when={device() && paired()}>
        <SettingRow label="Unpair" description={packaged() ? "The computer it's paired with stops serving it. The connection and its models stay." : "Its device token stops working. The connection and its models stay."}>
          <Action testID={`connection-unpair-open:${c().id}`} label="Unpair…" variant="destructive" compact disabled={!!busy()} onPress={() => deviceConfirmUnpair.set(c().id)} />
        </SettingRow>
      </If>
      <If when={deviceConfirmUnpair() === c().id}>
        <Column gap={2} padX={4} padY={3} testID={`connection-confirm-unpair:${c().id}`}>
          <Banner
            variant="inline"
            tone="warning"
            message={
              packaged()
                ? `Unpair “${c().name}”? The computer it's paired with stops serving it at once, until you connect a device again. The connection and its models stay, and you stay signed in to Metro.`
                : `Unpair “${c().name}”? Its device token stops working at once, so metro-connector can't serve it until you issue a new token. The connection and its models stay, and you stay signed in to Metro.`
            }
          />
          <ActionBar>
            <Action label="Cancel" variant="ghost" onPress={() => deviceConfirmUnpair.set(null)} />
            <Action
              testID={`connection-confirm-unpair-now:${c().id}`}
              label="Unpair device"
              busyLabel="Unpairing…"
              busy={busy() === `unpair:${c().id}`}
              variant="danger"
              onPress={() => void unpairDevice(c().id)}
            />
          </ActionBar>
        </Column>
      </If>
      <SettingRow label="Delete" description="Removes the connection and everything stored with it.">
        <Action testID={`connection-delete-open:${c().id}`} label="Delete…" variant="destructive" compact disabled={!!busy()} onPress={() => providerConfirmDelete.set(c().id)} />
      </SettingRow>
      <If when={providerConfirmDelete() === c().id}>
        <Column gap={2} padX={4} padY={3} testID={`connection-confirm-delete:${c().id}`}>
          <Banner variant="inline" tone="warning" message={deleteText(c(), device(), shareStateOf(c()) !== null)} />
          <ActionBar>
            <Action label="Cancel" variant="ghost" onPress={() => providerConfirmDelete.set(null)} />
            <Action
              testID={`connection-confirm-delete-now:${c().id}`}
              label="Delete connection"
              busyLabel="Deleting…"
              busy={busy() === `delete:${c().id}`}
              variant="danger"
              onPress={() => {
                const id = c().id;
                const currentSession = sessionGuard();
                void deleteConnection(id).then(() => {
                  if (currentSession() && detailsOf() === id && !connections()?.some((x) => x.id === id)) detailsOf.set(null);
                });
              }}
            />
          </ActionBar>
        </Column>
      </If>
    </SettingsGroup>
  );
}

/** What deleting a connection does, in the confirmation. */
function deleteText(c: ProviderConnection, device: boolean, shared: boolean): string {
  const cap = connectionCapability(c);
  const what = device
    ? packaged()
      ? `Delete “${c.name}”? No computer can serve it any more.`
      : `Delete “${c.name}”? Its device token stops working, so metro-connector can't serve it any more.`
    : `Delete “${c.name}”? Its stored key is erased.`;
  const fallback = c.isDefault
    ? ` It's your default ${capabilityWord(cap)} connection: until you pick another, ${cap === "chat" ? "runs you start have no personal connection" : `you have no default for ${CAPABILITY_USE[cap]}`}.`
    : "";
  return `${what}${fallback}${shared ? " It's shared: the people you shared it with lose it at once." : ""}`;
}

// Leaving the page (or Settings) closes a connection's details.
onTransientClear(() => detailsOf.set(null));

// ------------------------------------------------------------------ sharing (owner only)

/**
 * Who besides the owner may use a connection: nobody (the default), members
 * of Spaces the owner picks, or everyone signed in to this server. Sharing
 * grants use, never the key, sign-in or device token.
 */
function SharingPanel(props: { connection: ProviderConnection }) {
  const c = () => props.connection;
  const d = () => (shareDraft()?.connectionId === c().id ? shareDraft() : null);
  const stored = () => shares()[c().id];
  const device = () => isDeviceConnection(c());
  const preset = () => isSubscription(c().providerId);
  const memberOf = () => new Set(spaces().map((s) => s.id));
  const gone = () => (stored()?.spaceIds ?? []).filter((id) => !memberOf().has(id)).length;
  const unchanged = () => {
    const x = d();
    const s = stored();
    if (!x || !s) return true;
    const ids = (list: string[]) => [...list].filter((id) => memberOf().has(id)).sort().join(",");
    return x.visibility === s.visibility && (x.visibility !== "spaces" || ids(x.spaceIds) === ids(s.spaceIds));
  };
  const saveLabel = () => {
    const x = d();
    if (!x || x.visibility === "personal") return stored()?.visibility === "personal" ? "Keep private" : "Stop sharing";
    const what = preset() ? "Share preset" : "Share";
    if (x.visibility === "organization") return `${what} with the organization`;
    const n = x.spaceIds.filter((id) => memberOf().has(id)).length;
    return `${what} with ${n} Space${n === 1 ? "" : "s"}`;
  };
  return (
    <Column gap={3} testID={`sharing:${c().id}`}>
      <Label variant="strong">{preset() ? "Share a bring-your-own preset" : "Who can use this connection"}</Label>
      <If when={preset()}>
        <Banner
          variant="inline"
          tone="warning"
          testID={`sharing-preset:${c().id}`}
          message="Your ChatGPT sign-in and plan can't be shared or lent. Sharing shares only a preset: the model name, with no credential. Each person signs in with their own ChatGPT account and uses their own plan. A preset never switches to anyone's paid API key."
        />
      </If>
      <If when={!d() && !shareError()}>
        <Label variant="caption" color="muted">
          Loading who can use it…
        </Label>
      </If>
      <If when={d()}>
        {(x) => (
          <Column gap={3}>
            <SegmentedControl
              testID={`sharing-visibility:${c().id}`}
              accessibilityLabel={`Who can use ${c().name}`}
              options={SHARE_OPTIONS}
              value={x().visibility}
              onChange={(v) => setShareVisibility(v)}
            />
            <If when={x().visibility === "personal"}>
              <Label variant="caption" color="muted">
                Only you can use it. This is the default.
              </Label>
            </If>
            <If when={x().visibility === "spaces"}>
              <Column gap={2}>
                <Label variant="caption" color="muted">
                  Members of the Spaces you pick can use it there while you and they are both members. Every use is checked again. Only Spaces you're in are listed.
                </Label>
                <Row gap={1} wrap testID={`sharing-spaces:${c().id}`}>
                  <Each of={spaces()} key={(s) => s.id}>
                    {(s) => (
                      <Chip
                        testID={`sharing-space:${c().id}:${s().id}`}
                        label={s().name}
                        selected={x().spaceIds.includes(s().id)}
                        onPress={() => toggleShareSpace(s().id)}
                      />
                    )}
                  </Each>
                </Row>
                <If when={spaces().length === 0}>
                  <Label variant="caption" color="muted">
                    You're not in any Space yet.
                  </Label>
                </If>
                <If when={gone() > 0}>
                  <Label variant="caption" color="muted">
                    {`It's also shared with ${gone()} Space${gone() === 1 ? "" : "s"} you're no longer in, where it no longer works. Saving removes ${gone() === 1 ? "it" : "them"}.`}
                  </Label>
                </If>
              </Column>
            </If>
            <If when={x().visibility === "organization"}>
              <Label variant="caption" color="muted">
                Everyone signed in to this Metro server can use it, in their Chat and in any Space.
              </Label>
            </If>
            <If when={x().visibility !== "personal" && !c().model}>
              <Label variant="caption" color="warning" testID={`sharing-needs-model:${c().id}`}>
                Add a model first: a connection with no model can't answer anyone.
              </Label>
            </If>
            <If when={x().visibility !== "personal"}>
              <Banner
                variant="inline"
                tone="warning"
                testID={`sharing-warning:${c().id}`}
                title={preset() ? undefined : "You see their requests and may pay for them"}
                message={
                  preset()
                    ? "People you share the preset with get a setup to copy, with nothing of yours in it. Their requests go to their own sign-in, never through yours."
                    : `Their requests go through this connection${device() ? ", your computer and your metro-connector (their requests fail while it's offline)" : ""}, using your credits or plan. As the owner of the ${device() ? "device" : "endpoint"}, you can see what they send, and you may pay for their use. They never see its base URL, key, sign-in or device token, and can't manage it. You can make it private again at any time.`
                }
              />
            </If>
          </Column>
        )}
      </If>
      <If when={shareError()}>{(message) => <Banner variant="inline" tone="danger" testID={`sharing-error:${c().id}`} message={message()} />}</If>
      <If when={shareNotice()}>{(message) => <Banner variant="inline" tone="success" testID={`sharing-notice:${c().id}`} message={message()} />}</If>
      <ActionBar>
        <Action testID={`sharing-close:${c().id}`} label="Close" variant="ghost" onPress={closeSharing} />
        <Action
          testID={`sharing-save:${c().id}`}
          label={saveLabel()}
          busyLabel="Saving…"
          busy={shareBusy() === c().id}
          disabled={!d() || (unchanged() && gone() === 0) || (d()!.visibility !== "personal" && !c().model)}
          variant={d()?.visibility === "personal" ? "secondary" : "primary"}
          onPress={() => void saveSharing()}
        />
      </ActionBar>
    </Column>
  );
}

// ------------------------------------------------------------------ adding a connection

function AddConnection() {
  const chosen = () => {
    const a = adding();
    return a.step === "form" ? providerById(a.providerId) ?? null : null;
  };
  // Keyed by way: choosing another one starts a fresh form (and drops a typed key).
  return (
    <If when={adding().step !== "closed"}>
      <If when={chosen()} else={<Picker />}>
        {(p) => (
          <Each of={[p()]} key={(x) => x.id}>
            {(x) => (
              <If when={x().execution === "device"} else={<ServerForm provider={x()} onDone={closeAdding} />}>
                <DeviceForm provider={x()} onDone={closeAdding} />
              </If>
            )}
          </Each>
        )}
      </If>
    </If>
  );
}

/**
 * Every provider's ways to connect, as one list: search, a capability filter,
 * and a row per way. A provider's heading narrows the list to it.
 */
function Picker() {
  const kinds = () => {
    const offered = new Set((catalog()?.providers ?? []).map((p) => p.capability));
    return [
      { value: "all" as const, label: "All" },
      ...(["chat", "system_one", "embeddings"] as const).map((c) => ({ value: c, label: CAPABILITY_LABEL[c], disabled: !offered.has(c) })),
    ];
  };
  const groups = () => pickerGroups(catalog()?.providers ?? [], { query: pickQuery(), capability: pickCapability(), vendor: pickVendor() }, { packaged: packaged() });
  return (
    <PickerPanel testID="add-connection" accessibilityLabel="Add a connection">
      <Row align="center" gap={2} wrap>
        <Field testID="provider-search" accessibilityLabel="Search providers and ways to connect" placeholder="Search providers" value={pickQuery()} onChange={pickQuery.set} grow autoFocus maxLength={80} />
        <SegmentedControl testID="provider-capability" accessibilityLabel="Capability" options={kinds()} value={pickCapability()} onChange={pickCapability.set} />
        <Action testID="provider-add-close" label="Close" variant="ghost" compact onPress={closeAdding} />
      </Row>
      <If when={catalog()} else={<Label variant="caption" color="muted">{catalogError() ? "The provider list couldn't be loaded." : "Loading providers…"}</Label>}>
        {(cat) => (
          <Column gap={3}>
            <If when={pickVendor()}>
              <Row>
                <Action testID="provider-picker-all" label="All providers" variant="ghost" compact onPress={() => pickVendor.set(null)} />
              </Row>
            </If>
            <If when={cat().providers.length === 0}>
              <Label variant="caption" color="muted">
                This server offers no model providers.
              </Label>
            </If>
            <If when={cat().providers.length > 0 && groups().length === 0}>
              <Label variant="caption" color="muted" testID="provider-search-empty">
                Nothing matches.
              </Label>
            </If>
            <Each of={groups()} key={(g) => g.key}>
              {(g) => <PickerGroup group={g()} />}
            </Each>
          </Column>
        )}
      </If>
    </PickerPanel>
  );
}

function PickerGroup(props: { group: VendorGroup }) {
  const g = () => props.group;
  return (
    <Column gap={1}>
      <VendorHeading
        testID={`provider-picker:${g().key}`}
        mark={g().mark}
        name={g().name}
        selected={pickVendor() === g().key}
        accessibilityLabel={pickVendor() === g().key ? `${g().name}, shown alone` : `Show only ${g().name}`}
        onPress={() => pickVendor.set(pickVendor() === g().key ? null : g().key)}
      />
      <Column testID={`provider-ways:${g().key}`}>
        <Each of={g().paths} key={(p) => p.id}>
          {(p) => {
            const label = () => wayLabel(p(), { packaged: packaged() });
            return (
              <Column testID={`provider-way:${p().id}`}>
                <MethodRow
                  testID={`provider:${p().id}`}
                  label={label()}
                  detail={methodDetail(p(), { packaged: packaged() })}
                  capability={{ label: CAPABILITY_LABEL[p().capability], tone: CAPABILITY_TONE[p().capability] }}
                  accessibilityLabel={`${g().name} · ${label()}, ${CAPABILITY_LABEL[p().capability]}`}
                  onPress={() => adding.set({ step: "form", providerId: p().id })}
                />
              </Column>
            );
          }}
        </Each>
      </Column>
    </Column>
  );
}

/** The chosen way, as the form's heading: provider, way, where it runs, capability and who can use it, then who pays. */
function ChoiceSummary(props: { provider: CatalogProvider }) {
  const how = () => describePath(props.provider, { packaged: packaged() });
  const v = () => vendorOf(props.provider);
  return (
    <Row align="center" gap={2} wrap padX={4} padY={3} testID="provider-choice">
      <BrandMark mark={v().mark} />
      <Label variant="strong" heading>
        {v().name}
      </Label>
      <Badge tone="neutral" variant="outline" label={how().method} />
      <Badge tone="neutral" variant="outline" label={how().where} />
      <Badge tone={CAPABILITY_TONE[props.provider.capability]} label={CAPABILITY_LABEL[props.provider.capability]} />
      <Badge tone="neutral" variant="outline" label={how().scope} />
      <Label variant="caption" color="muted">
        {how().billing}
      </Label>
    </Row>
  );
}

function DeviceForm(props: { provider: CatalogProvider; onDone: () => void }) {
  const name = signal(props.provider.name);
  const submit = async () => {
    const created = await createConnection({ name: name().trim() || props.provider.name, provider_id: props.provider.id }, { fetchModels: false });
    if (!created) return;
    detailsOf.set(created.id); // its details follow its setup; its models list themselves once it's online
    props.onDone();
  };
  return (
    <SettingsGroup testID="device-form">
      <ChoiceSummary provider={props.provider} />
      <SettingRow label="Name" stacked>
        <Field testID="provider-name" accessibilityLabel="Connection name" value={name()} onChange={name.set} maxLength={80} onSubmit={() => void submit()} />
      </SettingRow>
      <Row padX={4} padY={3} gap={2} justify="end" align="center" wrap>
        <Column grow>
          <Label variant="caption" color="muted" testID="device-form-next">
            {packaged()
              ? "Next: sign in on this computer. Your sign-in or key stays here; the server never receives it. Its models are listed once it's set up."
              : "Next: a device token for metro-connector. You sign in there; Metro never receives your key. Its models are listed once it's online."}
          </Label>
        </Column>
        <Action label="Cancel" variant="ghost" onPress={props.onDone} />
        <Action testID="provider-create-device" label={packaged() ? "Connect this device" : "Create and get device token"} busyLabel="Creating…" busy={providerBusy() === "create"} variant="primary" onPress={() => void submit()} />
      </Row>
    </SettingsGroup>
  );
}

function ServerForm(props: { provider: CatalogProvider; onDone: () => void }) {
  const p = props.provider;
  const name = signal(p.name);
  const baseUrl = signal(p.allowCustomUrl ? "" : p.defaultBaseUrl ?? "");
  const apiKey = signal("");
  const models = signal<NewConnectionModel[]>([]);
  const formError = signal<string | null>(null);
  /** Self-hosted endpoints: models are added here, with names and thinking options. */
  const custom = p.allowCustomUrl || !p.listsModels;
  const thinking = p.allowCustomUrl && p.capability === "chat";
  const urlOptions = () => {
    const allowed = catalog()?.allowedBaseUrls ?? [];
    return p.defaultBaseUrl && !allowed.includes(p.defaultBaseUrl) ? [p.defaultBaseUrl, ...allowed] : allowed;
  };
  /** A custom-endpoint adapter with nothing approved can't work: nothing to save. */
  const noEndpoint = () => p.allowCustomUrl && !p.defaultBaseUrl && urlOptions().length === 0;
  const submit = async () => {
    formError.set(null);
    const url = baseUrl().trim();
    if (p.allowCustomUrl) {
      if (!url && !p.defaultBaseUrl) {
        formError.set("Choose one of the endpoints the server operator approved.");
        return;
      }
      if (url && url !== p.defaultBaseUrl && !isAllowedBaseUrl(url)) {
        formError.set("That endpoint isn't on the server's approved list. Pick one of the approved URLs exactly, or ask the operator to add it.");
        return;
      }
    }
    const key = apiKey().trim();
    if (p.requiresKey && !key) {
      formError.set("This provider needs an API key.");
      return;
    }
    if (!p.listsModels && models().length === 0) {
      formError.set("This provider can't list its models: add the model ID it documents.");
      return;
    }
    apiKey.set(""); // sent once, never kept in the form
    const created = await createConnection(
      {
        name: name().trim() || p.name,
        provider_id: p.id,
        ...(url && p.allowCustomUrl ? { base_url: url } : {}),
        ...(key ? { api_key: key } : {}),
        ...(models().length ? { models: models() } : {}),
      },
      { fetchModels: p.listsModels },
    );
    if (!created) return;
    detailsOf.set(created.id); // its models, to switch off the ones not wanted
    props.onDone();
  };
  const addDraft = (draft: NewConnectionModel) => {
    if (models().some((m) => m.model_id === draft.model_id)) {
      formError.set("That model is already in the list.");
      return false;
    }
    formError.set(null);
    models.set([...models(), draft]);
    return true;
  };
  return (
    <SettingsGroup testID="server-form">
      <ChoiceSummary provider={p} />
      <SettingRow label="Name" stacked>
        <Field testID="provider-name" accessibilityLabel="Connection name" value={name()} onChange={name.set} maxLength={80} />
      </SettingRow>
      <If when={p.allowCustomUrl}>
        <SettingRow label="Endpoint" description="Only endpoints the operator approved." stacked>
          <Column gap={2}>
            <Row gap={1} wrap testID="provider-url-options">
              <Each of={urlOptions()} key={(u) => u}>
                {(u) => <Chip testID={`provider-url:${u()}`} label={u()} selected={baseUrl().trim() === u()} onPress={() => baseUrl.set(u())} />}
              </Each>
            </Row>
            <If when={urlOptions().length === 0}>
              <Label variant="caption" color="muted">
                The operator hasn't approved any custom endpoints yet.
              </Label>
            </If>
            <Field testID="provider-base-url" accessibilityLabel="Endpoint URL" value={baseUrl()} onChange={baseUrl.set} placeholder={p.defaultBaseUrl ?? "https://…/v1"} />
          </Column>
        </SettingRow>
      </If>
      <SettingRow label={p.requiresKey ? "API key" : "API key (optional)"} description="Stored encrypted on the server · never shown again" stacked>
        <Field testID="provider-key" accessibilityLabel="API key" value={apiKey()} onChange={apiKey.set} placeholder="API key" masked />
      </SettingRow>
      <If
        when={custom}
        else={
          <SettingRow label="Models" description="Every model it lists is added and switched on. You can switch off the ones you don't want after saving." />
        }
      >
        <SettingRow
          label={p.listsModels ? "Models (optional)" : "Models"}
          description={p.listsModels ? "Whatever the endpoint lists is added too. Add any it doesn't list." : "Add each model ID the provider documents."}
          stacked
        >
          <Column gap={2}>
            <Each of={models()} key={(m) => m.model_id}>
              {(m) => (
                <Row gap={2} align="center" testID={`provider-model-draft:${m().model_id}`}>
                  <Column grow>
                    <Label>{m().display_name ?? m().model_id}</Label>
                    <Label variant="caption" color="muted">
                      {[m().display_name ? m().model_id : null, m().thinking ? `Thinking: ${m().thinking!.efforts.join(" · ")}` : null].filter(Boolean).join(" · ")}
                    </Label>
                  </Column>
                  <Action
                    testID={`provider-model-draft-remove:${m().model_id}`}
                    label="Remove"
                    variant="ghost"
                    compact
                    onPress={() => models.set(models().filter((x) => x.model_id !== m().model_id))}
                  />
                </Row>
              )}
            </Each>
            <ModelDraftForm testID="provider-model" thinking={thinking} submitLabel={models().length ? "Add another" : "Add model"} onSubmit={addDraft} />
          </Column>
        </SettingRow>
      </If>
      <If when={formError()}>
        {(message) => (
          <Column padX={4} padY={2}>
            <Banner variant="inline" tone="danger" testID="provider-form-error" message={message()} />
          </Column>
        )}
      </If>
      <Row padX={4} padY={3} gap={2} justify="end" wrap>
        <Action
          label="Cancel"
          variant="ghost"
          onPress={() => {
            apiKey.set("");
            props.onDone();
          }}
        />
        <Action
          testID="provider-save"
          label={p.listsModels ? "Save and list models" : "Save"}
          busyLabel="Saving…"
          busy={providerBusy() === "create"}
          disabled={noEndpoint()}
          variant="primary"
          onPress={() => void submit()}
        />
      </Row>
    </SettingsGroup>
  );
}
