import { signal } from "nsh";
import { cancelLocalOpen, localAvailability, localError, localOpening, openLocalWorkspace, remember, restoreWorkspace } from "../localWorkspace";
import { connect, connectError, serverUrlInput, status } from "../store";
import { Action, Banner, Column, Divider, Field, FormRow, Label, Row, Surface } from "../ui";

export function ConnectScreen() {
  // Once per launch: offer the local workspace, and reopen it if it was the last one used.
  void restoreWorkspace();

  // The token lives in this component (and, once connected, in the store's
  // memory) only. It is never written to storage.
  const token = signal("");
  const connecting = () => status().phase === "connecting";
  const submit = () => {
    if (connecting()) return;
    cancelLocalOpen(); // the user chose a remote server: an open in flight doesn't sign in
    const value = token();
    void connect(serverUrlInput(), value).then(() => {
      if (connectError()) token.set(""); // don't keep a token that failed
      else if (status().phase === "online" || status().phase === "retrying") void remember("remote");
    });
  };
  const offerLocal = () => localAvailability() !== "absent";

  return (
    <Surface variant="panel" grow testID="connect-screen">
      <Column grow align="center" justify="center" pad={6}>
        <Surface variant="dialog" pad={6} gap={4}>
          <Column gap={1}>
            <Label variant="title">Connect to Metro</Label>
            <Label variant="body" color="muted">
              You'll see your personal Chat and the Spaces you own or belong to.
            </Label>
          </Column>

          <If when={offerLocal()}>
            <Column gap={2} testID="local-workspace">
              <Label variant="heading">On this computer</Label>
              <Label variant="body" color="muted">
                A private workspace on this computer. No account required.
              </Label>
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
                  onPress={() => void openLocalWorkspace()}
                />
              </Row>
            </Column>
            <Divider />
            <Label variant="heading">Or connect to a server</Label>
          </If>

          <FormRow label="Server URL" hint="http:// only for this machine (localhost, 127.0.0.1, ::1). Use https:// for remote servers.">
            <Field
              testID="server-url"
              accessibilityLabel="Server URL"
              value={serverUrlInput()}
              onChange={serverUrlInput.set}
              placeholder="http://127.0.0.1:8787"
              onSubmit={submit}
            />
          </FormRow>

          <FormRow label="Access token" hint="Kept in memory for this session only. Never saved to disk.">
            <Field
              testID="access-token"
              accessibilityLabel="Access token"
              value={token()}
              onChange={token.set}
              placeholder="Paste your bearer token"
              masked
              autoFocus={!offerLocal()}
              onSubmit={submit}
            />
          </FormRow>

          <If when={connectError()}>
            {(message) => (
              <Banner tone="danger" testID="connect-error">
                <Label color="danger">{message()}</Label>
              </Banner>
            )}
          </If>

          <Row justify="end">
            <Action
              testID="connect"
              label="Connect"
              busyLabel="Connecting…"
              busy={connecting()}
              variant={offerLocal() ? "secondary" : "primary"}
              onPress={submit}
            />
          </Row>
        </Surface>
      </Column>
    </Surface>
  );
}
