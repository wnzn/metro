// Settings › Model providers: the user's personal model connections. The
// providers offered come from the server (GET /v1/provider-catalog), never
// from a list in this app. Device connections (preferred) run in the user's
// own metro-connector, so this form never asks for their vendor key or
// password; server-stored API profiles take a key that is sent once and
// never shown again.
//
// Layout: your connections grouped by provider, each a compact row with one
// Manage control that opens everything else about it; then the Documents &
// media model (./library owns it); then Add a connection: what it's for, one
// compact drop-down of providers (one entry per provider however many ways it
// has), and that provider's ways to connect, each saying how you sign in,
// where it runs and who pays. Grouping is ./providerGroups. Feedback shows in
// the page's fixed footer, so it's seen wherever the page is scrolled.

import { signal } from "nsh";
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
  CAPABILITY_LABEL,
  CAPABILITY_USE,
  catalog,
  catalogError,
  connectionCapability,
  connections,
  connectionsError,
  createConnection,
  deviceConfirmUnpair,
  deviceStatuses,
  deleteConnection,
  fetchModels,
  isAllowedBaseUrl,
  isDeviceConnection,
  modelListings,
  providerBusy,
  providerById,
  providerConfirmDelete,
  providerNotice,
  revealed,
  rotateDeviceToken,
  unpairDevice,
  updateConnection,
} from "../settings";
import type { Capability, CatalogProvider, ProviderConnection } from "../settings";
import { onSessionEnd, spaces } from "../store";
import { Action, ActionBar, Badge, Banner, BrandMark, BrandPicker, Chip, Column, compactWidth, Field, Label, PageHeader, PageLayout, Row, SegmentedControl, SettingRow, SettingsGroup } from "../ui";
import type { BrandItem } from "../ui";
import { MediaModelSettings } from "./library/LibraryView";
import { CAPABILITY_TONE, describePath, groupCatalog, vendorOf, wayLabel } from "./providerGroups";
import type { VendorGroup } from "./providerGroups";
import { LocalDeviceBadge } from "../device/DeviceSetup";
import { packaged } from "../device/state";
import { DeviceStatusBadge, deviceStatusText, ListingResult, TokenReveal } from "./TokenReveal";

// Used only when the server's catalog gives no description of its own.
const FALLBACK_DESCRIPTION: Record<string, string> = {
  "device-openai-oauth": "Sign in with ChatGPT (eligible local integrations; preview). You approve it in your browser on the computer that runs metro-connector. Metro never asks for an OAuth code or token.",
  "device-claude-code": "Uses your Anthropic API key on this device with the installed Claude Code CLI. API billing; Claude Pro/Max subscriptions are not used.",
  "device-openai-compatible": "A local or private OpenAI-compatible endpoint you run. Its URL and key stay on this computer.",
  "device-systemone": "System One from metro-connector on this computer, with your own TypeSafe key there. Metro never receives it.",
  "device-embeddings": "An OpenAI-compatible embeddings endpoint reached from metro-connector on this computer, such as a local server you run.",
};

const SHARE_OPTIONS = [
  { value: "personal", label: "Only me" },
  { value: "spaces", label: "Selected Spaces" },
  { value: "organization", label: "Organization" },
] as const;

const SHARE_BADGE: Record<Exclude<ShareVisibility, "personal">, string> = { spaces: "Shared with Spaces", organization: "Shared with organization" };

/** The packaged app sets device connections up itself; it never names a helper process. */
const PACKAGED_DEVICE_TEXT =
  "Runs in Metro on this computer, with your own sign-in or key kept here. The Metro server relays requests and never receives them. You connect it next, in Metro.";

function describeProvider(p: CatalogProvider): string {
  if (p.execution === "device" && packaged()) return PACKAGED_DEVICE_TEXT;
  if (p.description) return p.description;
  if (FALLBACK_DESCRIPTION[p.id]) return FALLBACK_DESCRIPTION[p.id];
  if (p.execution === "device") return "Runs in metro-connector on this computer.";
  const parts = [p.requiresKey ? "Needs an API key, stored encrypted on the server." : "No API key needed."];
  if (p.allowCustomUrl) parts.push("Custom endpoints the operator approved.");
  return parts.join(" ");
}

