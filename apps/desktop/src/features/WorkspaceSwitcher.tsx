// Moving between workspaces from the account menu: "On this computer", the
// saved servers (the one in use checked), Add a server, Save this server and
// Manage saved servers. Switching never needs a sign-out first, and a
// message the user hasn't sent is never dropped without asking. See
// src/savedWorkspaces.ts for what is kept and where.

import { batch, signal } from "nsh";
import { localAvailability, onLocalWorkspace } from "../localWorkspace";
import {
  activeSaved,
  addressLabel,
  cancelSwitch,
  defaultName,
  forgetServer,
  hasToken,
  MAX_SERVER_NAME,
  renameServer,
  saveCurrentServer,
  savedError,
  saveServer,
  savedServers,
  serverCandidate,
  switching,
  switchTo,
  unsentDraft,
} from "../savedWorkspaces";
import type { SavedServer, WorkspaceTarget } from "../savedWorkspaces";
import { connectedTo } from "../store";
import { setStartupServer, startupError, startupServerId } from "../startupWorkspace";
import { Action, Banner, Column, Field, FormRow, Label, Modal, Row, SavedServerChoice, ScrollArea, Spacer, Toggle } from "../ui";
import type { AccountMenuItem } from "../ui";

type Dialog =
  | { kind: "draft"; target: WorkspaceTarget }
  | { kind: "token"; server: SavedServer }
  | { kind: "add" }
  | { kind: "checking" }
  | { kind: "startup" }
  | { kind: "save" }
  | { kind: "manage" };

const dialog = signal<Dialog | null>(null);

const targetName = (t: WorkspaceTarget) => (t.kind === "local" ? "On this computer" : t.server.name);
const active = (t: WorkspaceTarget) => (t.kind === "local" ? onLocalWorkspace() : activeSaved()?.id === t.server.id);

/** Switch, after the draft check and, for a saved server without a token in memory, the sign-in prompt. */
function request(target: WorkspaceTarget, draftChecked = false) {
  if (active(target)) return;
  if (!draftChecked && unsentDraft()) {
    dialog.set({ kind: "draft", target });
    return;
  }
  if (target.kind === "server" && !hasToken(target.server.baseUrl)) {
    dialog.set({ kind: "token", server: target.server });
    return;
  }
  const progress = { kind: "checking" } as const;
  dialog.set(progress);
  void switchTo(target).finally(() => {
    if (dialog() === progress) dialog.set(null);
  });
}

/** The Workspaces group of the account menu. Item ids are testIDs. */
export function workspaceItems(): AccountMenuItem[] {
  const group = "Workspaces";
  const items: AccountMenuItem[] = [];
  const url = connectedTo();
  if (localAvailability() === "available" || onLocalWorkspace()) {
    items.push({ id: "workspace-local", group, label: "On this computer", detail: "Private, no account", icon: "device", checked: onLocalWorkspace(), onPress: () => request({ kind: "local" }) });
  }
  for (const s of savedServers()) {
    items.push({
      id: `workspace-server:${s.id}`,
      group,
      label: s.name,
      detail: addressLabel(s.baseUrl),
      icon: "server",
      checked: activeSaved()?.id === s.id,
      onPress: () => request({ kind: "server", server: s }),
    });
  }
  const unsavedRemote = !!url && !onLocalWorkspace() && !activeSaved();
  if (unsavedRemote) {
    items.push({ id: "workspace-current", group, label: defaultName(url!), detail: "Not saved", icon: "server", checked: true, onPress: () => {} });
    items.push({ id: "save-server", group, label: "Save this server…", icon: "plus", onPress: () => dialog.set({ kind: "save" }) });
  }
  items.push({ id: "add-server", group, label: "Add a server…", icon: "plus", onPress: () => dialog.set({ kind: "add" }) });
  const startupName = savedServers().find((s) => s.id === startupServerId())?.name ?? "On this computer";
  items.push({ id: "startup-server", group, label: "Startup server…", detail: startupName, icon: "settings", onPress: () => {
    startupError.set(null);
    dialog.set({ kind: "startup" });
  } });
  if (savedServers().length > 0) items.push({ id: "manage-servers", group, label: "Manage saved servers…", icon: "settings", onPress: () => dialog.set({ kind: "manage" }) });
  return items;
}

