// Owner-only management of the active Space: members, and which runner
// serves it (an attached registered runner, or its own dedicated token).
//
// One primary action (Add member). Every first step that removes or stops
// something is `variant="destructive"` (outlined) at the card's trailing
// edge; only its confirmation, which says what happens, is filled ("danger").

import { signal } from "nsh";
import type { JSX } from "nsh";
import { activeRoom, activeSpace, activeSpaceId, addMember, closeManage, manageError, me, memberBusy, members, notice, openSettings, removeMember } from "../store";
import {
  activeSpaceRunner,
  attachableRunners,
  attachRunner,
  detachRunner,
  issueDedicatedToken,
  registeredRunnersSupported,
  revealed,
  revokeDedicatedToken,
  runners,
  spaceRunnerBusy,
  spaceRunnerNotice,
} from "../settings";
import type { SpaceRunner } from "../settings";
import { Action, Badge, Banner, Column, Divider, Field, Label, Row, ScrollArea, Spacer, Surface } from "../ui";
import { ConnectionBanner } from "./Conversation";
import { TokenReveal } from "./TokenReveal";
import { TEST_RUNNER_DESCRIPTION, TEST_RUNNER_LABEL, TestRunnerAction } from "./space/TestRunner";
import { LocalToolsCard } from "./localtools/LocalToolsCard";
import { clearLocalToolsMessages, loadLocalTools } from "./localtools/localTools";

export function ManageSpace() {
  return (
    <Surface variant="panel" grow testID="manage-space">
      <Surface variant="toolbar" padX={4} padY={3} gap={3}>
        <Column grow gap={1}>
          <Label variant="heading" heading>{`Manage ${activeSpace()?.name ?? ""}`}</Label>
          <Label variant="caption" color="muted">
            Only you, the owner, see this.
          </Label>
        </Column>
        <Action testID="close-manage" label="Back to room" onPress={closeManage} />
      </Surface>
      <ConnectionBanner />
      <If when={revealed()?.kind === "dedicated"} else={<ManageBody />}>
        <ScrollArea pad={6}>
          <Surface variant="content">
            <TokenReveal />
          </Surface>
        </ScrollArea>
      </If>
    </Surface>
  );
}