/** Connection rows the user opened to manage. Ids only; dropped at sign-out. */
const managing = signal<ReadonlySet<string>>(new Set());
const isManaging = (id: string) => managing().has(id);
function setManaging(id: string, open: boolean) {
  const next = new Set(managing());
  if (open) next.add(id);
  else next.delete(id);
  managing.set(next);
}
onSessionEnd(() => managing.set(new Set()));

export function ProvidersSettings() {
  const device = () => revealed()?.kind === "device";
  const hasMessages = () => !device() && !!(catalogError() || connectionsError() || providerNotice());
  return (
    <PageLayout testID="providers-settings" pad={compactWidth() ? 4 : 6} gap={6} footer={hasMessages() ? <Messages /> : undefined}>
      <If
        when={device()}
        else={
          <Column gap={6}>
            <PageHeader
              title="Model providers"
              subtitle="Each connection serves one capability, and each capability has its own default. Your runs, and the threads they start, use your default chat connection unless Model routing picks another. Connections are private unless you share one."
            />
            <ConnectionList />
            {/* Documents & media's own form and Save (./library). Mounting it only loads the choice: nothing is indexed or sent. */}
            <MediaModelSettings />
            <AddConnection />
          </Column>
        }
      >
        <TokenReveal />
      </If>
    </PageLayout>
  );
}

/** Errors and what the last change did, in the page's fixed footer. */
function Messages() {
  return (
    <Column gap={2}>
      <If when={catalogError()}>{(message) => <Banner variant="inline" tone="danger" testID="catalog-error" message={message()} />}</If>
      <If when={connectionsError()}>{(message) => <Banner variant="inline" tone="danger" testID="providers-error" message={message()} />}</If>
      <If when={providerNotice()}>{(message) => <Banner variant="inline" tone="success" testID="providers-notice" message={message()} />}</If>
    </Column>
  );
}

// ------------------------------------------------------------------ the list

/** "1 connection", "2 connections". */
export const countLabel = (n: number, noun: string) => `${n} ${noun}${n === 1 ? "" : "s"}`;

type ConnectionGroup = { key: string; name: string; mark: VendorGroup["mark"]; items: ProviderConnection[] };

/** The user's connections, by provider, in the catalog's provider order. */
function groupConnections(list: readonly ProviderConnection[]): ConnectionGroup[] {
  const order = groupCatalog(catalog()?.providers ?? []).map((g) => g.key);
  const groups = new Map<string, ConnectionGroup>();
  for (const c of list) {
    const v = vendorOf(providerById(c.providerId) ?? { id: c.providerId, name: c.providerId });
    let g = groups.get(v.key);
    if (!g) {
      g = { key: v.key, name: v.name, mark: v.mark, items: [] };
      groups.set(v.key, g);
    }
    g.items.push(c);
  }
  const rank = (key: string) => {
    const i = order.indexOf(key);
    return i < 0 ? order.length : i;
  };
  return [...groups.values()].sort((a, b) => rank(a.key) - rank(b.key) || a.name.localeCompare(b.name));
}

function ConnectionList() {
  return (
    <Column gap={3} testID="connection-list">
      <Column gap={1}>
        <Label variant="heading" heading>
          Your connections
        </Label>
        <Label variant="caption" color="muted">
          Only you can change these. A saved key is never shown again, and sharing never reveals it.
        </Label>
      </Column>
      <If when={connections()} else={<SettingsGroup><SettingRow label="Loading your connections…" /></SettingsGroup>}>
        {(list) => (
          <Column gap={4}>
            <If when={list().length === 0}>
              <SettingsGroup>
                <SettingRow label="No connections yet" description="Add one below. Until then, Ask agent uses what the server operator configured, if anything." />
              </SettingsGroup>
            </If>
            <Each of={groupConnections(list())} key={(g) => g.key}>
              {(g) => (
                <Column gap={2} testID={`connection-group:${g().key}`}>
                  <Row align="center" gap={2} padX={1}>
                    <BrandMark mark={g().mark} />
                    <Label variant="strong" heading>
                      {g().name}
                    </Label>
                    <Label variant="caption" color="muted">
                      {countLabel(g().items.length, "connection")}
                    </Label>
                  </Row>
                  <SettingsGroup>
                    <Each of={g().items} key={(c) => c.id}>
                      {(c) => <ConnectionRow connection={c()} />}
                    </Each>
                  </SettingsGroup>
                </Column>
              )}
            </Each>
          </Column>
        )}
      </If>
    </Column>
  );
}

