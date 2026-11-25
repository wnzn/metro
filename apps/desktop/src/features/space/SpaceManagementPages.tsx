import { signal } from "nsh";
import type { RegisteredRunner, SpaceRunner } from "../../api/client";
import { activeRoom, activeSpaceId, mainPlace, me, openSettings } from "../../store";
import { Action, Badge, Banner, Column, compactWidth, ConfirmInline, ConnectionItem, Disclosure, Field, Label, PageHeader, PageLayout, SettingRow, SettingsGroup } from "../../ui";
import { LocalToolsCard } from "../localtools/LocalToolsCard";
import { localToolsBusy, localToolsError, localToolsNotice } from "../localtools/localTools";
import { PersonAvatar } from "../profile/PersonAvatar";
import { profileName } from "../profile/profileState";
import { TokenReveal } from "../TokenReveal";
import { TEST_RUNNER_DESCRIPTION, TEST_RUNNER_LABEL, TestRunnerAction } from "./TestRunner";
import {
  changeMember, changeRunner, closeSpaceSettings, dedicatedRevealIsCurrent, isSettingsOwner, loadMembers, loadRunner, loadRunnerInventory,
  memberBusy, memberNotice, preferredSpaceRunner, roster, rosterError, runnerAdvanced, runnerBusy, runnerError,
  runnerInventory, runnerInventoryError, runnerNotice, runnerSnapshot, setRunnerAdvanced, spaceSettingsFor,
} from "./spaceSettingsState";

const scope = { kind: "space" } as const;
const ownerOnly = "Only the owner can change this.";
const pad = () => compactWidth() ? 4 : 6;

export function MembersPage() {
  const newId = signal("");
  const submit = async () => {
    if (await changeMember("add", newId())) newId.set("");
  };
  return (
    <PageLayout testID="space-members" pad={pad()} gap={6} footer={
      rosterError() || memberNotice() ? <Column gap={2}>
        <If when={rosterError()}>
          {(message) => <Banner variant="inline" tone="danger" message={message()} action={<Action label="Try again" onPress={() => void loadMembers()} />} />}
        </If>
        <If when={memberNotice()}>
          {(n) => <Banner variant="inline" tone={n().ok ? "success" : "danger"} message={n().message} testID={n().ok ? "manage-notice" : "manage-error"} />}
        </If>
      </Column> : undefined
    }>
      <PageHeader title="Members" scope={scope} subtitle={isSettingsOwner() ? "See who's in this Space and manage access." : ownerOnly} />
      <SettingsGroup title="People in this Space" testID="members-card">
        <If when={roster()} else={<If when={!rosterError()}><SettingRow label="Loading members…" /></If>}>
          {(list) => (
            <Column testID="members-list">
              <Each of={list()} key={(m) => m.userId}>
                {(m) => {
                  const uid = () => m().userId;
                  const display = () => profileName(uid()) ?? uid();
                  const you = () => uid() === me();
                  return (
                    <ConnectionItem
                      testID={`member:${uid()}`}
                      name={display()}
                      leading={<PersonAvatar userId={uid()} name={display()} size="sm" />}
                      tags={[{ label: m().role === "owner" ? "Owner" : "Member", tone: m().role === "owner" ? "agent" : "neutral" }]}
                      status={{ tone: "neutral", text: `@${uid()}${you() ? " · You" : ""}` }}
                      trailing={
                        <If when={isSettingsOwner() && m().role !== "owner" && !you()}>
                          <ConfirmInline testID={`remove-member:${uid()}`} triggerLabel="Remove…" confirmLabel={`Remove ${display()}`}
                            message={`${display()} loses access to this Space and runs they started here stop.`}
                            scopeKey={`${spaceSettingsFor()}:${uid()}`} disabled={!!memberBusy()}
                            onConfirm={() => void changeMember("remove", uid())} />
                        </If>
                      }
                    />
                  );
                }}
              </Each>
            </Column>
          )}
        </If>
        <If when={isSettingsOwner()}>
          <SettingRow label="Add member by user ID" description="Ask the person to use Copy ID in their account menu." stacked>
            <Field testID="add-member-id" accessibilityLabel="User ID to add" value={newId()} onChange={newId.set}
              placeholder="User ID" disabled={!!memberBusy()} onSubmit={() => void submit()} />
            <Action testID="add-member" label="Add member" busyLabel="Adding…" busy={memberBusy() === newId().trim()}
              disabled={!!memberBusy()} variant="primary" onPress={() => void submit()} />
          </SettingRow>
        </If>
      </SettingsGroup>
    </PageLayout>
  );
}

