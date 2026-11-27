// Shell command policy controls.
//
// - RunnerShellSection: Settings › Runners, under a runner this person
//   manages. Its default, then each attached Space with Inherit or an
//   override, the effective mode and, when Metro lowers it, why.
// - SpaceShellSection: Space settings › Runner. Every member reads the
//   effective policy; the Space owner edits a dedicated runner's policy here.
//   A registered runner's policy is edited in Settings › Runners.

import { effect, signal } from "nsh";
import type { RegisteredRunner } from "../../api/client";
import type { RunnerShellPolicy, RunnerSpaceShell, ShellApprover, ShellMode, SpaceShell } from "../../api/shell";
import { Action, Badge, Banner, Column, Disclosure, Label, Picker, Row, SettingRow, SettingsGroup } from "../../ui";
import {
  APPROVER_LABEL, approverItems, DEDICATED_APPROVER_LABEL, downgradeText, effectiveText, MODE_HELP, MODE_LABEL, modeItems,
  ORG_APPROVER_LABEL,
} from "./copy";
import { loadRunnerShell, runnerShell, runnerShellBusy, runnerShellNotice, saveRunnerShellDefault, saveRunnerShellSpace } from "./runnerShellState";
import {
  isSettingsOwner, runnerBusy, runnerSnapshot, saveSpaceShell, shellBusy, shellNotice,
} from "../space/spaceSettingsState";

const BUILT_IN_ALLOW = "On a built-in runner, Always allow applies only while it serves exactly one Space.";
const isMode = (v: string | null): v is ShellMode => v === "off" || v === "approve" || v === "classify" || v === "allow";
const isApprover = (v: string | null): v is ShellApprover => v === "owner" || v === "members";

// ---------------------------------------------------------------- Settings › Runners

export function RunnerShellSection(props: { runner: RegisteredRunner; builtIn?: boolean }) {
  const open = signal(false);
  const id = () => props.runner.id;
  const entry = () => runnerShell(id());
  const toggle = (next: boolean) => {
    open.set(next);
    if (next) void loadRunnerShell(id());
  };
  return (
    <Disclosure title="Shell commands" testID={`runner-shell:${id()}`} open={open()} onToggle={toggle}
      summary={entry()?.policy ? effectiveText(entry()!.policy!.default.mode, entry()!.policy!.default.approver,
        entry()!.policy!.visibility === "organization" ? ORG_APPROVER_LABEL : APPROVER_LABEL) : undefined}>
      <If when={open()}>
        <Column gap={3} testID={`runner-shell-panel:${id()}`}>
          <If when={entry()?.error}>
            {(message) => (
              <Banner variant="inline" tone="danger" testID={`runner-shell-error:${id()}`} message={message()}
                action={<Action label="Try again" onPress={() => void loadRunnerShell(id())} />} />
            )}
          </If>
          <If when={entry()?.policy} else={<If when={!entry()?.error}><Label variant="caption" color="muted">Loading the shell policy…</Label></If>}>
            {(policy) => <RunnerShellEditor policy={policy()} builtIn={!!props.builtIn || policy().builtIn} />}
          </If>
          <If when={runnerShellNotice(id())}>
            {(n) => <Banner variant="inline" tone={n().ok ? "success" : "danger"} testID={n().ok ? `runner-shell-notice:${id()}` : `runner-shell-save-error:${id()}`} message={n().message} />}
          </If>
        </Column>
      </If>
    </Disclosure>
  );
}

