// Owner-only management of the active Space: members, and which runner
// serves it (an attached registered runner, or its own dedicated token).

import { signal } from "nsh";
import { activeSpace, addMember, closeManage, manageError, me, memberBusy, members, notice, openSettings, removeMember } from "../store";
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
import { Action, Badge, Banner, Column, Divider, Field, Label, Row, ScrollArea, Surface } from "../ui";
import { ConnectionBanner } from "./Conversation";
import { TokenReveal } from "./TokenReveal";
import { LocalToolsCard } from "./localtools/LocalToolsCard";
import { clearLocalToolsMessages, loadLocalTools } from "./localtools/localTools";

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
  return (
    <ScrollArea pad={4} gap={4}>
        <If when={manageError()}>
          {(message) => (
            <Banner tone="danger" testID="manage-error">
              <Label color="danger">{message()}</Label>
            </Banner>
          )}
        </If>
        <If when={notice() ?? spaceRunnerNotice()}>
          {(message) => (
            <Label variant="caption" color="muted" testID="manage-notice">
              {message()}
            </Label>
          )}
        </If>
        <MembersCard />
        <LocalToolsCard />
        <RunnerCard />
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
          <Label variant="heading">Runner</Label>
          <Label variant="caption" color="muted">
            This Space uses at most one runner. Attached or paired means jobs from this Space go to it; it doesn't mean the runner is online.
          </Label>
        </Column>
        <Badge testID="runner-paired" tone={badge().tone} label={badge().label} />
      </Row>

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
            <If
              when={confirm() === "detach"}
              else={<Row><Action testID="detach-runner" label="Detach…" variant="ghost" onPress={() => confirm.set("detach")} /></Row>}
            >
              <Row gap={2} wrap>
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
                <Action label="Cancel" variant="ghost" onPress={() => confirm.set(null)} />
              </Row>
            </If>
          </Column>
        )}
      </If>

      {/* The Space's own dedicated token (single-Space, legacy): rotate or revoke it. */}
      <If when={runner()?.kind === "dedicated"}>
        <Column gap={2} testID="dedicated-runner">
          <Label variant="caption" color="muted">
            A dedicated token serves this Space only. To use a registered runner instead, revoke it first.
          </Label>
          <Row gap={2} wrap>
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
            <If when={confirm() === "revoke"} else={<Action testID="revoke-runner-token" label="Revoke…" variant="ghost" onPress={() => confirm.set("revoke")} />}>
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
              <Action label="Cancel" variant="ghost" onPress={() => confirm.set(null)} />
            </If>
          </Row>
        </Column>
      </If>

      {/* Nobody serves the Space: attach one from the same list as Settings › Runners, or pair a dedicated runner. */}
      <If when={runner() === null}>
        <Column gap={3} testID="attach-runner-list">
          <If when={registeredRunnersSupported()}>
            <Column gap={1}>
              <Label variant="strong">Attach a runner</Label>
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
                      No runner you can attach yet. Add one in Settings › Runners: your personal runners, and the organization's, show up here.
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
                              busyLabel="Attaching…"
                              busy={spaceRunnerBusy() === `attach:${r().id}`}
                              disabled={!!spaceRunnerBusy()}
                              variant="primary"
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
              <Row>
                <Action testID="open-runner-settings" label="Manage runners in Settings" variant="ghost" onPress={() => openSettings("runners")} />
              </Row>
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
