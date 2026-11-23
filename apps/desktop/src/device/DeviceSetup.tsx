// In-app setup of a device connection on this computer, in the packaged app:
// explicit consent, then pairing (the launcher's bridge gets the device token
// from the server; this UI never sees it), the vendor sign-in or key, and the
// approved models. Then re-pair, sign out or forget. No terminal, no source
// build, no token to copy.
//
// A vendor key typed here lives in this panel's signal until it is sent to
// the bridge or the panel closes, and is cleared either way.

import { cleanup, effect, signal, untracked } from "nsh";
import { connectedTo } from "../store";
import { fetchModels, modelListings, providerById } from "../settings";
import { Action, ActionBar, Badge, Banner, Column, Field, Row, SettingRow, SettingsGroup, Toggle } from "../ui";
import { isEndpointKind } from "./bridge";
import type { LocalConnection } from "./bridge";
import {
  approveModelsHere,
  availableModels,
  clearPairingMessages,
  forgetHere,
  helper,
  helperKinds,
  listModelsHere,
  localBadge,
  localConnections,
  localStep,
  manageLoginHere,
  pairBusy,
  pairError,
  pairHere,
  pairNotice,
  refreshHelper,
  rotateHere,
  setEndpointHere,
  signInHere,
  signOutHere,
} from "./state";

const SIGN_IN_POLL_MS = 1500;

/** What signing in looks like for a kind, in a sentence for the consent. */
function credentialWords(kind: string): string {
  if (kind === "device-openai-oauth") return "You sign in with ChatGPT in your browser on this computer. The sign-in stays here; Metro's server never receives it.";
  if (kind === "device-claude-code") return "Your Anthropic API key is saved on this computer only and used with the Claude Code CLI installed here. That is API billing; Claude Pro/Max subscriptions are not used.";
  return "The endpoint's URL and key are saved on this computer only, bound to that endpoint. Metro's server never receives the key.";
}

/** The status of Metro's device helper itself, when it can't serve this session. Renders nothing when it can. */
export function HelperStatus() {
  const h = () => helper();
  const text = (): string | null => {
    const v = h();
    switch (v.phase) {
      case "absent":
        return null;
      case "ready":
        return null;
      case "checking":
        return "Checking Metro's device helper on this computer…";
      case "secondary":
        return "Another Metro window on this computer already runs your device connections. Use that window, or close it and open Metro again.";
      case "unbound":
      case "unreachable":
        return v.message;
    }
  };
  return (
    <If when={text()}>
      {(t) => (
        <Banner
          variant="inline"
          tone={h().phase === "checking" ? "neutral" : "warning"}
          testID="device-helper-status"
          message={t()}
          action={h().phase !== "checking" ? <Action testID="device-helper-retry" label="Check again" variant="ghost" onPress={() => void refreshHelper()} /> : undefined}
        />
      )}
    </If>
  );
}

/** For a provider row: the connection's state on this computer. Packaged app only (render it under `packaged()`). */
export function LocalDeviceBadge(props: { connectionId: string }) {
  const badge = () => localBadge(localConnections()[props.connectionId]);
  return <Badge testID={`device-local:${props.connectionId}`} tone={badge().tone} label={badge().label} />;
}