function RunnerShellEditor(props: { policy: RunnerShellPolicy; builtIn: boolean }) {
  const id = () => props.policy.runnerId;
  const organization = () => props.policy.visibility === "organization";
  const labels = () => (organization() ? ORG_APPROVER_LABEL : APPROVER_LABEL);
  const mode = signal<ShellMode>(props.policy.default.mode);
  const approver = signal<ShellApprover>(props.policy.default.approver);
  // A new answer from the server replaces any unsaved draft.
  effect(() => {
    mode.set(props.policy.default.mode);
    approver.set(props.policy.default.approver);
  });
  const changed = () => mode() !== props.policy.default.mode || approver() !== props.policy.default.approver;
  const invalid = () => organization() && mode() === "classify";
  const busy = () => runnerShellBusy() === `default:${id()}`;
  return (
    <Column gap={3}>
      <Column gap={2} testID={`runner-shell-default:${id()}`}>
        <Label variant="strong">Default</Label>
        <Label variant="caption" color="muted">Used by every attached Space without its own setting.</Label>
        <Row gap={2} wrap align="center">
          <Picker testID={`runner-shell-mode:${id()}`} accessibilityLabel="Shell commands" items={modeItems(organization())}
            value={mode()} disabled={!!runnerShellBusy()} onChange={(v) => { if (isMode(v)) mode.set(v); }} />
          <Picker testID={`runner-shell-approver:${id()}`} accessibilityLabel="Who approves" items={approverItems(labels())}
            value={approver()} disabled={!!runnerShellBusy()}
            onChange={(v) => { if (isApprover(v)) approver.set(v); }} />
          <Action testID={`runner-shell-save:${id()}`} label="Save" busyLabel="Saving…" variant="primary" compact
            busy={busy()} disabled={!changed() || invalid() || !!runnerShellBusy()}
            onPress={() => void saveRunnerShellDefault(id(), { mode: mode(), approver: approver() })} />
        </Row>
        <Label variant="caption" color="muted" testID={`runner-shell-help:${id()}`}>{MODE_HELP[mode()]}</Label>
        <If when={props.builtIn && mode() === "allow"}>
          <Label variant="caption" color="muted" testID={`runner-shell-builtin-note:${id()}`}>{BUILT_IN_ALLOW}</Label>
        </If>
      </Column>
      <Column gap={2} testID={`runner-shell-spaces:${id()}`}>
        <Label variant="strong">Attached Spaces</Label>
        <If when={props.policy.spaces.length > 0} else={<Label variant="caption" color="muted">Not attached to any Space.</Label>}>
          <Each of={props.policy.spaces} key={(s) => s.spaceId}>
            {(s) => <SpaceOverride runnerId={id()} space={s()} defaults={props.policy.default} organization={organization()} builtIn={props.builtIn} />}
          </Each>
        </If>
      </Column>
    </Column>
  );
}

function SpaceOverride(props: { runnerId: string; space: RunnerSpaceShell; defaults: { mode: ShellMode; approver: ShellApprover }; organization: boolean; builtIn: boolean }) {
  const key = () => `${props.runnerId}:${props.space.spaceId}`;
  const mode = signal<string>(props.space.mode ?? "inherit");
  const approver = signal<string>(props.space.approver ?? "inherit");
  effect(() => {
    mode.set(props.space.mode ?? "inherit");
    approver.set(props.space.approver ?? "inherit");
  });
  const sendMode = () => (isMode(mode()) ? mode() as ShellMode : null);
  const sendApprover = () => (isApprover(approver()) ? approver() as ShellApprover : null);
  const changed = () => sendMode() !== props.space.mode || sendApprover() !== props.space.approver;
  const invalid = () => props.organization && sendMode() === "classify";
  const draftMode = () => sendMode() ?? props.defaults.mode;
  const busy = () => runnerShellBusy() === `space:${key()}`;
  const name = () => props.space.spaceName ?? "A Space you're not a member of";
  const labels = () => (props.organization ? ORG_APPROVER_LABEL : APPROVER_LABEL);
  return (
    <Column gap={2} testID={`runner-shell-space:${key()}`}>
      <Row gap={2} align="center" wrap>
        <Label variant="body">{name()}</Label>
        <Badge testID={`runner-shell-effective:${key()}`} tone={props.space.effectiveMode === "allow" ? "warning" : "neutral"}
          label={effectiveText(props.space.effectiveMode, props.space.effectiveApprover, labels())} />
      </Row>
      <If when={props.space.downgradedReason}>
        {(reason) => <Label variant="caption" color="muted" testID={`runner-shell-downgrade:${key()}`}>{downgradeText(reason())}</Label>}
      </If>
      <Row gap={2} wrap align="center">
        <Picker testID={`runner-shell-space-mode:${key()}`} accessibilityLabel={`Shell commands in ${name()}`}
          items={modeItems(props.organization, props.defaults.mode)} value={mode()} disabled={!!runnerShellBusy()}
          onChange={(v) => { if (v) mode.set(v); }} />
        <Picker testID={`runner-shell-space-approver:${key()}`} accessibilityLabel={`Who approves in ${name()}`}
          items={approverItems(labels(), props.defaults.approver)} value={approver()}
          disabled={!!runnerShellBusy()}
          onChange={(v) => { if (v) approver.set(v); }} />
        <Action testID={`runner-shell-space-save:${key()}`} label="Save" busyLabel="Saving…" compact
          busy={busy()} disabled={!changed() || invalid() || !!runnerShellBusy()}
          onPress={() => void saveRunnerShellSpace(props.runnerId, props.space.spaceId, { mode: sendMode(), approver: sendApprover() })} />
      </Row>
      <If when={props.builtIn && draftMode() === "allow"}>
        <Label variant="caption" color="muted" testID={`runner-shell-space-builtin-note:${key()}`}>{BUILT_IN_ALLOW}</Label>
      </If>
    </Column>
  );
}

