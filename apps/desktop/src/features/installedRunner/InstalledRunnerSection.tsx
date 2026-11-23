// Settings › Runners: "This computer's runner", the personal registered
// runner the installer asked for, on this computer's own Metro workspace.
// Shown only when the launcher's bridge says so (see installedRunner.ts).
// Opening reads; Reconnect and Show folder act only when pressed.

import { Clipboard, effect, signal, untracked } from "nsh";
import { connectedTo, me } from "../../store";
import { Action, ActionBar, Badge, Banner, Column, Label, Row, SettingRow, SettingsGroup } from "../../ui";
import type { Tone } from "../../ui";
import {
  folders,
  installedBusy,
  installedError,
  installedNotice,
  installedRunner,
  loadInstalledRunner,
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
  const fresh = "A new runner isn't attached to any Space: attach it in each Space's Manage view.";
  if (r.runner) {
    return `Reconnecting issues a new token for “${r.runner.name}” if it's still one of your personal runners; its old token stops working at once, wherever else it was used, and the Spaces it's attached to stay attached. If it no longer exists, Metro creates a new personal runner instead. ${fresh}`;
  }
  if (r.state === "interrupted") {
    return `Reconnecting creates a new personal runner. ${fresh} The interrupted setup may have left an unused runner${r.unconfirmedName ? ` (it would be named “${r.unconfirmedName}”, though names aren't unique)` : ""} in your list below: delete it there if you don't need it.`;
  }
  return `Reconnecting creates a new personal runner for this computer. ${fresh}`;
}

/**
 * Call once at the top of the Settings › Runners page (not inside a branch
 * that re-renders): reads the status on open and on any server or account
 * switch. Only reads.
 */
export function watchInstalledRunner() {
  effect(() => {
    connectedTo();
    me();
    untracked(() => void loadInstalledRunner());
  });
}

export function InstalledRunnerSection() {
  return (
    <If when={installedRunner()}>
      {(r) => <Section r={r()} />}
    </If>
  );
}

function Section(props: { r: InstalledRunner }) {
  const confirm = signal(false);
  const copied = signal<string | null>(null);
  const r = () => props.r;
  const badge = () => installedRunnerBadge(r());
  return (
    <SettingsGroup
      title="This computer's runner"
      description="A personal runner the Metro installer set up on this computer, for this local workspace. It's separate from a Space's Local tools switch."
      testID="installed-runner"
    >
      <SettingRow
        label={r().runner?.name ?? (r().unconfirmedName ? `Not confirmed (asked for “${r().unconfirmedName}”)` : "Not registered yet")}
        description={`Read-only: ${r().tools.join(", ") || "workspace.list, workspace.read"}. It never changes files, runs commands or MCP servers.`}
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
      <SettingRow
        label="Attach it to a Space"
        description="Metro doesn't attach it for you. In a Space you own, open Manage and choose it under Runner. Each Space gets its own folder on this computer."
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
                          disabled={!!installedBusy()}
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
          <If
            when={confirm()}
            else={
              <Row justify="end">
                <Action testID="installed-runner-reconnect" label="Reconnect…" disabled={!!installedBusy()} onPress={() => confirm.set(true)} />
              </Row>
            }
          >
            <Banner variant="inline" tone="warning" testID="installed-runner-impact" message={reconnectImpact(r())} />
            <ActionBar>
              <Action testID="installed-runner-cancel" label="Cancel" variant="ghost" onPress={() => confirm.set(false)} />
              <Action
                testID="installed-runner-confirm-reconnect"
                label="Reconnect"
                busyLabel="Reconnecting…"
                busy={installedBusy() === "reconnect"}
                variant="danger"
                onPress={() => {
                  confirm.set(false);
                  void reconnectInstalledRunner();
                }}
              />
            </ActionBar>
          </If>
        </Column>
      </If>
    </SettingsGroup>
  );
}
