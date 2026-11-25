// Settings › Runners: "This computer's runner", the personal registered
// runner the installer asked for, on this computer's own Metro workspace.
// Shown only when the launcher's bridge says so (see installedRunner.ts).
// Opening reads; Reconnect and Show folder act only when pressed.

import { Clipboard, signal } from "nsh";
import type { JSX } from "nsh";
import { runnerBusy, runners } from "../../settings";
import { attachMany, eligibleSpaces, installedFolderRefreshExhausted, installedRunnerChoice, retryInstalledFolderRefresh, runnerAttachBusy, runnerSessionKey } from "../../runnersState";
import { spaces } from "../../store";
import { Action, Badge, Banner, Checkbox, Column, ConfirmInline, Label, Row, SettingRow, SettingsGroup } from "../../ui";
import type { Tone } from "../../ui";
import {
  folders,
  installedBusy,
  installedError,
  installedNotice,
  installedRunner,
  checkingInstalledScope,
  ownedInstalledRunner,
  reconnectInstalledRunner,
  runnerSpaces,
  showRunnerFolder,
} from "./installedRunner";
import type { InstalledRunner } from "./installedRunner";

export function installedRunnerBadge(r: InstalledRunner): { tone: Tone; label: string } {
  switch (r.state) {
    case "online":
      return { tone: "success", label: "Online" };
    case "starting":
      return { tone: "neutral", label: "Starting…" };
    case "offline":
      return { tone: "warning", label: "Can't reach the local server" };
    case "revoked":
      return { tone: "danger", label: "Token no longer accepted" };
    case "deleted":
      return { tone: "danger", label: "Deleted" };
    case "interrupted":
      return { tone: "warning", label: "Setup interrupted" };
    case "failed":
      return { tone: "danger", label: "Failed" };
    case "idle":
      return { tone: "neutral", label: "Not started" };
    default:
      return { tone: "neutral", label: "Stopped" };
  }
}