/**
 * Under the account: a switch in progress, or why the last one didn't
 * happen (the user is still where they were). Dialogs show their own.
 */
export function WorkspaceNotice() {
  const line = (): { text: string; error: boolean } | null => {
    const to = switching();
    if (to) return { text: `Checking ${to}…`, error: false };
    const why = dialog() ? null : savedError();
    return why ? { text: why, error: true } : null;
  };
  return (
    <If when={line()}>
      {(l) => (
        <Row padX={2}>
          <Label variant="caption" color={l().error ? "danger" : "muted"} testID={l().error ? "workspace-error" : "workspace-switching"}>
            {l().text}
          </Label>
        </Row>
      )}
    </If>
  );
}

/** The dialogs the Workspaces items open. Mounted once, beside the account menu. */
export function WorkspaceDialogs() {
  const close = () => {
    batch(() => {
      cancelSwitch();
      dialog.set(null);
      savedError.set(null);
    });
  };
  return (
    <Modal open={dialog() !== null} title={titleOf(dialog())} onClose={close} size={dialog()?.kind === "startup" ? "large" : "compact"} testID="workspace-dialog">
      <Column pad={6} gap={4} grow={dialog()?.kind === "startup"}>
        <Label variant="title" heading>
          {titleOf(dialog())}
        </Label>
        <If when={dialog()?.kind === "draft" ? (dialog() as Extract<Dialog, { kind: "draft" }>) : null}>{(d) => <DraftQuestion target={d().target} onCancel={close} />}</If>
        <If when={dialog()?.kind === "token" ? (dialog() as Extract<Dialog, { kind: "token" }>) : null}>{(d) => <TokenForm server={d().server} onCancel={close} />}</If>
        <If when={dialog()?.kind === "add"}>
          <AddForm onCancel={close} />
        </If>
        <If when={dialog()?.kind === "save"}>
          <SaveForm onCancel={close} />
        </If>
        <If when={dialog()?.kind === "manage"}>
          <ManageList onDone={close} />
        </If>
        <If when={dialog()?.kind === "checking"}>
          <Label color="muted">{`Checking ${switching() ?? "server"}…`}</Label>
          <Action testID="workspace-cancel-switch" label="Cancel" variant="secondary" onPress={close} />
        </If>
        <If when={dialog()?.kind === "startup"}>
          <StartupPicker onDone={close} />
        </If>
      </Column>
    </Modal>
  );
}

function titleOf(d: Dialog | null): string {
  switch (d?.kind) {
    case "checking":
      return "Switch workspace";
    case "startup":
      return "Startup server";
    case "draft":
      return "Leave your unsent message?";
    case "token":
      return `Sign in to ${d.server.name}`;
    case "add":
      return "Add a server";
    case "save":
      return "Save this server";
    case "manage":
      return "Saved servers";
    default:
      return "Workspaces";
  }
}

function ErrorLine() {
  return (
    <If when={savedError()}>
      {(m) => (
        <Banner tone="danger" testID="workspace-error">
          <Label color="danger">{m()}</Label>
        </Banner>
      )}
    </If>
  );
}

function DraftQuestion(props: { target: WorkspaceTarget; onCancel: () => void }) {
  return (
    <Column gap={4}>
      <Label color="muted">{`You have a message you haven't sent. Switching to ${targetName(props.target)} discards it.`}</Label>
      <Row gap={2} justify="end">
        <Action testID="workspace-keep-draft" label="Keep writing" variant="secondary" onPress={props.onCancel} />
        <Action testID="workspace-discard-switch" label="Discard and switch" variant="danger" onPress={() => request(props.target, true)} />
      </Row>
    </Column>
  );
}

