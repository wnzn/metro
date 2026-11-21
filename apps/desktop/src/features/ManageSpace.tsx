// Owner-only management of the active Space: members and the runner token.

import { Clipboard, signal } from "nsh";
import {
  activeSpace,
  addMember,
  closeManage,
  hideRunnerToken,
  issueRunnerToken,
  manageError,
  me,
  memberBusy,
  members,
  notice,
  removeMember,
  revealedRunnerToken,
  revokeRunnerToken,
  runnerBusy,
} from "../store";
import { Action, Badge, Banner, Column, Divider, Field, Label, Row, ScrollArea, SecretValue, Surface } from "../ui";
import { ConnectionBanner } from "./Conversation";

export function ManageSpace() {
  return (
    <Surface variant="panel" grow testID="manage-space">
      <Surface variant="toolbar" padX={4} padY={3} gap={3}>
        <Column grow gap={1}>
          <Label variant="heading">{`Manage ${activeSpace()?.name ?? ""}`}</Label>
          <Label variant="caption" color="muted">
            Only the owner sees this. Members use the Space's rooms and runner.
          </Label>
        </Column>
        <Action testID="close-manage" label="Back to room" onPress={closeManage} />
      </Surface>
      <ConnectionBanner />
      <ScrollArea pad={4} gap={4}>
        <If when={manageError()}>
          {(message) => (
            <Banner tone="danger" testID="manage-error">
              <Label color="danger">{message()}</Label>
            </Banner>
          )}
        </If>
        <If when={notice()}>
          {(message) => (
            <Label variant="caption" color="muted" testID="manage-notice">
              {message()}
            </Label>
          )}
        </If>
        <MembersCard />
        <RunnerCard />
      </ScrollArea>
    </Surface>
  );
}

function MembersCard() {
  const newId = signal("");
  const submit = async () => {
    if (await addMember(newId())) newId.set("");
  };
  return (
    <Surface variant="card" pad={4} gap={3} testID="members-card">
      <Column gap={1}>
        <Label variant="heading">Members</Label>
        <Label variant="caption" color="muted">
          Add people by the user ID shown in their sidebar (their identity provider's stable id, not a display name).
        </Label>
      </Column>
      <If
        when={members()}
        else={
          <Label variant="caption" color="muted">
            Loading members…
          </Label>
        }
      >
        {(list) => (
          <Column gap={1} testID="members-list">
            <Each of={list()} key={(m) => m.userId}>
              {(m) => (
                <Column gap={1}>
                  <Row align="center" gap={2} padY={1} testID={`member:${m().userId}`}>
                    <Column grow>
                      <Label variant="mono" lines={1}>
                        {m().userId === me() ? `${m().userId} (you)` : m().userId}
                      </Label>
                    </Column>
                    <Badge tone={m().role === "owner" ? "agent" : "neutral"} label={m().role} />
                    <If when={m().role !== "owner" && m().userId !== me()}>
                      <Action
                        testID={`remove-member:${m().userId}`}
                        label="Remove"
                        busyLabel="Removing…"
                        busy={memberBusy() === m().userId}
                        variant="ghost"
                        onPress={() => void removeMember(m().userId)}
                      />
                    </If>
                  </Row>
                  <Divider />
                </Column>
              )}
            </Each>
          </Column>
        )}
      </If>
      <Row align="center" gap={2}>
        <Field testID="add-member-id" accessibilityLabel="User ID to add" value={newId()} onChange={newId.set} placeholder="User ID" grow onSubmit={() => void submit()} />
        <Action testID="add-member" label="Add member" busyLabel="Adding…" busy={!!memberBusy() && memberBusy() === newId().trim()} variant="primary" onPress={() => void submit()} />
      </Row>
    </Surface>
  );
}

function RunnerCard() {
  const confirmRevoke = signal(false);
  const copied = signal(false);
  const paired = () => !!activeSpace()?.runnerPaired;
  const copy = () => {
    const token = revealedRunnerToken();
    if (!token) return;
    Clipboard.writeText(token)
      .then(() => copied.set(true))
      .catch(() => {});
  };
  return (
    <Surface variant="card" pad={4} gap={3} testID="runner-card">
      <Row align="center" gap={2}>
        <Column grow gap={1}>
          <Label variant="heading">Runner</Label>
          <Label variant="caption" color="muted">
            A runner serves only this Space. Paired means a token is issued; it doesn't mean the runner is online.
          </Label>
        </Column>
        <Badge testID="runner-paired" tone={paired() ? "runner" : "neutral"} label={paired() ? "Paired" : "Not paired"} />
      </Row>

      <If when={revealedRunnerToken()}>
        {(token) => (
          <Column gap={2} testID="runner-token-reveal">
            <Label variant="strong" color="warning">
              Copy this token now. It is shown once and disappears when you leave this view.
            </Label>
            <SecretValue testID="runner-token-value" value={token()} />
            <Label variant="caption" color="muted">
              Save it in a file only you can read (chmod 600) and point the runner's METRO_RUNNER_TOKEN at it. Issuing a new one replaces it.
            </Label>
            <Row gap={2}>
              <Action testID="copy-runner-token" label={copied() ? "Copied" : "Copy token"} variant="primary" onPress={copy} />
              <Action
                testID="hide-runner-token"
                label="Done, hide it"
                onPress={() => {
                  copied.set(false);
                  hideRunnerToken();
                }}
              />
            </Row>
          </Column>
        )}
      </If>

      <Row gap={2} wrap>
        <Action
          testID="issue-runner-token"
          label={paired() ? "Rotate token" : "Issue token"}
          busyLabel="Issuing…"
          busy={runnerBusy() === "issue"}
          variant={paired() ? "secondary" : "primary"}
          onPress={() => {
            copied.set(false);
            confirmRevoke.set(false);
            void issueRunnerToken();
          }}
        />
        <If when={paired()}>
          <If
            when={confirmRevoke()}
            else={<Action testID="revoke-runner-token" label="Revoke…" variant="ghost" onPress={() => confirmRevoke.set(true)} />}
          >
            <Action
              testID="confirm-revoke"
              label="Revoke now: the runner stops serving this Space"
              busyLabel="Revoking…"
              busy={runnerBusy() === "revoke"}
              variant="danger"
              onPress={() => {
                confirmRevoke.set(false);
                void revokeRunnerToken();
              }}
            />
            <Action label="Cancel" variant="ghost" onPress={() => confirmRevoke.set(false)} />
          </If>
        </If>
      </Row>
    </Surface>
  );
}