/** One line on how a saved connection works: its way of connecting, its model, its endpoint. */
function connectionSummary(c: ProviderConnection): string {
  const p = providerById(c.providerId);
  const how = p ? describePath(p, { packaged: packaged() }) : null;
  const parts = [how ? `${how.method}${how.variant ? ` (${how.variant})` : ""}` : c.providerId, c.model ? c.model : "no model chosen yet"];
  if (c.baseUrl) parts.push(c.baseUrl);
  return parts.join(" · ");
}

/** "chat", "System One", "embeddings" inside a sentence. */
const capabilityWord = (c: Capability) => (c === "chat" ? "chat" : c === "system_one" ? CAPABILITY_LABEL[c] : CAPABILITY_LABEL[c].toLowerCase());

/**
 * One saved connection: its name, how it works and its badges, with one
 * Manage control. Everything else (default, sharing, models, key, device,
 * delete) is inside Manage, so a row stays one line of actions.
 */
function ConnectionRow(props: { connection: ProviderConnection }) {
  const c = () => props.connection;
  const provider = () => providerById(c().providerId);
  const device = () => isDeviceConnection(c());
  const manualModel = signal("");
  const replacingKey = signal(false);
  const newKey = signal("");
  const listing = () => modelListings()[c().id];
  const busy = () => providerBusy();
  const status = () => deviceStatuses()[c().id];
  const open = () => isManaging(c().id) || sharingOpen() === c().id || providerConfirmDelete() === c().id || deviceConfirmUnpair() === c().id;
  const paired = () => {
    const st = status();
    return !!st && !("error" in st) && st.paired;
  };
  const offline = () => {
    const st = status();
    return !!st && !("error" in st) && !st.online;
  };
  const saveModel = async (model: string) => {
    if (await updateConnection(c().id, { model }, `“${c().name}” now uses ${model.trim()}.`)) manualModel.set("");
  };
  const saveKey = async () => {
    const key = newKey();
    newKey.set(""); // never keep it, whatever the outcome
    if (!key.trim()) return;
    if (await updateConnection(c().id, { api_key: key.trim() }, `Replaced the key of “${c().name}”. It is never shown again.`)) replacingKey.set(false);
  };
  const capability = () => connectionCapability(c());
  const shared = () => {
    const v = c().visibility ?? shareOf(c().id);
    return v && v !== "personal" ? v : null;
  };
  const toggleManage = () => {
    const next = !open();
    setManaging(c().id, next);
    if (!next) {
      if (sharingOpen() === c().id) closeSharing();
      if (providerConfirmDelete() === c().id) providerConfirmDelete.set(null);
      if (deviceConfirmUnpair() === c().id) deviceConfirmUnpair.set(null);
      replacingKey.set(false);
      newKey.set("");
    }
  };
  return (
    <Column testID={`connection:${c().id}`}>
      <SettingRow label={c().name} description={connectionSummary(c())}>
        <Row align="center" gap={2} wrap justify="end">
          <Badge testID={`connection-capability:${c().id}`} tone={CAPABILITY_TONE[capability()]} label={CAPABILITY_LABEL[capability()]} />
          <If when={c().isDefault}>
            <Badge testID={`connection-default:${c().id}`} tone="neutral" variant="outline" label={`Default ${capabilityWord(capability())}`} />
          </If>
          <If when={shared()}>
            {(v) => <Badge testID={`connection-shared:${c().id}`} tone="warning" label={SHARE_BADGE[v()]} />}
          </If>
          <If when={device()} else={<Badge tone="neutral" variant="outline" label={c().hasKey ? "Key saved" : "No key"} />}>
            <DeviceStatusBadge connectionId={c().id} />
            <If when={packaged()}>
              <LocalDeviceBadge connectionId={c().id} />
            </If>
          </If>
          <Action
            testID={`connection-manage:${c().id}`}
            label="Manage"
            accessibilityLabel={`Manage ${c().name}`}
            expanded={open()}
            onPress={toggleManage}
          />
        </Row>
      </SettingRow>
      <If when={device()}>
        <Column padX={4} padY={1}>
          <Label variant="caption" color="muted" testID={`device-status-text:${c().id}`}>
            {deviceStatusText(c().id)}
            {c().isDefault && offline() ? " Runs you start fail until it's back online; Metro never switches to another connection." : ""}
          </Label>
        </Column>
      </If>
      <If when={open()}>
        <Column padX={4} padY={3} gap={3} testID={`connection-details:${c().id}`}>
          <Row gap={2} wrap>
            <If when={!c().isDefault}>
              <Action
                testID={`connection-make-default:${c().id}`}
                label={`Make default for ${capabilityWord(capability())}`}
                disabled={!!busy()}
                onPress={() =>
                  void updateConnection(
                    c().id,
                    { is_default: true },
                    `“${c().name}” is now your default ${capabilityWord(capability())} connection, for ${CAPABILITY_USE[capability()]}. Your defaults for other capabilities are unchanged.`,
                  )
                }
              />
            </If>
            <Action
              testID={`connection-sharing:${c().id}`}
              label="Sharing"
              accessibilityLabel={`Who can use ${c().name}`}
              expanded={sharingOpen() === c().id}
              disabled={!!busy()}
              onPress={() => void openSharing(c().id)}
            />
            <If when={device() || provider()?.listsModels !== false}>
              <Action
                testID={`connection-models:${c().id}`}
                label={device() ? "Test connection" : "Fetch models"}
                busyLabel={device() ? "Testing…" : "Fetching…"}
                busy={listing()?.state === "loading"}
                onPress={() => void fetchModels(c().id)}
              />
            </If>
            <If when={device()}>
              <Action
                testID={`connection-token:${c().id}`}
                label={packaged() ? (paired() ? "This computer…" : "Set up on this computer") : "New device token"}
                busyLabel={packaged() ? "Opening…" : "Issuing…"}
                busy={busy() === `token:${c().id}`}
                disabled={!!busy()}
                onPress={() => void rotateDeviceToken(c().id)}
              />
            </If>
            <If when={!device() && (provider()?.requiresKey || c().hasKey)}>
              <Action
                testID={`connection-replace-key:${c().id}`}
                label={replacingKey() ? "Cancel key change" : "Replace key…"}
                variant="ghost"
                onPress={() => {
                  newKey.set("");
                  replacingKey.set(!replacingKey());
                }}
              />
            </If>
            <If when={device() && paired()}>
              <Action testID={`connection-unpair:${c().id}`} label="Unpair…" variant="destructive" disabled={!!busy()} onPress={() => deviceConfirmUnpair.set(c().id)} />
            </If>
            <Action testID={`connection-delete:${c().id}`} label="Delete…" variant="destructive" disabled={!!busy()} onPress={() => providerConfirmDelete.set(c().id)} />
          </Row>

          <If when={replacingKey()}>
            <Row gap={2} align="center">
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

          <ListingResult connectionId={c().id} />
          <If when={listing()?.state === "ok" && (listing() as { models: string[] }).models.length > 0 ? (listing() as { models: string[] }).models : null}>
            {(models) => (
              <Column gap={1}>
                <Label variant="caption" color="muted">
                  Pick the model this connection uses:
                </Label>
                <Row gap={1} wrap testID={`connection-model-list:${c().id}`}>
                  <Each of={models()} key={(m) => m}>
                    {(m) => <Chip testID={`model:${c().id}:${m()}`} label={m()} selected={c().model === m()} disabled={!!busy()} onPress={() => void saveModel(m())} />}
                  </Each>
                </Row>
              </Column>
            )}
          </If>
          <Row gap={2} align="center">
            <Field
              testID={`connection-model-input:${c().id}`}
              accessibilityLabel={`Model ID for ${c().name}`}
              value={manualModel()}
              onChange={manualModel.set}
              placeholder={c().model ? `Model ID (now ${c().model})` : "Model ID, as your provider documents it"}
              grow
              maxLength={200}
              onSubmit={() => void saveModel(manualModel())}
            />
            <Action
              testID={`connection-model-save:${c().id}`}
              label="Use this model"
              busyLabel="Saving…"
              busy={busy() === `patch:${c().id}`}
              disabled={!manualModel().trim()}
              onPress={() => void saveModel(manualModel())}
            />
          </Row>

          <If when={sharingOpen() === c().id}>
            <SharingPanel connection={c()} />
          </If>

          <If when={deviceConfirmUnpair() === c().id}>
            <Column gap={2} testID={`connection-confirm-unpair:${c().id}`}>
              <Banner
                variant="inline"
                tone="warning"
                message={
                  packaged()
                    ? `Unpair “${c().name}”? The computer it's paired with stops serving it at once, until you connect a device again. The connection and its model stay, and you stay signed in to Metro.`
                    : `Unpair “${c().name}”? Its device token stops working at once, so metro-connector can't serve it until you issue a new token. The connection and its model stay, and you stay signed in to Metro.`
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

          <If when={providerConfirmDelete() === c().id}>
            <Column gap={2} testID={`connection-confirm-delete:${c().id}`}>
              <Banner variant="inline" tone="warning" message={deleteText(c(), device(), shared() !== null)} />
              <ActionBar>
                <Action label="Cancel" variant="ghost" onPress={() => providerConfirmDelete.set(null)} />
                <Action
                  testID={`connection-confirm-delete-now:${c().id}`}
                  label="Delete connection"
                  busyLabel="Deleting…"
                  busy={busy() === `delete:${c().id}`}
                  variant="danger"
                  onPress={() => void deleteConnection(c().id)}
                />
              </ActionBar>
            </Column>
          </If>
        </Column>
      </If>
    </Column>
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
                Choose this connection's model first: a connection with no model can't answer anyone.
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
        <Action label="Close" variant="ghost" onPress={closeSharing} />
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
  const kind = signal<Capability | "all">("all");
  const vendor = signal<string | null>(null);
  const chosen = signal<string | null>(null);
  const kinds = () => {
    const offered = new Set((catalog()?.providers ?? []).map((p) => p.capability));
    return [
      { value: "all" as const, label: "All" },
      ...(["chat", "system_one", "embeddings"] as const).map((c) => ({ value: c, label: CAPABILITY_LABEL[c], disabled: !offered.has(c) })),
    ];
  };
  /** The catalog by provider, keeping only the ways that serve the chosen capability. */
  const groups = () =>
    groupCatalog(catalog()?.providers ?? [])
      .map((g) => ({ ...g, paths: g.paths.filter((p) => kind() === "all" || p.capability === kind()) }))
      .filter((g) => g.paths.length > 0);
  const items = (): BrandItem[] =>
    groups().map((g) => ({
      value: g.key,
      label: g.name,
      mark: g.mark,
      hint: `${g.paths.length === 1 ? describePath(g.paths[0]).method : `${g.paths.length} ways to connect`} · ${g.capabilities.filter((c) => g.paths.some((p) => p.capability === c)).map((c) => CAPABILITY_LABEL[c]).join(", ")}`,
    }));
  const group = () => groups().find((g) => g.key === vendor()) ?? null;
  const selected = () => {
    const id = chosen();
    const g = group();
    return id && g ? g.paths.find((p) => p.id === id) ?? null : null;
  };
  const chooseVendor = (key: string | null) => {
    vendor.set(key);
    const g = groups().find((x) => x.key === key);
    // One way only: nothing more to choose.
    chosen.set(g && g.paths.length === 1 ? g.paths[0].id : null);
  };
  const chooseKind = (value: Capability | "all") => {
    kind.set(value);
    const g = group();
    if (!g) {
      vendor.set(null);
      chosen.set(null);
    } else if (!g.paths.some((p) => p.id === chosen())) chosen.set(g.paths.length === 1 ? g.paths[0].id : null);
  };
  return (
    <Column gap={4} testID="add-connection">
      <Column gap={1}>
        <Label variant="heading" heading>
          Add a connection
        </Label>
        <Label variant="caption" color="muted">
          Pick a provider, then how to connect. On your device is preferred: your sign-in or key stays on your computer.
        </Label>
      </Column>
      <If when={catalog()} else={<Label variant="caption" color="muted">{catalogError() ? "The provider list couldn't be loaded." : "Loading providers…"}</Label>}>
        {(cat) => (
          <Column gap={4}>
            <If when={cat().providers.length === 0}>
              <Label variant="caption" color="muted">
                This server offers no model providers.
              </Label>
            </If>
            <SettingsGroup>
              <SettingRow label="For" description="Chat answers, System One decides, Embeddings index for search. A connection serves only one.">
                <SegmentedControl testID="provider-capability" accessibilityLabel="What the connection is for" options={kinds()} value={kind()} onChange={chooseKind} />
              </SettingRow>
              <SettingRow label="Provider" description="Only what this server supports." stacked>
                <BrandPicker testID="provider-picker" accessibilityLabel="Provider" placeholder="Choose a provider" items={items()} value={group() ? vendor() : null} onChange={chooseVendor} />
              </SettingRow>
            </SettingsGroup>
            <If when={group()}>
              {(g) => <WaysToConnect group={g()} chosen={chosen()} onChoose={chosen.set} />}
            </If>
            {/* Keyed by provider: choosing another one starts a fresh form (and drops a typed key). */}
            <Each of={selected() ? [selected()!] : []} key={(p) => p.id}>
              {(p) => (
                <If when={p().execution === "device"} else={<ServerForm provider={p()} onDone={() => chosen.set(null)} />}>
                  <DeviceForm provider={p()} onDone={() => chosen.set(null)} />
                </If>
              )}
            </Each>
          </Column>
        )}
      </If>
    </Column>
  );
}

/** A provider's ways to connect: how you sign in, where it runs, who pays, and what it's for. */
function WaysToConnect(props: { group: VendorGroup; chosen: string | null; onChoose: (id: string) => void }) {
  return (
    <SettingsGroup
      title={`Connect ${props.group.name}`}
      description={props.group.paths.length === 1 ? "One way to connect on this server." : `${props.group.paths.length} ways to connect on this server. Each makes its own connection.`}
      testID={`provider-ways:${props.group.key}`}
    >
      <Each of={props.group.paths} key={(p) => p.id}>
        {(p) => {
          const how = () => describePath(p(), { packaged: packaged() });
          const label = () => wayLabel(p(), { packaged: packaged() });
          const chosen = () => props.chosen === p().id;
          return (
            <SettingRow label={label()} description={`${how().where} · ${how().billing}`} testID={`provider-way:${p().id}`}>
              <Row align="center" gap={2}>
                <Badge tone={CAPABILITY_TONE[p().capability]} label={CAPABILITY_LABEL[p().capability]} />
                <Action
                  testID={`provider:${p().id}`}
                  label={chosen() ? "Setting up" : "Set up"}
                  accessibilityLabel={chosen() ? `Setting up ${label()}, below` : `Set up ${label()}`}
                  disabled={chosen()}
                  onPress={() => props.onChoose(p().id)}
                />
              </Row>
            </SettingRow>
          );
        }}
      </Each>
    </SettingsGroup>
  );
}

/** What the chosen way means, shown on its form: the way, where it runs, who pays, what it's for and who can use it. */
function ChoiceSummary(props: { provider: CatalogProvider }) {
  const how = () => describePath(props.provider, { packaged: packaged() });
  const v = () => vendorOf(props.provider);
  return (
    <Row align="center" gap={2} wrap padX={4} padY={3} testID="provider-choice">
      <BrandMark mark={v().mark} />
      <Label variant="strong">{v().name}</Label>
      <Badge tone="neutral" variant="outline" label={how().method} />
      <Badge tone="neutral" variant="outline" label={how().where} />
      <Badge tone={CAPABILITY_TONE[props.provider.capability]} label={CAPABILITY_LABEL[props.provider.capability]} />
      <Badge tone="neutral" variant="outline" label={how().scope} />
      <Label variant="caption" color="muted">
        {how().billing}
      </Label>
      <Label variant="caption" color="muted" testID="provider-choice-description">
        {describeProvider(props.provider)}
      </Label>
    </Row>
  );
}

function DeviceForm(props: { provider: CatalogProvider; onDone: () => void }) {
  const name = signal(props.provider.name);
  const submit = async () => {
    const created = await createConnection({ name: name().trim() || props.provider.name, provider_id: props.provider.id }, { fetchModels: false });
    if (!created) return;
    setManaging(created.id, true); // open, to pick its model next
    props.onDone();
  };
  return (
    <SettingsGroup
      title={`New ${props.provider.name} connection`}
      description={
        packaged()
          ? "No vendor key or password goes to the Metro server. Next, Metro asks your consent and connects this computer: you sign in to the provider here and choose the models it may serve."
          : "No vendor key or password goes into Metro. You'll get a device token for metro-connector, and you sign in to the provider there."
      }
      testID="device-form"
    >
      <ChoiceSummary provider={props.provider} />
      <SettingRow label="Name" stacked>
        <Field testID="provider-name" accessibilityLabel="Connection name" value={name()} onChange={name.set} maxLength={80} onSubmit={() => void submit()} />
      </SettingRow>
      <Row padX={4} padY={3} gap={2} justify="end" wrap>
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
  const model = signal("");
  const formError = signal<string | null>(null);
  const urlOptions = () => {
    const allowed = catalog()?.allowedBaseUrls ?? [];
    return p.defaultBaseUrl && !allowed.includes(p.defaultBaseUrl) ? [p.defaultBaseUrl, ...allowed] : allowed;
  };
  /** A custom-endpoint adapter with nothing approved can't work: nothing to save. */
  const noEndpoint = () => p.allowCustomUrl && !p.defaultBaseUrl && urlOptions().length === 0;
  const submit = async (fetch: boolean) => {
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
    if (!p.listsModels && !model().trim()) {
      formError.set("This provider can't list its models: enter the model ID it documents.");
      return;
    }
    apiKey.set(""); // sent once, never kept in the form
    const created = await createConnection(
      {
        name: name().trim() || p.name,
        provider_id: p.id,
        ...(url && p.allowCustomUrl ? { base_url: url } : {}),
        ...(key ? { api_key: key } : {}),
        ...(model().trim() ? { model: model().trim() } : {}),
      },
      { fetchModels: fetch },
    );
    if (!created) return;
    setManaging(created.id, true); // open, to pick its model next
    props.onDone();
  };
  return (
    <SettingsGroup title={`New ${p.name} profile`} description="Stored on the server for you only. The server calls this provider directly." testID="server-form">
      <ChoiceSummary provider={p} />
      <SettingRow label="Name" stacked>
        <Field testID="provider-name" accessibilityLabel="Connection name" value={name()} onChange={name.set} maxLength={80} />
      </SettingRow>
      <If when={p.allowCustomUrl}>
        <SettingRow label="Endpoint" description="Only endpoints the server operator approved, exactly as listed." stacked>
          <Column gap={2}>
            <Row gap={1} wrap testID="provider-url-options">
              <Each of={urlOptions()} key={(u) => u}>
                {(u) => <Chip testID={`provider-url:${u()}`} label={u()} selected={baseUrl().trim() === u()} onPress={() => baseUrl.set(u())} />}
              </Each>
            </Row>
            <If when={urlOptions().length === 0}>
              <Label variant="caption" color="muted">
                The operator hasn't approved any custom endpoints (METRO_PROVIDER_ALLOWED_BASE_URLS).
              </Label>
            </If>
            <Field testID="provider-base-url" accessibilityLabel="Endpoint URL" value={baseUrl()} onChange={baseUrl.set} placeholder={p.defaultBaseUrl ?? "https://…/v1"} />
          </Column>
        </SettingRow>
      </If>
      <SettingRow label={p.requiresKey ? "API key" : "API key (optional)"} description="Sent once and stored encrypted. Metro never shows it again, to you or anyone." stacked>
        <Field testID="provider-key" accessibilityLabel="API key" value={apiKey()} onChange={apiKey.set} placeholder="API key" masked />
      </SettingRow>
      <SettingRow
        label={p.listsModels ? "Model (optional)" : "Model"}
        description={p.listsModels ? "Leave empty to pick from the list after saving, or enter a model ID by hand." : "This provider can't list its models: enter the model ID it documents."}
        stacked
      >
        <Field testID="provider-model" accessibilityLabel="Model ID" value={model()} onChange={model.set} placeholder="Model ID" maxLength={200} />
      </SettingRow>
      <If when={formError()}>
        {(message) => (
          <Column padX={4} padY={2}>
            <Banner variant="inline" tone="danger" testID="provider-form-error" message={message()} />
          </Column>
        )}
      </If>
      <Row padX={4} padY={3} gap={2} justify="end" wrap>
        <Action label="Cancel" variant="ghost" onPress={() => {
          apiKey.set("");
          props.onDone();
        }} />
        <Action testID="provider-save" label="Save" busyLabel="Saving…" busy={providerBusy() === "create"} disabled={noEndpoint()} variant={p.listsModels ? "secondary" : "primary"} onPress={() => void submit(false)} />
        <If when={p.listsModels}>
          <Action testID="provider-save-fetch" label="Save and fetch models" busyLabel="Saving…" busy={providerBusy() === "create"} disabled={noEndpoint()} variant="primary" onPress={() => void submit(true)} />
        </If>
      </Row>
    </SettingsGroup>
  );
}
