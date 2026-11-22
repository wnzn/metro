// Settings › Runners: register a runner (personal, or organization-wide for
// org admins), get its token once with Docker setup steps, and rotate or
// delete the runners this user manages. Attaching happens per Space, in the
// Space owner's Manage view, from the same list.

import { signal } from "nsh";
import { orgAdmin } from "../store";
import {
  cancelRunnerAction,
  confirmRunnerAction,
  createRunner,
  deleteRunner,
  revealed,
  rotateRunner,
  runnerBusy,
  runnerConfirm,
  runnerImpact,
  runnerNotice,
  runners,
  runnersError,
} from "../settings";
import type { RegisteredRunner, RunnerVisibility } from "../settings";
import { Action, ActionBar, Badge, Banner, Column, compactWidth, Field, PageHeader, PageLayout, Row, SegmentedControl, SettingRow, SettingsGroup } from "../ui";
import { TokenReveal } from "./TokenReveal";

export function RunnersSettings() {
  const token = () => revealed()?.kind === "runner";
  const hasMessages = () => !token() && !!(runnersError() || runnerNotice());
  return (
    <PageLayout testID="runners-settings" pad={compactWidth() ? 4 : 6} gap={6} footer={hasMessages() ? <Messages /> : undefined}>
      <If
        when={token()}
        else={
          <Column gap={6}>
            <PageHeader
              title="Runners"
              subtitle="A runner runs workspace tools for Spaces. Register one, start it, then attach it to Spaces you own. One runner can serve several Spaces, each in its own folder; each Space uses at most one runner."
            />
            <AddRunner />
            <RunnerList />
          </Column>
        }
      >
        <TokenReveal />
      </If>
    </PageLayout>
  );
}

/** Errors and what the last change did, in the page's fixed footer. */
function Messages() {
  return (
    <Column gap={2}>
      <If when={runnersError()}>{(message) => <Banner variant="inline" tone="danger" testID="runners-error" message={message()} />}</If>
      <If when={runnerNotice()}>{(message) => <Banner variant="inline" tone="success" testID="runners-notice" message={message()} />}</If>
    </Column>
  );
}

function AddRunner() {
  const name = signal("");
  const visibility = signal<RunnerVisibility>("personal");
  const submit = async () => {
    if (await createRunner(name(), visibility())) {
      name.set("");
      visibility.set("personal");
    }
  };
  return (
    <SettingsGroup title="Add a runner" description="You'll get its token once, with the steps to start it." testID="add-runner-group">
      <SettingRow label="Name" description="Something you'll recognise in a Space's runner list, such as the machine it runs on." stacked>
        <Field testID="runner-name" accessibilityLabel="Runner name" value={name()} onChange={name.set} placeholder="Build box" maxLength={80} onSubmit={() => void submit()} />
      </SettingRow>
      <SettingRow
        label="Who can attach it"
        description={
          orgAdmin()
            ? "Personal: only you, to Spaces you own. Organization: any Space owner on this server; only org admins manage it."
            : "Personal: only you, to Spaces you own. Only organization admins can add organization runners."
        }
      >
        <SegmentedControl
          testID="runner-visibility"
          accessibilityLabel="Who can attach the runner"
          value={visibility()}
          onChange={visibility.set}
          options={[
            { value: "personal", label: "Personal" },
            { value: "organization", label: "Organization", disabled: !orgAdmin(), description: orgAdmin() ? undefined : "Only organization admins can add these" },
          ]}
        />
      </SettingRow>
      <Row padX={4} padY={3} justify="end">
        <Action testID="add-runner" label="Add runner" busyLabel="Adding…" busy={runnerBusy() === "create"} variant="primary" onPress={() => void submit()} />
      </Row>
    </SettingsGroup>
  );
}

function RunnerList() {
  return (
    <SettingsGroup
      title="Runners you can see"
      description="Your personal runners and the organization's. Attached to a Space means that Space sends it jobs; it doesn't mean the runner is online."
      testID="runner-list"
    >
      <If
        when={runners()}
        else={
          <SettingRow label="Loading runners…" />
        }
      >
        {(list) => (
          <Column>
            <If when={list().length === 0}>
              <SettingRow label="No runners yet" description="Add one above. Spaces without a runner still chat with their agent; only workspace tools are missing." />
            </If>
            <Each of={list()} key={(r) => r.id}>
              {(r) => <RunnerRow runner={r()} />}
            </Each>
          </Column>
        )}
      </If>
    </SettingsGroup>
  );
}

function scopeText(r: RegisteredRunner): string {
  if (r.visibility === "personal") return "Personal · yours. Only you can attach it, to Spaces you own.";
  return r.canManage ? "Organization · you manage it as an org admin. Any Space owner can attach it." : "Organization · managed by org admins. You can attach it to Spaces you own.";
}

function RunnerRow(props: { runner: RegisteredRunner }) {
  const r = () => props.runner;
  const confirming = () => (runnerConfirm()?.runnerId === r().id ? runnerConfirm()!.action : null);
  return (
    <Column testID={`runner:${r().id}`}>
      <SettingRow label={r().name} description={scopeText(r())}>
        <Row align="center" gap={2} wrap justify="end">
          <Badge tone={r().visibility === "organization" ? "agent" : "neutral"} label={r().visibility === "organization" ? "Organization" : "Personal"} />
          <If when={r().canManage}>
            <Action
              testID={`rotate-runner:${r().id}`}
              label="Rotate token…"
              busyLabel="Rotating…"
              busy={runnerBusy() === `rotate:${r().id}`}
              disabled={!!runnerBusy()}
              onPress={() => void confirmRunnerAction("rotate", r().id)}
            />
            <Action
              testID={`delete-runner:${r().id}`}
              label="Delete…"
              busyLabel="Deleting…"
              busy={runnerBusy() === `delete:${r().id}`}
              disabled={!!runnerBusy()}
              variant="destructive"
              onPress={() => void confirmRunnerAction("delete", r().id)}
            />
          </If>
        </Row>
      </SettingRow>
      <If when={confirming()}>
        {(action) => (
          <Column padX={4} padY={3} gap={2} testID={`runner-confirm:${r().id}`}>
            <Banner variant="inline" tone="warning" message={impactText(action(), r())} />
            <ActionBar>
              <Action testID="cancel-runner-action" label="Cancel" variant="ghost" onPress={cancelRunnerAction} />
              <Action
                testID={`confirm-${action()}-runner`}
                label={action() === "rotate" ? "Rotate: the old token stops now" : "Delete: detach it everywhere"}
                variant="danger"
                onPress={() => void (action() === "rotate" ? rotateRunner(r().id) : deleteRunner(r().id))}
              />
            </ActionBar>
          </Column>
        )}
      </If>
    </Column>
  );
}

function impactText(action: "rotate" | "delete", r: RegisteredRunner): string {
  const impact = runnerImpact();
  const known = impact?.runnerId === r.id ? impact : null;
  let where: string;
  if (!known || known.spaces === null) where = "Checking which of your Spaces use it…";
  else if (known.spaces.length === 0) where = known.partial ? "None of the Spaces you belong to use it, but other Spaces in the organization may." : "None of your Spaces use it.";
  else where = `Spaces you belong to that use it: ${known.spaces.join(", ")}.${known.partial ? " Other Spaces in the organization may use it too." : ""}`;
  return action === "rotate"
    ? `Rotating stops the current token at once, in every Space this runner serves. Put the new token in its runner.env and restart it. ${where}`
    : `Deleting revokes its token and detaches it from every Space it serves; those Spaces have no runner until their owners attach another. ${where}`;
}