// ---------------------------------------------------------------- Space settings › Runner

const SOURCE_TEXT: Record<SpaceShell["source"], string> = {
  dedicated: "Set for this Space's dedicated runner.",
  runner_default: "The runner's default.",
  space_override: "Set for this Space on the runner.",
};

export function SpaceShellSection() {
  const runner = () => runnerSnapshot()?.runner;
  const shell = () => runnerSnapshot()?.shell ?? null;
  const dedicated = () => runner()?.kind === "dedicated";
  const labels = () => {
    const r = runner();
    return r?.kind === "dedicated" ? DEDICATED_APPROVER_LABEL : r?.visibility === "organization" ? ORG_APPROVER_LABEL : APPROVER_LABEL;
  };
  return (
    <If when={runner()}>
      <SettingsGroup title="Shell commands" testID="space-shell">
        <If when={shell()} else={<SettingRow label="Policy unknown" testID="space-shell-unknown" description="This server didn't report a shell policy." />}>
          {(s) => (
            <Column>
              <SettingRow label="Now" testID="space-shell-effective-row"
                description={[SOURCE_TEXT[s().source], s().effectiveMode !== s().mode ? `Set to ${MODE_LABEL[s().mode]}.` : ""].filter(Boolean).join(" ")}>
                <Badge testID="space-shell-effective" tone={s().effectiveMode === "allow" ? "warning" : "neutral"}
                  label={effectiveText(s().effectiveMode, s().approver, labels())} />
              </SettingRow>
              <If when={s().downgradedReason}>
                {(reason) => (
                  <Column padX={4} padY={2}>
                    <Label variant="caption" color="muted" testID="space-shell-downgrade">{downgradeText(reason())}</Label>
                  </Column>
                )}
              </If>
              <If when={dedicated() && isSettingsOwner()} else={
                <Column padX={4} padY={3}>
                  <Label variant="caption" color="muted" testID="space-shell-readonly">
                    {dedicated() ? "Only the owner can change this." : "Whoever manages the runner changes this in Settings › Runners."}
                  </Label>
                </Column>
              }>
                <DedicatedShellEditor shell={s()} />
              </If>
            </Column>
          )}
        </If>
        <If when={shellNotice()}>
          {(n) => (
            <Column padX={4} padY={2}>
              <Banner variant="inline" tone={n().ok ? "success" : "danger"} testID={n().ok ? "space-shell-notice" : "space-shell-error"} message={n().message} />
            </Column>
          )}
        </If>
      </SettingsGroup>
    </If>
  );
}

function DedicatedShellEditor(props: { shell: SpaceShell }) {
  const mode = signal<ShellMode>(props.shell.mode);
  const approver = signal<ShellApprover>(props.shell.approver);
  effect(() => {
    mode.set(props.shell.mode);
    approver.set(props.shell.approver);
  });
  const changed = () => mode() !== props.shell.mode || approver() !== props.shell.approver;
  const disabled = () => shellBusy() || !!runnerBusy();
  return (
    <SettingRow label="Dedicated runner" testID="space-shell-editor" stacked description={MODE_HELP[mode()]}>
      <Row gap={2} wrap align="center">
        <Picker testID="space-shell-mode" accessibilityLabel="Shell commands" items={modeItems(false)} value={mode()}
          disabled={disabled()} onChange={(v) => { if (isMode(v)) mode.set(v); }} />
        <Picker testID="space-shell-approver" accessibilityLabel="Who approves" items={approverItems(DEDICATED_APPROVER_LABEL)}
          value={approver()} disabled={disabled()}
          onChange={(v) => { if (isApprover(v)) approver.set(v); }} />
        <Action testID="space-shell-save" label="Save" busyLabel="Saving…" variant="primary" compact
          busy={shellBusy()} disabled={!changed() || disabled()}
          onPress={() => void saveSpaceShell({ mode: mode(), approver: approver() })} />
      </Row>
    </SettingRow>
  );
}
