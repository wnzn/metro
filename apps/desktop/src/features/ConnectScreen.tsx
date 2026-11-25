import { cleanup, effect, keymap, Scroll, signal, View } from "nsh";
import type { JSX, ViewRef } from "nsh";
import { localAvailability, localError, localOpening, openLocalWorkspace } from "../localWorkspace";
import { pendingPlainHttp } from "../plainHttp";
import { addressLabel, cancelSwitch, confirmPlainHttp, defaultName, hasToken, saveCurrentServer, savedError, savedServers, switchTo } from "../savedWorkspaces";
import type { SavedServer } from "../savedWorkspaces";
import { chooseManually, startupOpening, startupPrompt } from "../startupWorkspace";
import { connect, connectError, serverUrlInput, sessionGuard, status } from "../store";
import { PlainHttpConsent, SERVER_URL_HINT } from "./PlainHttpConsent";
import { Action, Banner, Column, Divider, Field, FormRow, Label, MetroMark, Row, SavedServerChoice, Surface, Toggle } from "../ui";

/**
 * The first connect screen of a run is quiet when servers are saved: the
 * local workspace and the saved servers first, the address and token form
 * only on request (or when a saved server needs its token). After a sign-out
 * or a failure, and on a first visit with nothing saved, the form shows as
 * it always did.
 */
let launchScreen = true;

type SubmitControl = "button" | "url" | "token";

