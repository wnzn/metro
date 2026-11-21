import { signal } from "nsh";
import { connect, connectError, serverUrlInput, status } from "../store";
import { Action, Banner, Column, Field, FormRow, Label, Row, Surface } from "../ui";

export function ConnectScreen() {
  // The token lives in this component (and, once connected, in the store's
  // memory) only. It is never written to storage.
  const token = signal("");
  const connecting = () => status().phase === "connecting";
  const submit = () => {
    if (connecting()) return;
    const value = token();
    void connect(serverUrlInput(), value).then(() => {
      if (connectError()) token.set(""); // don't keep a token that failed
    });
  };

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
              autoFocus
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
            <Action testID="connect" label="Connect" busyLabel="Connecting…" busy={connecting()} variant="primary" onPress={submit} />
          </Row>
        </Surface>
      </Column>
    </Surface>
  );
}
