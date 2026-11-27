// The one place a just-issued token is shown: a focused panel that takes over
// its page (Settings › Runners, Settings › Model providers, or a Space's
// Runner settings) until the user presses Done or leaves. The token starts
// masked; Copy works without showing it. The setup steps beside it are
// non-secret text (src/features/setupTemplates.ts): they say where to paste
// the token, never contain it.
//
// A device connection in the packaged app has no token here at all: the same
// panel shows in-app setup on this computer (src/device/DeviceSetup.tsx), and
// the launcher's bridge gets the device token from the server itself.

import { Clipboard } from "nsh";
import manifest from "../../nsh.json";
import { connectedTo } from "../store";
import { DeviceSetup } from "../device/DeviceSetup";
import { localBadge, localConnections, packaged } from "../device/state";
import { deviceStatuses, dismissReveal, fetchModels, modelListings, revealCopied, revealed, revealShown } from "../settings";
import type { Reveal } from "../settings";
import { Action, Badge, Banner, Column, CopyBlock, Label, Row, RunnerInstall, SecretValue, SettingRow, SettingsGroup } from "../ui";
import type { Tone } from "../ui";
import { deviceGuide } from "./setupTemplates";

/** This app's release version: the runner image of the same release goes with it. */
const APP_VERSION: string = (manifest as { version: string }).version;

const MASK = "•".repeat(32);
type RunnerReveal = Extract<Reveal, { kind: "runner" }>;
type DeviceReveal = Extract<Reveal, { kind: "device" }>;
/** A device reveal of the packaged app: in-app setup, no token. */
const nativeSetup = (r: Reveal): DeviceReveal | null => (r.kind === "device" && r.token === null ? r : null);

function titleOf(r: Reveal): string {
  if (r.kind === "runner") return r.reason === "created" ? `Connect “${r.runnerName}”` : `New token for “${r.runnerName}”`;
  if (r.kind === "dedicated") return `Dedicated runner token for ${r.spaceName}`;
  if (nativeSetup(r)) return r.reason === "created" ? `Set up “${r.connectionName}” on this computer` : `“${r.connectionName}” on this computer`;
  return r.reason === "created" ? `Device token for “${r.connectionName}” (headless computer)` : `New device token for “${r.connectionName}”`;
}

function subtitleOf(r: Reveal): string {
  if (r.kind === "runner") {
    return r.reason === "created"
      ? "Start it on its machine, then attach it to Spaces you own."
      : "The old token stopped working in every Space this runner serves. Put the new one in its runner.token (or runner.env) and restart the runner.";
  }
  if (r.kind === "dedicated") return "This token serves this one Space only. Issuing a new one replaces it.";
  if (nativeSetup(r)) return "Runs on this computer, in Metro. Pair it, sign in to the provider here, and approve the models it may serve.";
  return r.reason === "created"
    ? "This build of Metro can't set it up itself. Give this token to metro-connector on the headless computer that serves it."
    : "The old device token stopped working. Give the new one to metro-connector on the headless computer that serves it.";
}

export function TokenReveal() {
  const serverUrl = () => connectedTo() ?? "https://metro.example.com";
  const copy = () => {
    const r = revealed();
    if (!r || r.token === null) return;
    Clipboard.writeText(r.token)
      .then(() => revealCopied.set(true))
      .catch(() => {});
  };
  return (
    <If when={revealed()}>
      {(r) => (
        <Column gap={6} testID="token-reveal" accessibilityLabel={titleOf(r())}>
          <Column gap={1}>
            <Label variant="title" testID="token-reveal-title">
              {titleOf(r())}
            </Label>
            <Label variant="body" color="muted">
              {subtitleOf(r())}
            </Label>
          </Column>

          <If when={!nativeSetup(r())}>
            <SettingsGroup title="Token" description="Shown once. Copy it now: after Done Metro can't show it again, and a new one stops this one.">
              <SettingRow label="Keep it in a private file (chmod 600), never in a command line or a chat." stacked>
                <Column gap={2}>
                  <SecretValue testID="runner-token-value" value={revealShown() ? r().token ?? MASK : MASK} />
                  <Row gap={2} wrap>
                    <Action testID="copy-runner-token" label={revealCopied() ? "Copied" : "Copy token"} variant="primary" onPress={copy} />
                    <Action
                      testID="reveal-toggle"
                      label={revealShown() ? "Hide" : "Show"}
                      accessibilityLabel={revealShown() ? "Hide the token" : "Show the token"}
                      onPress={() => revealShown.set(!revealShown())}
                    />
                    <Action testID="hide-runner-token" label="Done" accessibilityLabel="Done, forget the token" onPress={dismissReveal} />
                  </Row>
                </Column>
              </SettingRow>
            </SettingsGroup>
          </If>
          <If when={nativeSetup(r())}>
            {(d) => (
              <Column gap={6}>
                <DeviceSetup connectionId={d().connectionId} connectionName={d().connectionName} providerId={d().providerId} />
                <ListingResult connectionId={d().connectionId} />
                <Row>
                  <Action testID="device-setup-done" label="Done" accessibilityLabel="Done, back to Model providers" onPress={dismissReveal} />
                </Row>
              </Column>
            )}
          </If>

          <If when={r().kind === "runner" ? (r() as RunnerReveal) : null}>
            {(rr) => <RunnerInstall serverUrl={serverUrl()} runnerName={rr().runnerName} appVersion={APP_VERSION} />}
          </If>
          <If when={r().kind === "dedicated" ? (r() as Extract<Reveal, { kind: "dedicated" }>) : null}>
            {(rr) => <RunnerInstall serverUrl={serverUrl()} runnerName={rr().spaceName} appVersion={APP_VERSION} dedicated />}
          </If>
          <If when={r().kind === "device" && !nativeSetup(r()) ? (r() as DeviceReveal) : null}>
            {(d) => <DeviceSteps connectionId={d().connectionId} connectionName={d().connectionName} providerId={d().providerId} />}
          </If>
        </Column>
      )}
    </If>
  );
}