function StartupPicker(props: { onDone: () => void }) {
  const picker = dialog();
  const busy = signal(false);
  const select = async (id: string) => {
    if (busy()) return;
    busy.set(true);
    try {
      if (await setStartupServer(id) && dialog() === picker) props.onDone();
    } finally {
      busy.set(false);
    }
  };
  return (
    <Column grow gap={3}>
      <Label color="muted">Choose where Metro starts next time. Your current workspace stays open.</Label>
      <ScrollArea gap={2}>
        <If when={localAvailability() === "available" || onLocalWorkspace()}>
          <SavedServerChoice testID="startup-server:local" name="On this computer" address="Private local workspace" selected={startupServerId() === "local"} onPress={() => void select("local")} />
        </If>
        <Each of={savedServers()} key={(s) => s.id}>
          {(s) => <SavedServerChoice testID={`startup-server:${s().id}`} name={s().name} address={addressLabel(s().baseUrl)} selected={startupServerId() === s().id} onPress={() => void select(s().id)} />}
        </Each>
      </ScrollArea>
      <If when={startupError()}>{(m) => <Label color="danger">{m()}</Label>}</If>
      <Action label="Close" variant="secondary" onPress={props.onDone} />
    </Column>
  );
}

const KEEP_LABEL = "Stay signed in until Metro quits";

function TokenForm(props: { server: SavedServer; onCancel: () => void }) {
  const token = signal("");
  const keep = signal(false);
  const busy = () => switching() !== null;
  const submit = async () => {
    const value = token();
    if (!value.trim()) {
      savedError.set(`Enter your access token for ${props.server.name}.`);
      return;
    }
    // Checked before anything changes: a refused token keeps this dialog (and the current session) as they are.
    if (await switchTo({ kind: "server", server: props.server }, value, { keep: keep() })) {
      token.set("");
      dialog.set(null);
    }
  };
  return (
    <Column gap={3}>
      <Label variant="mono" color="muted">
        {addressLabel(props.server.baseUrl)}
      </Label>
      <FormRow label="Access token" hint="Used only here. Never saved to disk.">
        <Field testID="workspace-token" accessibilityLabel={`Access token for ${props.server.name}`} value={token()} onChange={token.set} placeholder="Paste your bearer token" masked autoFocus onSubmit={() => void submit()} />
      </FormRow>
      <Toggle testID="workspace-keep" label={KEEP_LABEL} showLabel checked={keep()} onChange={keep.set} />
      <Label variant="caption" color="muted">
        Metro checks the token first. Your current session ends only once it works.
      </Label>
      <ErrorLine />
      <Row gap={2} justify="end">
        <Action label="Cancel" variant="secondary" onPress={props.onCancel} />
        <Action testID="workspace-sign-in" label="Sign in" busyLabel="Checking…" busy={busy()} variant="primary" onPress={() => void submit()} />
      </Row>
    </Column>
  );
}

function AddForm(props: { onCancel: () => void }) {
  const name = signal("");
  const url = signal("");
  const token = signal("");
  const keep = signal(false);
  const busy = signal(false);
  const confirm = signal(false);
  const submit = async (discard = false) => {
    if (busy()) return;
    const value = token();
    if (!value.trim()) {
      savedError.set("Enter your access token for this server.");
      return;
    }
    const server = serverCandidate(url(), name());
    if (!server) return;
    if (!discard && unsentDraft()) {
      confirm.set(true);
      return;
    }
    confirm.set(false);
    busy.set(true);
    const form = dialog();
    try {
      if (await switchTo({ kind: "server", server }, value, { keep: keep() })) {
        await saveServer(server.baseUrl, server.name);
        token.set("");
        if (dialog() === form) dialog.set(null);
      }
    } finally {
      busy.set(false);
    }
  };
  return (
    <Column gap={3}>
      <If when={confirm()}>
        <Label color="muted">Signing in discards your unsent message and attachments.</Label>
        <Row gap={2} justify="end">
          <Action testID="workspace-keep-draft" label="Keep writing" variant="secondary" onPress={props.onCancel} />
          <Action testID="workspace-discard-switch" label="Discard and switch" variant="danger" onPress={() => void submit(true)} />
        </Row>
      </If>
      <If when={!confirm()}>
        <FormRow label="Name" hint="Optional display name.">
          <Field testID="add-server-name" accessibilityLabel="Server name" value={name()} onChange={name.set} placeholder="Work" maxLength={MAX_SERVER_NAME} />
        </FormRow>
        <FormRow label="Server URL" hint="Use HTTPS, or HTTP on loopback.">
          <Field testID="add-server-url" accessibilityLabel="Server URL" value={url()} onChange={url.set} placeholder="https://metro.example.org" autoFocus />
        </FormRow>
        <FormRow label="Access token" hint="Never saved to disk.">
          <Field testID="add-server-token" accessibilityLabel="Access token" value={token()} onChange={token.set} placeholder="Paste your bearer token" masked onSubmit={() => void submit()} />
        </FormRow>
        <Toggle testID="add-server-keep" label={KEEP_LABEL} showLabel checked={keep()} onChange={keep.set} />
        <Label variant="caption" color="muted">
          Only the verified server’s name and address are saved.
        </Label>
        <ErrorLine />
        <Row gap={2} justify="end">
          <Action label="Cancel" variant="secondary" onPress={props.onCancel} />
          <Action testID="add-server-submit" label="Save and sign in" busyLabel="Checking…" busy={busy()} variant="primary" onPress={() => void submit()} />
        </Row>
      </If>
    </Column>
  );
}