export function RunnerPage() {
  const runner = () => runnerSnapshot()?.runner;
  const badge = () => {
    const r = runner();
    if (runnerError()) return { tone: "warning", label: "Status unknown" } as const;
    if (r === undefined) return { tone: "neutral", label: "Checking…" } as const;
    if (r === null) return { tone: "neutral", label: "No runner" } as const;
    return { tone: "runner", label: r.kind === "dedicated" ? "Dedicated token" : `Attached · ${r.name}` } as const;
  };
  const sameRoom = () => mainPlace() === "space" && activeSpaceId() === spaceSettingsFor() ? activeRoom() : null;
  const showLocalFeedback = () => isSettingsOwner() && runnerAdvanced();
  const reveal = dedicatedRevealIsCurrent;
  return (
    <PageLayout testID="space-runner" pad={pad()} gap={6} footer={
      !reveal() && (runnerError() || runnerInventoryError() || runnerNotice() || (showLocalFeedback() && (localToolsError() || localToolsNotice()))) ? <Column gap={2}>
        <If when={runnerError()}>
          {(message) => <Banner variant="inline" tone="danger" message={message()} action={<Action label="Try again" onPress={() => void loadRunner()} />} />}
        </If>
        <If when={isSettingsOwner() ? runnerInventoryError() : null}>
          {(message) => <Banner variant="inline" tone="danger" message={message()} action={<Action label="Try again" onPress={() => void loadRunnerInventory()} />} />}
        </If>
        <If when={runnerNotice()}>
          {(n) => <Banner variant="inline" tone={n().ok ? "success" : "danger"} message={n().message} testID={n().ok ? "manage-notice" : "manage-error"} />}
        </If>
        <If when={showLocalFeedback() ? localToolsError() : null}>
          {(message) => <Banner variant="inline" tone="danger" message={message()} testID="local-tools-error" />}
        </If>
        <If when={showLocalFeedback() ? localToolsNotice() : null}>
          {(message) => <Banner variant="inline" tone="success" message={message()} testID="local-tools-notice" />}
        </If>
      </Column> : undefined
    }>
      <If when={reveal()} else={
        <>
          <PageHeader title="Runner" scope={scope} subtitle="See which runner serves this Space and test it without using a model." />
          <SettingsGroup title="Status" testID="runner-card">
            <SettingRow label="Runner" testID="runner-status-row" description={
              runnerError() ? "Metro couldn't confirm the assignment." : runner() === undefined ? "Checking this Space's runner." :
                runner() ? "This Space sends jobs to this runner. The assignment doesn't mean it's online." :
                  "Agents can still answer, but without runner workspace tools."
            }>
              <Badge testID="space-runner-status" tone={badge().tone} label={badge().label} />
            </SettingRow>
            <If when={runner()}>
              <SettingRow label={TEST_RUNNER_LABEL} testID="test-runner-row" description={`${TEST_RUNNER_DESCRIPTION}.${sameRoom() ? ` The listing is posted in # ${sameRoom()!.name}.` : ""}`}>
                <TestRunnerAction spaceId={spaceSettingsFor()} paired={!!runner()} />
              </SettingRow>
            </If>
          </SettingsGroup>
          <ChooseRunner />
          <If when={isSettingsOwner()}>
            <Disclosure title="Advanced: dedicated token and local tools" testID="space-runner-advanced" open={runnerAdvanced()} onToggle={setRunnerAdvanced}>
              <If when={runnerAdvanced()}>
                <DedicatedAndLocalTools />
              </If>
            </Disclosure>
          </If>
        </>
      }>
        <TokenReveal />
      </If>
    </PageLayout>
  );
}

const busy = () => !!runnerBusy() || !!localToolsBusy();
const visibilityTag = (r: { visibility: RegisteredRunner["visibility"] }) =>
  r.visibility === "organization" ? { label: "Organization", tone: "agent" as const } : { label: "Personal", tone: "neutral" as const };

function ChooseRunner() {
  const runner = () => runnerSnapshot()?.runner;
  const registered = () => {
    const r = runner();
    return r?.kind === "registered" ? r : null;
  };
  const list = () => {
    const recommended = preferredSpaceRunner();
    return [...(runnerInventory()?.runners ?? [])].sort((a, b) => Number(b.id === recommended) - Number(a.id === recommended) ||
      Number(a.visibility === "organization") - Number(b.visibility === "organization"));
  };
  return (
    <SettingsGroup title="Choose a runner" description={isSettingsOwner() ? "One runner per Space. A registered runner works in its own folder for this Space." : ownerOnly}>
      <If when={registered()}>
        {(r) => (
          <ConnectionItem testID="attached-runner" name={r().name} tags={[visibilityTag(r())]}
            status={{ tone: "neutral", text: `spaces/${spaceSettingsFor()}/ on the runner · Token managed in Settings › Runners` }}
            trailing={
              <If when={isSettingsOwner()}>
                <DetachRunner runner={r()} />
              </If>
            } />
        )}
      </If>
      <If when={runner()?.kind === "dedicated"}>
        <SettingRow label="Dedicated runner" description={isSettingsOwner() ? "To choose a registered runner, revoke the dedicated token in Advanced first." : "The dedicated token serves this Space only."} />
      </If>
      <If when={runner() === undefined}>
        <SettingRow label={runnerError() ? "Assignment not confirmed" : "Loading this Space's runner…"} />
      </If>
      <If when={runner() === null}>
        <Column testID="attach-runner-list">
          <If when={isSettingsOwner()} else={<SettingRow label="No runner" />}>
            <SettingRow label="Registered runners" description="Choose one of your personal runners or an organization runner.">
              <Action testID="open-runner-settings" label="Open Runners" variant="ghost" onPress={() => {
                closeSpaceSettings();
                openSettings("runners");
              }} />
            </SettingRow>
            <If when={runnerSnapshot()?.legacy || runnerInventory()?.attachIfEmpty === false}
              else={
                <If when={runnerInventory()} else={<If when={!runnerInventoryError()}><SettingRow label="Loading runners…" /></If>}>
                  <If when={list().length > 0} else={<SettingRow label="No runners you can attach yet" description="Add a runner in Settings › Runners." />}>
                    <Each of={list()} key={(r) => r.id}>
                      {(r) => <AttachRunner runner={r()} />}
                    </Each>
                  </If>
                </If>
              }>
              <SettingRow testID="space-runner-attach-unsupported" label="Attachment unavailable"
                description="This server doesn't support attaching only to Spaces without a runner." />
            </If>
          </If>
        </Column>
      </If>
    </SettingsGroup>
  );
}

function AttachRunner(props: { runner: RegisteredRunner }) {
  const r = () => props.runner;
  const installed = () => preferredSpaceRunner() === r().id;
  const name = () => installed() ? "This computer's runner" : r().name;
  return (
    <ConnectionItem testID={`attachable:${r().id}`} name={name()} tags={installed() ? [{ label: "Recommended", tone: "info" }, visibilityTag(r())] : [visibilityTag(r())]}
      status={{ tone: "neutral", text: r().presence ? (r().presence?.online ? "Online" : "Offline") : "Online status unknown" }}
      trailing={
        <Action testID={`space-runner-attach:${r().id}`} label="Attach" accessibilityLabel={`Attach ${name()}`}
          busyLabel="Attaching…" busy={runnerBusy() === `attach:${r().id}`} disabled={busy()} variant="secondary"
          onPress={() => void changeRunner({ kind: "attach", id: r().id, installed: installed() })} />
      } />
  );
}

function DetachRunner(props: { runner: Extract<SpaceRunner, { kind: "registered" }> }) {
  return (
    <If when={props.runner.assignmentRevision} else={<GuardedRemovalUnavailable />}>
      {(revision) => (
        <ConfirmInline testID="space-runner-detach" triggerLabel="Detach…" confirmLabel="Detach runner"
          message={`This Space stops sending jobs to ${props.runner.name}. Its other Spaces and files stay as they are.`}
          scopeKey={`${spaceSettingsFor()}:${props.runner.id}:${revision()}`} disabled={busy()}
          onConfirm={() => void changeRunner({ kind: "detach", id: props.runner.id, revision: revision() })} />
      )}
    </If>
  );
}

function GuardedRemovalUnavailable(props: { tokenChange?: boolean }) {
  return (
    <Column gap={1} testID={props.tokenChange ? "space-runner-token-change-unavailable" : "space-runner-removal-unavailable"}>
      <Label variant="caption" color="muted">{props.tokenChange ? "Guarded token changes are unavailable." : "Safe removal is unavailable for this assignment."}</Label>
      <Action label="Refresh status" variant="ghost" disabled={busy()} onPress={() => void loadRunner()} />
    </Column>
  );
}

function DedicatedAndLocalTools() {
  const runner = () => runnerSnapshot()?.runner;
  const tokenChanges = () => runnerSnapshot()?.guardedTokenChanges;
  return (
    <Column gap={3}>
      <SettingsGroup title="Dedicated token" description="Pair a runner that serves only this Space. A new token is shown once.">
        <If when={runner() === null}>
          <SettingRow label="No dedicated token">
            <If when={tokenChanges()} else={<GuardedRemovalUnavailable tokenChange />}>
              <Action testID="issue-runner-token" label="Issue dedicated token" busyLabel="Issuing…" busy={runnerBusy() === "issue"}
                disabled={busy()} onPress={() => void changeRunner({ kind: "issue" })} />
            </If>
          </SettingRow>
        </If>
        <If when={runner()?.kind === "dedicated"}>
          <Column testID="dedicated-runner">
            <SettingRow label="Rotate the token" description="The old token stops working. The runner needs the new one." stacked>
              <If when={tokenChanges() ? runner()?.assignmentRevision : undefined} else={<GuardedRemovalUnavailable tokenChange />}>
                {(revision) => (
                  <ConfirmInline testID="issue-runner-token" triggerLabel="Rotate…" confirmLabel="Rotate the token"
                    message="The current dedicated token stops working. The new one can be copied once."
                    scopeKey={`${spaceSettingsFor()}:dedicated:${revision()}`} disabled={busy()}
                    onConfirm={() => void changeRunner({ kind: "rotate", revision: revision() })} />
                )}
              </If>
            </SettingRow>
            <SettingRow label="Revoke the token" stacked>
              <If when={runner()?.assignmentRevision} else={<GuardedRemovalUnavailable />}>
                {(revision) => (
                  <ConfirmInline testID="revoke-runner-token" triggerLabel="Revoke…" confirmLabel="Revoke token"
                    message="The dedicated runner stops serving this Space." scopeKey={`${spaceSettingsFor()}:dedicated:${revision()}`}
                    disabled={busy()} onConfirm={() => void changeRunner({ kind: "revoke", revision: revision() })} />
                )}
              </If>
            </SettingRow>
          </Column>
        </If>
        <If when={runner()?.kind === "registered"}>
          <SettingRow label="Not in use" description="This Space already has a registered runner. Detach it before issuing a dedicated token." />
        </If>
        <If when={runner() === undefined}>
          <SettingRow label="Waiting for runner status" />
        </If>
      </SettingsGroup>
      <LocalToolsCard spaceId={spaceSettingsFor()} runner={runner()} disabled={!!runnerBusy()}
        refreshRunner={async (sid, latest) => { if (spaceSettingsFor() === sid) await loadRunner(latest); }} />
    </Column>
  );
}