function ManageBody() {
  // Local tools exist only with the packaged launcher and this computer's own workspace.
  clearLocalToolsMessages();
  void loadLocalTools();
  // A runner change that just went through reads as done; anything else the store says is plain news.
  const message = () => notice() ?? spaceRunnerNotice();
  return (
    <ScrollArea pad={4}>
      <Surface variant="content" gap={4}>
        <If when={manageError()}>
          {(m) => (
            <Banner tone="danger" testID="manage-error">
              <Label color="danger">{m()}</Label>
            </Banner>
          )}
        </If>
        <If when={message()}>
          {(m) => (
            <Banner tone={notice() ? "neutral" : "success"} testID="manage-notice">
              <Label color={notice() ? "default" : "success"}>{m()}</Label>
            </Banner>
          )}
        </If>
        <MembersCard />
        <LocalToolsCard />
        <RunnerCard />
      </Surface>
    </ScrollArea>
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
        <Label variant="heading" heading>
          Members
        </Label>
        <Label variant="caption" color="muted">
          Add people by their user ID (Copy ID in their account menu), not a display name.
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
                        variant="destructive"
                        testID={`remove-member:${m().userId}`}
                        label="Remove"
                        accessibilityLabel={`Remove ${m().userId} from this Space`}
                        busyLabel="Removing…"
                        busy={memberBusy() === m().userId}
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

/** A destructive first step at the trailing edge, and in its place the filled confirmation with Cancel. */
function ConfirmAtEdge(props: {
  asking: boolean;
  lead?: JSX.Element;
  trigger: JSX.Element;
  confirm: JSX.Element;
  onCancel: () => void;
}) {
  return (
    <Row align="center" gap={2} wrap>
      {props.lead}
      <Spacer />
      <If when={props.asking} else={props.trigger}>
        <Action label="Cancel" variant="ghost" onPress={props.onCancel} />
        {props.confirm}
      </If>
    </Row>
  );
}

function RunnerCard() {
  const confirm = signal<null | "detach" | "revoke">(null);
  const runner = () => activeSpaceRunner();
  const badge = () => {
    const r = runner();
    if (r === undefined) return { tone: "neutral" as const, label: "Checking…" };
    if (r === null) return { tone: "neutral" as const, label: "No runner" };
    return { tone: "runner" as const, label: r.kind === "dedicated" ? "Dedicated token" : "Attached" };
  };
  return (
    <Surface variant="card" pad={4} gap={3} testID="runner-card">
      <Row align="center" gap={2}>
        <Column grow gap={1}>
          <Label variant="heading" heading>
            Runner
          </Label>
          <Label variant="caption" color="muted">
            One runner at a time. Attached or paired means this Space's jobs go to it; it doesn't mean the runner is online.
          </Label>
        </Column>
        <Badge testID="runner-paired" tone={badge().tone} label={badge().label} />
      </Row>

      {/* Test runner: only while a runner is attached or paired. */}
      <If when={activeSpace()?.runnerPaired}>
        <Row align="center" gap={3} testID="test-runner-row">
          <Column grow gap={1}>
            <Label variant="strong">{TEST_RUNNER_LABEL}</Label>
            <Label variant="caption" color="muted">{`${TEST_RUNNER_DESCRIPTION}. The listing is posted in # ${activeRoom()?.name ?? "the room you have open"}.`}</Label>
          </Column>
          <TestRunnerAction spaceId={activeSpaceId()} />
        </Row>
      </If>

      <If when={runner() === undefined}>
        <Label variant="caption" color="muted">
          Loading this Space's runner…
        </Label>
      </If>

      {/* A registered runner: detach it (it keeps serving its other Spaces). */}
      <If when={runner()?.kind === "registered" ? (runner() as Extract<SpaceRunner, { kind: "registered" }>) : null}>
        {(r) => (
          <Column gap={2} testID="attached-runner">
            <Row align="center" gap={2}>
              <Label variant="strong">{r().name}</Label>
              <Badge tone={r().visibility === "organization" ? "agent" : "neutral"} label={r().visibility === "organization" ? "Organization" : "Personal"} />
            </Row>
            <Label variant="caption" color="muted">
              It works in its own folder for this Space (spaces/{activeSpace()?.id ?? ""}/ on the runner). Its token is managed in Settings › Runners.
            </Label>
            <ConfirmAtEdge
              asking={confirm() === "detach"}
              onCancel={() => confirm.set(null)}
              trigger={<Action variant="destructive" testID="detach-runner" label="Detach…" onPress={() => confirm.set("detach")} />}
              confirm={
                <Action
                  testID="confirm-detach"
                  label="Detach now: this Space stops sending it jobs"
                  busyLabel="Detaching…"
                  busy={spaceRunnerBusy() === "detach"}
                  variant="danger"
                  onPress={() => {
                    confirm.set(null);
                    void detachRunner();
                  }}
                />
              }
            />
          </Column>
        )}
      </If>

      {/* The Space's own dedicated token (single-Space, legacy): rotate or revoke it. */}
      <If when={runner()?.kind === "dedicated"}>
        <Column gap={2} testID="dedicated-runner">
          <Label variant="caption" color="muted">
            A dedicated token serves this Space only. To use a registered runner instead, revoke it first.
          </Label>
          <ConfirmAtEdge
            asking={confirm() === "revoke"}
            onCancel={() => confirm.set(null)}
            lead={
              <Action
                testID="issue-runner-token"
                label="Rotate token"
                busyLabel="Issuing…"
                busy={spaceRunnerBusy() === "issue"}
                disabled={!!spaceRunnerBusy()}
                onPress={() => {
                  confirm.set(null);
                  void issueDedicatedToken();
                }}
              />
            }
            trigger={<Action variant="destructive" testID="revoke-runner-token" label="Revoke…" disabled={!!spaceRunnerBusy()} onPress={() => confirm.set("revoke")} />}
            confirm={
              <Action
                testID="confirm-revoke"
                label="Revoke now: the runner stops serving this Space"
                busyLabel="Revoking…"
                busy={spaceRunnerBusy() === "revoke"}
                variant="danger"
                onPress={() => {
                  confirm.set(null);
                  void revokeDedicatedToken();
                }}
              />
            }
          />
        </Column>
      </If>

      {/* Nobody serves the Space: attach one from the same list as Settings › Runners, or pair a dedicated runner. */}
      <If when={runner() === null}>
        <Column gap={3} testID="attach-runner-list">
          <If when={registeredRunnersSupported()}>
            <Column gap={1}>
              <Row align="center" gap={2}>
                <Label variant="strong">Attach a runner</Label>
                <Spacer />
                <Action testID="open-runner-settings" label="Manage runners in Settings" variant="ghost" onPress={() => openSettings("runners")} />
              </Row>
              <If
                when={runners()}
                else={
                  <Label variant="caption" color="muted">
                    Loading runners…
                  </Label>
                }
              >
                <If
                  when={attachableRunners().length > 0}
                  else={
                    <Label variant="caption" color="muted">
                      No runner you can attach yet. Your personal runners and the organization's show up here once added in Settings › Runners.
                    </Label>
                  }
                >
                  <Column gap={1}>
                    <Each of={attachableRunners()} key={(r) => r.id}>
                      {(r) => (
                        <Column gap={1}>
                          <Row align="center" gap={2} padY={1} testID={`attachable:${r().id}`}>
                            <Column grow>
                              <Label lines={1}>{r().name}</Label>
                            </Column>
                            <Badge tone={r().visibility === "organization" ? "agent" : "neutral"} label={r().visibility === "organization" ? "Organization" : "Personal"} />
                            <Action
                              testID={`attach-runner:${r().id}`}
                              label="Attach"
                              accessibilityLabel={`Attach ${r().name}`}
                              busyLabel="Attaching…"
                              busy={spaceRunnerBusy() === `attach:${r().id}`}
                              disabled={!!spaceRunnerBusy()}
                              variant="secondary"
                              onPress={() => void attachRunner(r().id)}
                            />
                          </Row>
                          <Divider />
                        </Column>
                      )}
                    </Each>
                  </Column>
                </If>
              </If>
            </Column>
          </If>
          <Column gap={1}>
            <Label variant="strong">Or pair a dedicated runner</Label>
            <Label variant="caption" color="muted">
              A token for this Space only, shown once. Use it when a runner should never serve another Space.
            </Label>
            <Row>
              <Action
                testID="issue-runner-token"
                label="Issue dedicated token"
                busyLabel="Issuing…"
                busy={spaceRunnerBusy() === "issue"}
                disabled={!!spaceRunnerBusy()}
                onPress={() => void issueDedicatedToken()}
              />
            </Row>
          </Column>
        </Column>
      </If>
    </Surface>
  );
}