export function ConnectScreen() {
  const quiet = launchScreen;
  launchScreen = false;
  const formOpen = signal(false);
  const selected = signal<SavedServer | null>(null);
  const target = () => selected() ?? startupPrompt();
  const showForm = () => !!target() || formOpen() || !quiet || savedServers().length === 0 || !!connectError();

  // The token lives in this component (and, once connected, in the store's
  // memory) only. It is never written to storage.
  const token = signal("");
  /** Add this server to the saved list once signed in (its name and address only). */
  const save = signal(true);
  /** Keep the token in memory until Metro quits, for switching back without pasting it again. Off unless asked. */
  const keep = signal(false);
  const connecting = () => status().phase === "connecting";
  /** A plain-HTTP address waiting for the person's one-time confirmation. */
  const consent = signal<{ url: string; opener: SubmitControl } | null>(null);
  let content: ViewRef | undefined;
  const controls: Partial<Record<SubmitControl, ViewRef>> = {};
  let restoreConnect = false;
  let intent = 0;
  cleanup(() => { ++intent; });
  const revealAndFocus = (node: ViewRef, current: () => boolean, keepBottomPadding = false) => {
    // Wait for the expanded form (or the returning control) to lay out.
    void node.measure().then((layout) => {
      if (!layout || !current()) return;
      // The warning is at the end; reveal it with the card's bottom padding
      // so the buttons and their focus rings aren't on the viewport edge.
      if (keepBottomPadding) content?.reveal({ vertical: "end", animated: false });
      else node.reveal({ vertical: "nearest", animated: false });
      node.focus();
    });
  };
  const manual = () => {
    ++intent;
    cancelSwitch();
    chooseManually();
    savedError.set(null);
    consent.set(null);
  };
  const cancelConsent = () => {
    const pending = consent();
    if (!pending) return;
    const mine = ++intent;
    if (pending.opener === "button") restoreConnect = true;
    consent.set(null);
    if (pending.opener !== "button") {
      const opener = controls[pending.opener];
      if (opener) revealAndFocus(opener, () => mine === intent);
    }
  };
  effect(() => {
    if (!consent()) return;
    return keymap.bind("escape", cancelConsent, { when: "always" });
  });
  const submit = (opener: SubmitControl) => {
    if (connecting() || consent()) return;
    selected.set(target());
    manual();
    const value = token();
    const url = (target()?.baseUrl ?? serverUrlInput()).trim();
    const unconfirmed = pendingPlainHttp(url);
    if (unconfirmed) {
      // Asked before the token goes anywhere; nothing is sent until confirmed.
      connectError.set(null);
      consent.set({ url: unconfirmed, opener });
      return;
    }
    const saving = !!target() || save();
    const keeping = saving && keep();
    const signingIn = connect(url, value);
    const sameSession = sessionGuard();
    void signingIn.then((current) => {
      if (connectError()) token.set(""); // don't keep a token that failed
      else if (current && sameSession()) {
        // An already-saved address keeps its name; the token is kept only when asked.
        if (saving) void saveCurrentServer(defaultName(url), { keep: keeping, keepName: true });
      }
    });
  };
  /** A saved server: straight in when its token is in memory (this run); else its address goes in the form. */
  const choose = async (s: SavedServer) => {
    if (connecting()) return;
    manual();
    const mine = intent;
    selected.set(s);
    serverUrlInput.set(s.baseUrl);
    token.set("");
    if (hasToken(s.baseUrl)) {
      if (await switchTo({ kind: "server", server: s }) || mine !== intent) return;
    }
    formOpen.set(true);
  };
  const other = () => {
    manual();
    selected.set(null);
    token.set("");
    formOpen.set(true);
    connectError.set(null);
  };
  const offerLocal = () => !target() && localAvailability() !== "absent";

  return (
    <Surface variant="panel" grow testID="connect-screen">
      <ConnectViewport contentRef={(node) => { content = node; }}>
        <If when={!startupOpening()} else={<Column gap={3} align="center">
          <MetroMark size="lg" />
          <Label variant="strong">Opening workspace…</Label>
          <Action testID="startup-choose-manually" label="Choose a server" variant="ghost" onPress={other} />
        </Column>}>
        <Surface variant="dialog" pad={6} gap={4}>
          <Column gap={3}>
            <MetroMark size="lg" />
            <Column gap={1}>
              <Label variant="title" heading>
                {target() ? `Sign in to ${target()!.name}` : "Connect to Metro"}
              </Label>
            </Column>
          </Column>

          <If when={offerLocal()}>
            <Column gap={2} testID="local-workspace">
              <Label variant="heading">On this computer</Label>
              <If when={localAvailability() === "unavailable"}>
                <Label variant="body" color="muted" testID="local-unavailable">
                  This Metro install doesn't include the local server.
                </Label>
              </If>
              <If when={localAvailability() === "unreachable"}>
                <Label variant="body" color="muted" testID="local-unreachable">
                  Metro's helper on this computer didn't answer. Restart Metro to use it locally.
                </Label>
              </If>
              <If when={localError()}>
                {(message) => (
                  <Banner tone="danger" testID="local-error">
                    <Label color="danger">{message()}</Label>
                  </Banner>
                )}
              </If>
              <Row justify="start">
                <Action
                  testID="open-local"
                  accessibilityLabel="Use locally"
                  label="Use locally"
                  busyLabel="Opening…"
                  busy={localOpening()}
                  disabled={localAvailability() !== "available" || connecting()}
                  variant="primary"
                  onPress={() => { manual(); void openLocalWorkspace(); }}
                />
              </Row>
            </Column>
            <Divider />
            <Label variant="heading">Or connect to a server</Label>
          </If>

          <If when={savedServers().length > 0 && !startupPrompt()}>
            <Column gap={1} testID="saved-servers">
              <Label variant="strong">Saved servers</Label>
              <Each of={savedServers()} key={(s) => s.id}>
                {(s) => (
                  <SavedServerChoice
                    testID={`saved-server:${s().id}`}
                    name={s().name}
                    address={addressLabel(s().baseUrl)}
                    selected={serverUrlInput().trim() === s().baseUrl}
                    note={hasToken(s().baseUrl) ? "Signed in this session" : undefined}
                    onPress={() => choose(s())}
                  />
                )}
              </Each>
            </Column>
          </If>

          <If when={!showForm()}>
            <Row justify="start">
              <Action testID="show-server-form" label="Connect to another server" variant="ghost" onPress={other} />
            </Row>
          </If>

          <If when={showForm()}>
            <Column gap={4} testID="server-form-fields">
              <FormRow label="Server URL" hint={target() ? undefined : SERVER_URL_HINT}>
                <Field
                  testID="server-url"
                  accessibilityLabel="Server URL"
                  value={target()?.baseUrl ?? serverUrlInput()}
                  disabled={!!target() || connecting()}
                  onChange={(next) => { if (target()) return; manual(); token.set(""); serverUrlInput.set(next); }}
                  placeholder="http://127.0.0.1:8787"
                  inputRef={(node) => { controls.url = node; }}
                  onSubmit={() => submit("url")}
                />
              </FormRow>

              <FormRow label="Access token" hint="Never saved to disk.">
                <Field
                  testID="access-token"
                  accessibilityLabel="Access token"
                  value={token()}
                  onChange={(next) => {
                    // A late preference read must never change the address
                    // after a person has started typing a credential for it.
                    selected.set(target());
                    manual();
                    token.set(next);
                  }}
                  placeholder="Paste your bearer token"
                  masked
                  autoFocus={!offerLocal() || formOpen()}
                  inputRef={(node) => { controls.token = node; }}
                  onSubmit={() => submit("token")}
                />
              </FormRow>

              <If when={!target()}>
                <Toggle testID="save-server" label="Save this server" showLabel checked={save()} onChange={save.set} />
              </If>
              <If when={target() || save()}>
                <Toggle testID="keep-signed-in" label="Keep token until quit" showLabel checked={keep()} onChange={keep.set} />
              </If>

              <If when={connectError() || savedError()}>
                {(message) => (
                  <Banner tone="danger" testID="connect-error">
                    <Label color="danger">{message()}</Label>
                  </Banner>
                )}
              </If>

              <If when={consent()}>
                {(pending) => {
                  const shown = pending();
                  const mine = intent;
                  return (
                    <PlainHttpConsent
                      testID="plain-http-consent"
                      baseUrl={shown.url}
                      cancelRef={(node) => revealAndFocus(node, () => mine === intent && consent() === shown, true)}
                      onCancel={cancelConsent}
                      onAccept={() => {
                        if (consent() !== shown) return;
                        consent.set(null);
                        void confirmPlainHttp(shown.url);
                        submit(shown.opener);
                      }}
                    />
                  );
                }}
              </If>

              <If when={!consent()}>
                <Row justify="end">
                  <If when={target()}><Action testID="choose-other-server" label="Choose another server" variant="ghost" onPress={other} disabled={connecting()} /></If>
                  <Action
                    testID="connect"
                    label="Connect"
                    busyLabel="Connecting…"
                    busy={connecting()}
                    variant={offerLocal() ? "secondary" : "primary"}
                    viewRef={(node) => {
                      if (!restoreConnect) return;
                      restoreConnect = false;
                      const mine = intent;
                      revealAndFocus(node, () => mine === intent);
                    }}
                    onPress={() => submit("button")}
                  />
                </Row>
              </If>
            </Column>
          </If>
        </Surface>
        </If>
      </ConnectViewport>
    </Surface>
  );
}

function ConnectViewport(props: { contentRef: (node: ViewRef) => void; children?: JSX.Element }) {
  const height = signal(0);
  return (
    <Scroll class="flex-1 min-h-0" testID="connect-scroll" onLayout={({ bounds }) => height.set(bounds.size.height)}>
      {/* Center the card when it fits; give overflow a real scroll range, not a negative top edge. */}
      <View ref={props.contentRef} class="flex-col justify-center" style={{ minHeight: height() }}>
        <Column align="center" pad={6}>{props.children}</Column>
      </View>
    </Scroll>
  );
}