export function reconnectImpact(r: InstalledRunner): string {
  const fresh = "A new runner isn't attached to any Space. You can choose where to attach it afterward.";
  if (r.runner) {
    return `Reconnecting issues a new token for “${r.runner.name}” if it's still one of your personal runners; its old token stops working at once, wherever else it was used, and the Spaces it's attached to stay attached. If it no longer exists, Metro creates a new personal runner instead. ${fresh}`;
  }
  if (r.state === "interrupted") {
    return `Reconnecting creates a new personal runner. ${fresh} The interrupted setup may have left an unused runner${r.unconfirmedName ? ` (it would be named “${r.unconfirmedName}”, though names aren't unique)` : ""} in your list below: delete it there if you don't need it.`;
  }
  return `Reconnecting creates a new personal runner for this computer. ${fresh}`;
}

export function InstalledRunnerSection(props: { management?: JSX.Element } = {}) {
  return (
    <>
      <If when={installedRunner()}>
        {(r) => <Section r={r()} management={props.management} />}
      </If>
      <If when={installedFolderRefreshExhausted()}>
        <Banner variant="inline" tone="warning" testID="installed-runner-folders-unavailable"
          title="Couldn’t refresh this computer’s runner folders"
          message="Automatic checks stopped. If the runner isn’t registered, use Reconnect if available, then retry."
          action={<Action testID="installed-runner-folders-retry" label="Retry" onPress={retryInstalledFolderRefresh} />} />
      </If>
    </>
  );
}

function Section(props: { r: InstalledRunner; management?: JSX.Element }) {
  const copied = signal<string | null>(null);
  const r = () => props.r;
  const badge = () => installedRunnerBadge(r());
  const version = () => runners()?.find((item) => item.id === ownedInstalledRunner()?.id)?.presence?.version;
  return (
    <SettingsGroup
      title="This computer's runner"
      description="A personal runner the Metro installer set up on this computer, for this local workspace. It's separate from a Space's Local tools switch."
      testID="installed-runner"
    >
      <SettingRow
        label={r().runner?.name ?? (r().unconfirmedName ? `Not confirmed (asked for “${r().unconfirmedName}”)` : "Not registered yet")}
        description={`${version() ? `v${version()}` : "Version unknown until it connects"}. Updates with Metro. Read-only: ${r().tools.join(", ") || "workspace.list, workspace.read"}. It never changes files, runs commands or MCP servers.`}
      >
        <Badge testID="installed-runner-state" tone={badge().tone} label={badge().label} />
      </SettingRow>
      <If when={r().detail}>
        {(d) => (
          <Column padX={4} padY={2}>
            <Label variant="caption" color="muted" testID="installed-runner-detail">
              {d()}
            </Label>
          </Column>
        )}
      </If>
      <AttachPicks />
      <SettingRow
        label="Folders on this computer"
        description={runnerSpaces().length ? "Each Space gets its own folder on this computer."
          : installedFolderRefreshExhausted() ? "Couldn’t verify attached Spaces." : "Attached Spaces will appear here."}
        stacked
      >
        <If when={runnerSpaces().length > 0}>
          <Column gap={2} testID="installed-runner-spaces">
            <Each of={runnerSpaces()} key={(s) => s.id}>
              {(s) => (
                <Column gap={1} testID={`installed-runner-space:${s().id}`}>
                  <Row align="center" gap={2}>
                    <Column grow>
                      <Label variant="strong">{s().name}</Label>
                    </Column>
                    <If
                      when={folders()[s().id]}
                      else={
                        <Action
                          testID={`installed-runner-folder:${s().id}`}
                          label="Show folder"
                          busyLabel="Opening…"
                          busy={installedBusy() === `folder:${s().id}`}
                          disabled={!!installedBusy() || runnerAttachBusy() || !!runnerBusy()}
                          variant="ghost"
                          onPress={() => void showRunnerFolder(s().id)}
                        />
                      }
                    >
                      {(f) => (
                        <Action
                          testID={`installed-runner-copy:${s().id}`}
                          label={copied() === s().id ? "Copied" : "Copy path"}
                          variant="ghost"
                          onPress={() => {
                            Clipboard.writeText(f().folder);
                            copied.set(s().id);
                          }}
                        />
                      )}
                    </If>
                  </Row>
                  <If when={folders()[s().id]}>
                    {(f) => (
                      <Label variant="mono" lines={2} testID={`installed-runner-path:${s().id}`}>
                        {f().folder}
                      </Label>
                    )}
                  </If>
                </Column>
              )}
            </Each>
          </Column>
        </If>
      </SettingRow>
      <If when={installedError()}>
        {(m) => (
          <Column padX={4} padY={2}>
            <Banner variant="inline" tone="danger" testID="installed-runner-error" message={m()} />
          </Column>
        )}
      </If>
      <If when={installedNotice()}>
        {(m) => (
          <Column padX={4} padY={2}>
            <Banner variant="inline" tone="success" testID="installed-runner-notice" message={m()} />
          </Column>
        )}
      </If>
      <If when={r().canReconnect}>
        <Column padX={4} padY={3} gap={2}>
          <ConfirmInline testID="installed-runner-reconnect" triggerLabel="Reconnect…" confirmLabel="Reconnect"
            message={reconnectImpact(r())} scopeKey={JSON.stringify([runnerSessionKey(), r().runner?.id, r().state])}
            disabled={!!installedBusy() || runnerAttachBusy() || !!runnerBusy()} busy={installedBusy() === "reconnect"} busyLabel="Reconnecting…"
            onConfirm={() => void reconnectInstalledRunner()} />
        </Column>
      </If>
      {props.management}
    </SettingsGroup>
  );
}

function AttachPicks() {
  const unticked = signal<Record<string, boolean>>({});
  const selected = () => eligibleSpaces().filter((s) => !unticked()[s.id]).map((s) => s.id);
  const busy = () => runnerAttachBusy() || !!installedBusy() || !!runnerBusy();
  const state = () => {
    if (!installedFolderRefreshExhausted() && checkingInstalledScope() && !installedRunnerChoice()) return "Checking this computer's runner…";
    if (ownedInstalledRunner()?.attachIfEmpty === false) return "This server doesn't support attaching only to Spaces without a runner.";
    if (!installedRunnerChoice()) return "This computer's runner isn't available to attach for this account.";
    if (eligibleSpaces().length === 0) return spaces().some((s) => s.role === "owner" && s.runnerPaired === null)
      ? "Some runner assignments haven’t been confirmed yet." : "Every Space you own has a runner.";
    return "Agents in the Spaces you choose can list and read files in their own folders on this computer.";
  };
  return (
    <SettingRow label="Spaces without a runner" description={state()} stacked testID="runner-attach-list">
      <If when={eligibleSpaces().length > 0}>
        <Column gap={2}>
          <Each of={eligibleSpaces()} key={(s) => s.id}>
            {(s) => <Checkbox testID={`runner-attach-pick:${s().id}`} label={s().name} checked={!unticked()[s().id]}
              disabled={busy()} onChange={(checked) => unticked.set({ ...unticked(), [s().id]: !checked })} />}
          </Each>
          <Action testID="runner-attach-many" label={`Attach to ${selected().length} ${selected().length === 1 ? "Space" : "Spaces"}`}
            variant="primary" busy={runnerAttachBusy()} busyLabel="Attaching…" disabled={busy() || !selected().length}
            onPress={() => void attachMany(selected())} />
        </Column>
      </If>
    </SettingRow>
  );
}