function SaveForm(props: { onCancel: () => void }) {
  const name = signal(defaultName(connectedTo() ?? ""));
  const keep = signal(false);
  const submit = async () => {
    if (await saveCurrentServer(name(), { keep: keep() })) dialog.set(null);
  };
  return (
    <Column gap={3}>
      <Label variant="mono" color="muted">
        {addressLabel(connectedTo() ?? "")}
      </Label>
      <FormRow label="Name" hint="Only the name and address are saved on this computer, never your token.">
        <Field testID="save-server-name" accessibilityLabel="Server name" value={name()} onChange={name.set} maxLength={MAX_SERVER_NAME} autoFocus onSubmit={() => void submit()} />
      </FormRow>
      <Toggle testID="save-server-keep" label={KEEP_LABEL} showLabel checked={keep()} onChange={keep.set} />
      <ErrorLine />
      <Row gap={2} justify="end">
        <Action label="Cancel" variant="secondary" onPress={props.onCancel} />
        <Action testID="save-server-submit" label="Save" variant="primary" onPress={() => void submit()} />
      </Row>
    </Column>
  );
}

function ManageList(props: { onDone: () => void }) {
  return (
    <Column gap={3}>
      <Each of={savedServers()} key={(s) => s.id}>
        {(s) => <ManageRow server={s()} />}
      </Each>
      <If when={savedServers().length === 0}>
        <Label color="muted">No saved servers.</Label>
      </If>
      <Label variant="caption" color="muted">
        Forget removes a server from this list and its token from memory. A session on it carries on until you leave it.
      </Label>
      <Row justify="end">
        <Action testID="manage-servers-done" label="Done" variant="primary" onPress={props.onDone} />
      </Row>
    </Column>
  );
}

function ManageRow(props: { server: SavedServer }) {
  const name = signal(props.server.name);
  const commit = () => {
    if (name().trim() !== props.server.name) void renameServer(props.server.id, name());
  };
  return (
    <Column gap={1} testID={`manage-server:${props.server.id}`}>
      <Row align="center" gap={2}>
        <Field testID={`rename-server:${props.server.id}`} accessibilityLabel={`Name of ${addressLabel(props.server.baseUrl)}`} value={name()} onChange={name.set} maxLength={MAX_SERVER_NAME} grow onSubmit={commit} />
        <Action testID={`rename-server-save:${props.server.id}`} label="Rename" variant="secondary" onPress={commit} />
        <Spacer />
        <Action testID={`forget-server:${props.server.id}`} label="Forget" variant="destructive" onPress={() => void forgetServer(props.server.id)} />
      </Row>
      <Label variant="caption" color="muted">
        {addressLabel(props.server.baseUrl)}
        {activeSaved()?.id === props.server.id ? " · in use now" : ""}
      </Label>
    </Column>
  );
}