/** The whole setup page for one device connection. */
export function DeviceSetup(props: { connectionId: string; connectionName: string; providerId: string }) {
  const local = () => localConnections()[props.connectionId];
  const step = () => localStep(local());
  const ready = () => helper().phase === "ready";
  const supported = () => helperKinds().includes(props.providerId);
  clearPairingMessages();

  // While the browser sign-in is open, follow it.
  effect(() => {
    if (step() !== "signing-in") return;
    const timer = setInterval(() => void refreshHelper(), SIGN_IN_POLL_MS);
    cleanup(() => clearInterval(timer));
  });
  // Once signed in, ask the provider (from this computer) what it offers.
  effect(() => {
    if (step() !== "models" && step() !== "ready") return;
    untracked(() => {
      if (!availableModels()[props.connectionId]) void listModelsHere(props.connectionId);
    });
  });

  return (
    <Column gap={6} testID="device-setup">
      <HelperStatus />
      <Messages />
      <If when={ready() && !supported()}>
        <Banner
          variant="inline"
          tone="warning"
          testID="device-unsupported"
          message={`Metro's device helper in this app can't run ${providerById(props.providerId)?.name ?? props.providerId}. Pick another provider for this computer.`}
        />
      </If>
      <If when={ready() && supported()}>
        <Column gap={6}>
          <If when={step() === "pair"}>
            <Consent connectionId={props.connectionId} connectionName={props.connectionName} kind={props.providerId} />
          </If>
          <If when={step() === "credentials"}>
            <Credentials connectionId={props.connectionId} kind={props.providerId} local={local()} />
          </If>
          <If when={step() === "signing-in"}>
            <SigningIn connectionId={props.connectionId} local={local()} />
          </If>
          <If when={step() === "models"}>
            <Models connectionId={props.connectionId} current={[]} />
          </If>
          <If when={step() === "ready" ? local() : null}>{(c) => <Ready connection={c()} connectionName={props.connectionName} />}</If>
        </Column>
      </If>
    </Column>
  );
}

function Messages() {
  return (
    <Column gap={2}>
      <If when={pairError()}>{(e) => <Banner variant="inline" tone="danger" testID="device-setup-error" message={e()} />}</If>
      <If when={pairNotice()}>{(n) => <Banner variant="inline" tone="success" testID="device-setup-notice" message={n()} />}</If>
    </Column>
  );
}

function Consent(props: { connectionId: string; connectionName: string; kind: string }) {
  const agreed = signal(false);
  const baseUrl = signal("");
  const endpoint = isEndpointKind(props.kind);
  const server = () => connectedTo() ?? "this Metro server";
  const pair = () => {
    if (!agreed()) return;
    void pairHere(props.connectionId, props.kind, endpoint ? baseUrl() : undefined);
  };
  return (
    <SettingsGroup
      title="Pair this computer"
      description="Read what pairing means, then agree. Nothing is issued until you press Pair this computer."
      testID="device-consent"
    >
      <SettingRow
        label="A device token, kept here"
        description={`Metro's device helper on this computer gets a device token for “${props.connectionName}” from ${server()} and keeps it on this computer. You never see or copy it.`}
      />
      <SettingRow
        label="Requests pass through this computer"
        description="While Metro is open and signed in, requests sent through this connection (yours, and those of anyone you share it with) and their answers pass through this computer, using your sign-in or key and your credits."
      />
      <SettingRow label="Your sign-in or key stays here" description={credentialWords(props.kind)} />
      <SettingRow label="You stay in control" description="Only the models you approve here can be used. You can sign out, re-pair or forget this computer at any time." />
      <If when={endpoint}>
        <SettingRow label="Endpoint URL" description="Your local or private OpenAI-compatible endpoint, as this computer reaches it." stacked>
          <Field
            testID="device-endpoint-url"
            accessibilityLabel="Endpoint URL"
            value={baseUrl()}
            onChange={baseUrl.set}
            placeholder="http://127.0.0.1:8000/v1"
            maxLength={2048}
          />
        </SettingRow>
      </If>
      <SettingRow label={`I agree to pair this computer for “${props.connectionName}”`}>
        <Toggle testID="device-consent-agree" label={`I agree to pair this computer for “${props.connectionName}”`} checked={agreed()} onChange={agreed.set} />
      </SettingRow>
      <Row padX={4} padY={3} justify="end">
        <Action
          testID="device-pair"
          label="Pair this computer"
          busyLabel="Pairing…"
          busy={pairBusy() === `pair:${props.connectionId}`}
          disabled={!agreed() || !!pairBusy() || (endpoint && !baseUrl().trim())}
          variant="primary"
          onPress={pair}
        />
      </Row>
    </SettingsGroup>
  );
}