function DeviceSteps(props: { connectionId: string; connectionName: string; providerId: string }) {
  const guide = () => deviceGuide({ serverUrl: connectedTo() ?? "", connectionName: props.connectionName, providerId: props.providerId });
  const listing = () => modelListings()[props.connectionId];
  return (
    <SettingsGroup title="On a headless computer (operators and development builds)" description={guide().note} testID="connector-steps">
      <Each of={guide().steps.map((step, i) => ({ ...step, n: i + 1 }))} key={(s) => `${s.n}`}>
        {(s) => (
          <SettingRow label={`${s().n}. ${s().text}`} stacked={!!s().command}>
            <If when={s().command}>{(command) => <CopyBlock testID={`connector-step-${s().n}`} label="the metro-connector command" text={command()} />}</If>
          </SettingRow>
        )}
      </Each>
      <SettingRow label="Connector" description={deviceStatusText(props.connectionId)}>
        <DeviceStatusBadge connectionId={props.connectionId} />
      </SettingRow>
      <SettingRow label="Test connection" description="Asks your metro-connector for its models. It answers only while it's running and signed in.">
        <Action
          testID="reveal-test-connection"
          label="Test connection"
          busyLabel="Testing…"
          busy={listing()?.state === "loading"}
          onPress={() => void fetchModels(props.connectionId)}
        />
      </SettingRow>
      <ListingResult connectionId={props.connectionId} />
    </SettingsGroup>
  );
}

/** A successful listing in words: "Connected: 1 model listed." */
function listingText(l: { state: string; models?: string[] }): string {
  const n = l.state === "ok" ? l.models?.length ?? 0 : 0;
  return n ? `Connected: ${n} model${n === 1 ? "" : "s"} listed.` : "Connected, but it listed no models. Enter a model ID by hand.";
}

/** What the last model listing of a connection said. */
export function ListingResult(props: { connectionId: string }) {
  const listing = () => modelListings()[props.connectionId];
  return (
    <If when={listing() && listing()!.state !== "loading" ? listing() : null}>
      {(l) => (
        <Column padX={4} padY={3}>
          <If
            when={l().state === "error" ? (l() as { message: string; offline: boolean }) : null}
            else={<Banner variant="inline" tone="success" testID={`listing-ok:${props.connectionId}`} message={listingText(l())} />}
          >
            {(e) => <Banner variant="inline" tone={e().offline ? "warning" : "danger"} testID={`listing-error:${props.connectionId}`} message={e().message} />}
          </If>
        </Column>
      )}
    </If>
  );
}

/** Paired / online of a device connection: about its metro-connector, not a Space runner. */
export function deviceState(connectionId: string): { tone: Tone; label: string } {
  const st = deviceStatuses()[connectionId];
  if (!st) return { tone: "neutral", label: "Checking…" };
  if ("error" in st) return { tone: "warning", label: "Status unknown" };
  if (!st.paired) return { tone: "neutral", label: "Not paired" };
  return st.online ? { tone: "success", label: "Connected" } : { tone: "warning", label: "Paired · disconnected" };
}

export function deviceStatusText(connectionId: string): string {
  const st = deviceStatuses()[connectionId];
  if (packaged()) {
    const here = localConnections()[connectionId];
    if (!st) return "Checking whether its device is online…";
    if ("error" in st) return `Couldn't check its device: ${st.error}`;
    if (!st.paired) return "No computer is paired yet. Press Set up on this computer.";
    const seen = st.lastSeenSecs === null ? "" : ` Last seen ${ago(st.lastSeenSecs)}.`;
    if (here) return st.online ? `This computer serves it (${localBadge(here).label.toLowerCase()}).${seen}` : `Paired with this computer, which isn't serving it now (${localBadge(here).label.toLowerCase()}).${seen}`;
    return st.online ? `Another computer serves it.${seen}` : `Paired with another computer, which isn't serving it now.${seen}`;
  }
  if (!st) return "Checking whether metro-connector is online…";
  if ("error" in st) return `Couldn't check metro-connector: ${st.error}`;
  if (!st.paired) return "No device token is issued. Press New device token and run metro-connector setup.";
  const seen = st.lastSeenSecs === null ? "" : ` Last seen ${ago(st.lastSeenSecs)}.`;
  return st.online ? `metro-connector is polling for requests.${seen}` : `A device token is issued, but metro-connector isn't polling. Start it with metro-connector run.${seen}`;
}

function ago(secs: number): string {
  if (secs < 60) return `${Math.round(secs)} s ago`;
  if (secs < 3600) return `${Math.round(secs / 60)} min ago`;
  if (secs < 86400) return `${Math.round(secs / 3600)} h ago`;
  return `${Math.round(secs / 86400)} d ago`;
}

export function DeviceStatusBadge(props: { connectionId: string }) {
  return <Badge testID={`device-status:${props.connectionId}`} tone={deviceState(props.connectionId).tone} label={deviceState(props.connectionId).label} />;
}