function Credentials(props: { connectionId: string; kind: string; local: LocalConnection | undefined }) {
  const key = signal("");
  const keyless = signal(false);
  cleanup(() => key.set(""));
  const failed = () => {
    const login = props.local?.login;
    if (login?.state !== "failed") return null;
    if (!login.beforeBrowser) return login.error ?? "Start a new sign-in and finish it in your browser.";
    const help = login.stage === "discovery" ? "Check that this computer can reach OpenAI, then try again."
      : login.stage === "callback" ? "Close any other ChatGPT sign-in on this computer, then try again."
      : login.stage === "storage" ? "Check that Metro can save files on this computer, then try again."
      : "Try signing in again.";
    return ["No browser was opened.", login.error, help].filter(Boolean).join(" ");
  };
  const save = async () => {
    const value = key().trim();
    key.set(""); // never kept, whatever the outcome
    if (isEndpointKind(props.kind) && keyless()) {
      await signInHere(props.connectionId, { no_api_key: true });
      return;
    }
    if (!value) return;
    await signInHere(props.connectionId, { api_key: value });
  };
  return (
    <Column gap={4}>
      <If when={failed()}>
        {(f) => (
          <Banner variant="inline" tone="warning" testID="device-signin-failed" message={`Sign-in didn't finish: ${f()}`} />
        )}
      </If>
      <If
        when={props.kind === "device-openai-oauth"}
        else={
          <SettingsGroup
            title={props.kind === "device-claude-code" ? "Anthropic API key" : "Endpoint key"}
            description={
              props.kind === "device-claude-code"
                ? "Saved on this computer only, for the Claude Code CLI installed here. Metro's server never receives it."
                : `Saved on this computer only, bound to ${props.local?.baseUrl ?? "this endpoint"}. Metro's server never receives it.`
            }
            testID="device-credentials"
          >
            <SettingRow label="API key" stacked>
              <Field
                testID="device-api-key"
                accessibilityLabel="API key, saved on this computer only"
                value={key()}
                onChange={key.set}
                placeholder={props.kind === "device-claude-code" ? "sk-ant-api…" : "API key"}
                masked
                disabled={keyless()}
                onSubmit={() => void save()}
              />
            </SettingRow>
            <If when={isEndpointKind(props.kind)}>
              <SettingRow label="This endpoint needs no key" description="For a local server that takes no key.">
                <Toggle testID="device-no-key" label="This endpoint needs no key" checked={keyless()} onChange={keyless.set} />
              </SettingRow>
            </If>
            <Row padX={4} padY={3} justify="end">
              <Action
                testID="device-save-key"
                label={keyless() ? "Continue without a key" : "Save key on this computer"}
                busyLabel="Saving…"
                busy={pairBusy() === `signin:${props.connectionId}`}
                disabled={!!pairBusy() || (!keyless() && !key().trim())}
                variant="primary"
                onPress={() => void save()}
              />
            </Row>
          </SettingsGroup>
        }
      >
        <SettingsGroup
          title="Sign in with ChatGPT"
          description="Your browser opens on this computer. Approve the sign-in there; Metro's server never receives it, and Metro never asks you to paste a code or token."
          testID="device-credentials"
        >
          <Row padX={4} padY={3} justify="end">
            <Action
              testID="device-sign-in"
              label={failed() ? "Retry ChatGPT sign-in" : "Sign in with ChatGPT"}
              busyLabel="Preparing sign-in…"
              busy={pairBusy() === `signin:${props.connectionId}`}
              disabled={!!pairBusy()}
              variant="primary"
              onPress={() => void signInHere(props.connectionId, { open_browser: true })}
            />
          </Row>
        </SettingsGroup>
      </If>
    </Column>
  );
}

function SigningIn(props: { connectionId: string; local: LocalConnection | undefined }) {
  const login = () => props.local?.login;
  const description = () => {
    const l = login();
    if (!l?.browserReady) return "Preparing sign-in on this computer…";
    if (l.browser === "failed") return `${l.browserError ?? "The browser didn't open."} Check your default browser, then open it again.`;
    if (l.browser === "opening") return "Opening your default browser…";
    if (l.browser === "opened") return "Approve the sign-in in your browser on this computer. This page continues by itself when it's done.";
    return "Open your browser to continue signing in.";
  };
  return (
    <SettingsGroup title="Finish signing in" description={description()} testID="device-signing-in">
      <Row padX={4} padY={3} gap={2} wrap>
        <If when={login()?.browserReady && login()?.browser && login()?.browser !== "opening"}>
          <Action testID="device-open-signin" label="Open browser" variant="ghost" disabled={!!pairBusy()} onPress={() => void manageLoginHere(props.connectionId, "open")} />
        </If>
        <Action testID="device-cancel-signin" label="Cancel sign-in" variant="ghost" disabled={!!pairBusy()} onPress={() => void manageLoginHere(props.connectionId, "cancel")} />
        <Action testID="device-signin-check" label="Check now" variant="ghost" onPress={() => void refreshHelper()} />
      </Row>
    </SettingsGroup>
  );
}

function Models(props: { connectionId: string; current: string[]; onDone?: () => void }) {
  const chosen = signal<string[]>(props.current);
  const manual = signal("");
  const list = () => availableModels()[props.connectionId];
  const toggle = (m: string, on: boolean) => chosen.set(on ? [...new Set([...chosen(), m])] : chosen().filter((x) => x !== m));
  const offered = () => {
    const l = list();
    const fromProvider = l?.state === "ok" ? l.models : [];
    return [...new Set([...fromProvider, ...chosen()])];
  };
  const addManual = () => {
    const m = manual().trim();
    if (!m) return;
    toggle(m, true);
    manual.set("");
  };
  const save = async () => {
    if (await approveModelsHere(props.connectionId, chosen())) props.onDone?.();
  };
  return (
    <SettingsGroup
      title="Approve models"
      description="Metro can use only the models you approve here, from this computer. You can change them later."
      testID="device-models"
    >
      <If when={list()?.state === "loading"}>
        <SettingRow label="Asking the provider for its models, from this computer…" />
      </If>
      <If when={list()?.state === "error" ? (list() as { message: string }).message : null}>
        {(m) => (
          <Column padX={4} padY={2}>
            <Banner variant="inline" tone="warning" testID="device-models-error" message={`${m()} Enter a model ID below instead.`} />
          </Column>
        )}
      </If>
      <Each of={offered()} key={(m) => m}>
        {(m) => (
          <SettingRow label={m()}>
            <Toggle testID={`device-model:${m()}`} label={`Approve ${m()}`} checked={chosen().includes(m())} onChange={(on) => toggle(m(), on)} />
          </SettingRow>
        )}
      </Each>
      <SettingRow label="Another model ID" stacked>
        <Row gap={2} align="center">
          <Field testID="device-model-manual" accessibilityLabel="Model ID to approve" value={manual()} onChange={manual.set} placeholder="model-id" grow maxLength={200} onSubmit={addManual} />
          <Action testID="device-model-add" label="Add" variant="ghost" disabled={!manual().trim()} onPress={addManual} />
        </Row>
      </SettingRow>
      <Row padX={4} padY={3} gap={2} justify="end">
        <Action testID="device-models-refresh" label="List again" variant="ghost" disabled={list()?.state === "loading"} onPress={() => void listModelsHere(props.connectionId)} />
        <Action
          testID="device-models-save"
          label={chosen().length === 0 ? "Approve models" : `Approve ${chosen().length} model${chosen().length === 1 ? "" : "s"}`}
          busyLabel="Saving…"
          busy={pairBusy() === `models:${props.connectionId}`}
          disabled={!!pairBusy() || chosen().length === 0}
          variant="primary"
          onPress={() => void save()}
        />
      </Row>
    </SettingsGroup>
  );
}

const WORKER_TEXT: Record<string, string> = {
  online: "Serving: this computer answers this connection's requests while Metro is open.",
  starting: "Starting…",
  offline: "Not reaching the Metro server right now. It retries by itself.",
  stopped: "Stopped on this computer.",
  needs_setup: "Needs setup on this computer.",
  unauthorized: "The Metro server no longer accepts this computer's device token. Re-pair it.",
  error: "Stopped after an error.",
};

function Ready(props: { connection: LocalConnection; connectionName: string }) {
  const id = () => props.connection.connectionId;
  const editing = signal(false);
  const confirm = signal<null | "rotate" | "forget" | "signout">(null);
  const newUrl = signal("");
  const changingUrl = signal(false);
  const listing = () => modelListings()[id()];
  const badge = () => localBadge(props.connection);
  return (
    <Column gap={6}>
      <SettingsGroup title="On this computer" testID="device-ready">
        <SettingRow label="State" description={`${WORKER_TEXT[props.connection.worker] ?? props.connection.worker}${props.connection.detail ? ` ${props.connection.detail}` : ""}`}>
          <Badge testID={`device-local-state:${id()}`} tone={badge().tone} label={badge().label} />
        </SettingRow>
        <If when={props.connection.baseUrl}>
          {(u) => (
            <SettingRow label="Endpoint" description={u()}>
              <Action testID="device-endpoint-change" label={changingUrl() ? "Cancel" : "Change…"} variant="ghost" onPress={() => changingUrl.set(!changingUrl())} />
            </SettingRow>
          )}
        </If>
        <If when={changingUrl()}>
          <SettingRow label="New endpoint URL" description="Changing it deletes the key saved for the old one; you enter a key again." stacked>
            <Row gap={2} align="center">
              <Field testID="device-endpoint-new" accessibilityLabel="New endpoint URL" value={newUrl()} onChange={newUrl.set} grow maxLength={2048} />
              <Action
                testID="device-endpoint-save"
                label="Save"
                busy={pairBusy() === `endpoint:${id()}`}
                disabled={!newUrl().trim() || !!pairBusy()}
                onPress={() => void setEndpointHere(id(), newUrl()).then((ok) => ok && changingUrl.set(false))}
              />
            </Row>
          </SettingRow>
        </If>
        <SettingRow label="Approved models" description={props.connection.models.join(", ")}>
          <Action testID="device-models-edit" label={editing() ? "Close" : "Change…"} variant="ghost" onPress={() => editing.set(!editing())} />
        </SettingRow>
        <SettingRow label="Test connection" description="Asks the Metro server for this connection's models; it answers through this computer while it's serving.">
          <Action testID="device-test" label="Test connection" busyLabel="Testing…" busy={listing()?.state === "loading"} onPress={() => void fetchModels(id())} />
        </SettingRow>
      </SettingsGroup>

      <If when={editing()}>
        <Models connectionId={id()} current={props.connection.models} onDone={() => editing.set(false)} />
      </If>

      <SettingsGroup title="This computer" description="These change this computer only, except Forget, which also revokes its device token.">
        <Row padX={4} padY={3} gap={2} wrap>
          <Action testID="device-signout" label="Sign out here…" disabled={!!pairBusy()} onPress={() => confirm.set("signout")} />
          <Action testID="device-rotate" label="Re-pair…" disabled={!!pairBusy()} onPress={() => confirm.set("rotate")} />
          <Action testID="device-forget" label="Forget this computer…" variant="destructive" disabled={!!pairBusy()} onPress={() => confirm.set("forget")} />
        </Row>
        <If when={confirm()}>
          {(what) => (
            <Column padX={4} padY={3} gap={2} testID={`device-confirm-${what()}`}>
              <Banner
                variant="inline"
                tone="warning"
                message={
                  what() === "rotate"
                    ? `Re-pair “${props.connectionName}”? This computer gets a new device token and the old one stops working at once.`
                    : what() === "forget"
                      ? `Forget this computer for “${props.connectionName}”? Its sign-in or key, approved models and device token are deleted here, and the token is revoked, so nothing serves this connection until you set it up again. The connection itself stays.`
                      : `Sign out of the provider on this computer? It stops serving “${props.connectionName}” until you sign in again. The pairing stays.`
                }
              />
              <ActionBar>
                <Action label="Cancel" variant="ghost" onPress={() => confirm.set(null)} />
                <Action
                  testID={`device-confirm-${what()}-now`}
                  label={what() === "rotate" ? "Re-pair" : what() === "forget" ? "Forget and revoke" : "Sign out"}
                  variant="danger"
                  busy={!!pairBusy()}
                  onPress={() => {
                    const w = what();
                    confirm.set(null);
                    if (w === "rotate") void rotateHere(id());
                    else if (w === "forget") void forgetHere(id(), true);
                    else void signOutHere(id());
                  }}
                />
              </ActionBar>
            </Column>
          )}
        </If>
      </SettingsGroup>
    </Column>
  );
}
