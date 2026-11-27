// Settings › Runners: "This computer's runner", the personal registered
// runner built into Metro, for the server this session is signed in to.
// Shown only when the launcher's bridge says so (see installedRunner.ts).
// Opening reads; Set up, Reconnect and Show folder act only when pressed.

import { Clipboard, effect, signal, untracked } from "nsh";
import type { JSX } from "nsh";
import { runnerBusy, runners } from "../../settings";
import { attachMany, eligibleSpaces, installedFolderRefreshExhausted, installedRunnerChoice, retryInstalledFolderRefresh, runnerAttachBusy, runnerSessionKey } from "../../runnersState";
import { sessionGuard, spaces } from "../../store";
import { Action, Badge, Banner, Checkbox, Column, ConfirmInline, Field, Label, Row, SettingRow, SettingsGroup } from "../../ui";
import type { Tone } from "../../ui";
import {
  chosenFolders,
  clearRunnerFolder,
  folderErrors,
  folders,
  installedFolderSnapshot,
  verifiedDetached,
  loadChosenFolders,
  setRunnerFolder,
  installedBusy,
  installedError,
  installedNotice,
  installedRunner,
  checkingInstalledScope,
  ownedInstalledRunner,
  reconnectInstalledRunner,
  runnerSpaces,
  setupInstalledRunner,
  showRunnerFolder,
} from "./installedRunner";
import type { InstalledRunner } from "./installedRunner";

export function installedRunnerBadge(r: InstalledRunner): { tone: Tone; label: string } {
  switch (r.state) {
    case "online":
      return { tone: "success", label: "Connected" };
    case "starting":
      return { tone: "neutral", label: "Starting…" };
    case "offline":
      return { tone: "warning", label: "Disconnected" };
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
    case "not_set_up":
      return { tone: "neutral", label: "Not set up" };
    default:
      return { tone: "neutral", label: "Stopped" };
  }
}

/** What a "Disconnected" badge doesn't say: the server isn't answering. */
export function installedRunnerNote(r: InstalledRunner): string {
  return r.state === "offline" ? "Can't reach the server. " : "";
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

export function InstalledRunnerSection(props: { management?: JSX.Element; picks: AttachSelection }) {
  return (
    <>
      <If when={installedRunner()}>
        {(r) => <If when={r().state !== "not_set_up"} else={<SetUp r={r()} />}>
          <Section r={r()} management={props.management} picks={props.picks} />
        </If>}
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

const DESCRIPTION = "Built into Metro and runs while Metro is open. Agents in Spaces you attach it to work in each Space's folder on this computer. Commands follow its shell policy.";

function Notes() {
  return (
    <>
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
    </>
  );
}

function SetUp(props: { r: InstalledRunner }) {
  return (
    <SettingsGroup title="This computer's runner" description={DESCRIPTION} testID="installed-runner">
      <If when={props.r.detail}>
        {(d) => (
          <Column padX={4} padY={2}>
            <Label variant="caption" color="muted" testID="installed-runner-detail">{d()}</Label>
          </Column>
        )}
      </If>
      <Notes />
      <Row padX={4} padY={3} justify="end">
        <Action testID="installed-runner-setup" label="Set up" busyLabel="Setting up…" variant="primary"
          busy={installedBusy() === "setup"} disabled={!props.r.canSetUp || !!installedBusy() || runnerAttachBusy() || !!runnerBusy()}
          onPress={() => void setupInstalledRunner()} />
      </Row>
    </SettingsGroup>
  );
}

function Section(props: { r: InstalledRunner; management?: JSX.Element; picks: AttachSelection }) {
  const copied = signal<string | null>(null);
  const r = () => props.r;
  const badge = () => installedRunnerBadge(r());
  const version = () => runners()?.find((item) => item.id === ownedInstalledRunner()?.id)?.presence?.version;
  // Which folders were chosen: a read, so rows can show them and offer Use default.
  effect(() => {
    const id = r().runner?.id;
    if (!id || ownedInstalledRunner()?.id !== id || chosenFolders() !== null) return;
    untracked(() => void loadChosenFolders());
  });
  return (
    <SettingsGroup
      title="This computer's runner"
      description={DESCRIPTION}
      testID="installed-runner"
    >
      <SettingRow
        label={r().runner?.name ?? (r().unconfirmedName ? `Not confirmed (asked for “${r().unconfirmedName}”)` : "Not registered yet")}
        description={`${installedRunnerNote(r())}${version() ? `v${version()}` : "Version unknown until it connects"}. Updates with Metro.${r().tools.length ? ` Tools: ${r().tools.join(", ")}.` : ""} It never runs checks or MCP servers.`}
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
      <AttachPicks picks={props.picks} />
      <SettingRow
        label="Folders on this computer"
        description={runnerSpaces().length ? "Each Space uses its own folder here unless you choose one. Changing it moves no files and applies to the next job."
          : installedFolderRefreshExhausted() ? "Couldn’t verify attached Spaces." : "Attached Spaces will appear here."}
        stacked
      >
        <If when={runnerSpaces().length > 0}>
          <Column gap={3} testID="installed-runner-spaces">
            <Each of={runnerSpaces()} key={(s) => s.id}>
              {(s) => <FolderRow space={s()} copied={copied()} onCopied={copied.set} />}
            </Each>
          </Column>
        </If>
      </SettingRow>
      <StaleFolders />
      <Notes />
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

const folderBusy = () => !!installedBusy() || runnerAttachBusy() || !!runnerBusy();

/** One attached Space's folder: show it, change it by typing a path, or go back to the default. */
function FolderRow(props: { space: { id: string; name: string }; copied: string | null; onCopied: (id: string) => void }) {
  const sid = () => props.space.id;
  const editing = signal(false);
  const draft = signal("");
  const loaded = () => folders()[sid()];
  const chosen = () => chosenFolders()?.[sid()];
  const shown = () => loaded()?.folder ?? chosen() ?? null;
  const custom = () => loaded() ? loaded()!.custom : chosen() !== undefined;
  const save = async () => {
    if (await setRunnerFolder(sid(), draft())) editing.set(false);
  };
  return (
    <Column gap={1} testID={`installed-runner-space:${sid()}`}>
      <Row align="center" gap={2} wrap>
        <Column grow>
          <Label variant="strong">{props.space.name}</Label>
        </Column>
        <If when={shown()} else={
          <Action testID={`installed-runner-folder:${sid()}`} label="Show folder" busyLabel="Opening…"
            busy={installedBusy() === `folder:${sid()}`} disabled={folderBusy()} variant="ghost"
            onPress={() => void showRunnerFolder(sid())} />
        }>
          {(path) => (
            <Action testID={`installed-runner-copy:${sid()}`} label={props.copied === sid() ? "Copied" : "Copy path"} variant="ghost"
              onPress={() => {
                Clipboard.writeText(path());
                props.onCopied(sid());
              }} />
          )}
        </If>
        <If when={!editing()}>
          <Action testID={`installed-runner-folder-change:${sid()}`} label="Change…" variant="ghost" disabled={folderBusy()}
            accessibilityLabel={`Change the folder for ${props.space.name}`}
            onPress={() => {
              draft.set(custom() ? shown() ?? "" : "");
              editing.set(true);
            }} />
        </If>
        <If when={custom()}>
          <Action testID={`installed-runner-folder-default:${sid()}`} label="Use default" busyLabel="Resetting…" variant="ghost"
            busy={installedBusy() === `folder-clear:${sid()}`} disabled={folderBusy()}
            accessibilityLabel={`Use the default folder for ${props.space.name}`}
            onPress={() => void clearRunnerFolder(sid())} />
        </If>
      </Row>
      <If when={shown()}>
        {(path) => (
          <Label variant="mono" lines={2} testID={`installed-runner-path:${sid()}`}>
            {path()}
          </Label>
        )}
      </If>
      <If when={shown() && custom()}>
        <Label variant="caption" color="muted" testID={`installed-runner-folder-chosen:${sid()}`}>Chosen folder</Label>
      </If>
      <If when={loaded()?.problem}>
        {(problem) => <Label variant="caption" color="danger" testID={`installed-runner-folder-problem:${sid()}`}>{`Jobs fail until this is fixed: ${problem()}`}</Label>}
      </If>
      <If when={editing()}>
        <Column gap={2} testID={`installed-runner-folder-editor:${sid()}`}>
          <Field testID={`installed-runner-folder-input:${sid()}`} accessibilityLabel={`Folder for ${props.space.name}`}
            value={draft()} onChange={draft.set} placeholder="/full/path/to/folder" disabled={folderBusy()}
            onSubmit={() => void save()} />
          <Label variant="caption" color="muted">An existing folder. Not your home folder, a drive root, Metro's data folder, or another Space's folder.</Label>
          <Row gap={2}>
            <Action testID={`installed-runner-folder-save:${sid()}`} label="Save" busyLabel="Saving…" variant="primary" compact
              busy={installedBusy() === `folder-set:${sid()}`} disabled={folderBusy() || !draft().trim()}
              onPress={() => void save()} />
            <Action testID={`installed-runner-folder-cancel:${sid()}`} label="Cancel" variant="ghost" compact
              disabled={installedBusy() === `folder-set:${sid()}`} onPress={() => editing.set(false)} />
          </Row>
        </Column>
      </If>
      <If when={folderErrors()[sid()]}>
        {(message) => <Label variant="caption" color="danger" testID={`installed-runner-folder-error:${sid()}`}>{message()}</Label>}
      </If>
    </Column>
  );
}

/** Folders chosen for Spaces this runner no longer serves still count as taken until cleared. */
function StaleFolders() {
  const stale = () => {
    const chosen = chosenFolders();
    const proof = verifiedDetached();
    // Only Spaces a complete, current scan proved this runner doesn't serve.
    if (!chosen || !proof || proof.serial !== installedFolderSnapshot() || proof.runnerId !== ownedInstalledRunner()?.id) return [];
    const attached = new Set(runnerSpaces().map((s) => s.id));
    return Object.entries(chosen).filter(([sid]) => proof.ids.has(sid) && !attached.has(sid)).map(([sid, folder]) => ({
      sid, folder, name: spaces().find((s) => s.id === sid)?.name ?? sid,
    }));
  };
  return (
    <If when={stale().length > 0}>
      <SettingRow label="Folders for Spaces it no longer serves" testID="installed-runner-stale-folders" stacked
        description="Other Spaces can't use these folders until you clear them.">
        <Column gap={2}>
          <Each of={stale()} key={(f) => f.sid}>
            {(f) => (
              <Column gap={1} testID={`installed-runner-stale-folder:${f().sid}`}>
                <Row align="center" gap={2} wrap>
                  <Column grow><Label variant="strong">{f().name}</Label></Column>
                  <Action testID={`installed-runner-stale-default:${f().sid}`} label="Use default" busyLabel="Clearing…" variant="ghost"
                    busy={installedBusy() === `folder-clear:${f().sid}`} disabled={folderBusy()}
                    accessibilityLabel={`Clear the chosen folder for ${f().name}`}
                    onPress={() => void clearRunnerFolder(f().sid)} />
                </Row>
                <Label variant="mono" lines={2}>{f().folder}</Label>
                <If when={folderErrors()[f().sid]}>
                  {(message) => <Label variant="caption" color="danger">{message()}</Label>}
                </If>
              </Column>
            )}
          </Each>
        </Column>
      </SettingRow>
    </If>
  );
}

const attachBusy = () => runnerAttachBusy() || !!installedBusy() || !!runnerBusy();

export function createAttachSelection(listVisible: () => boolean) {
  const checked = signal<ReadonlySet<string>>(new Set());
  let initial = true;
  let previousRunnerId: string | undefined;
  let sameSession = sessionGuard();
  effect(() => {
    runnerSessionKey();
    const local = installedRunner();
    const runnerId = local?.runner?.id;
    const choice = installedRunnerChoice();
    const visible = eligibleSpaces();
    const changed = !sameSession() || (previousRunnerId !== undefined && previousRunnerId !== runnerId);
    sameSession = sessionGuard();
    previousRunnerId = runnerId;

    if (changed || (local && !runnerId)) {
      // A different or newly registered runner must not inherit defaults.
      initial = false;
      checked.set(new Set());
    } else if (initial && choice && listVisible()) {
      // Only default the first verified list shown to the user, even if empty.
      initial = false;
      checked.set(new Set(visible.map((s) => s.id)));
    } else {
      // Losing eligibility revokes the pick; a returning row needs a new pick.
      const before = untracked(checked);
      const kept = visible.filter((s) => before.has(s.id));
      if (kept.length !== before.size) checked.set(new Set(kept.map((s) => s.id)));
    }
  });
  return {
    checked,
    setChecked: (id: string, on: boolean) => {
      if (attachBusy() || !eligibleSpaces().some((s) => s.id === id)) return;
      const next = new Set(checked());
      if (on) next.add(id);
      else next.delete(id);
      checked.set(next);
    },
  };
}

type AttachSelection = ReturnType<typeof createAttachSelection>;

function AttachPicks(props: { picks: AttachSelection }) {
  const selected = () => eligibleSpaces().filter((s) => props.picks.checked().has(s.id)).map((s) => s.id);
  const state = () => {
    if (!installedFolderRefreshExhausted() && checkingInstalledScope() && !installedRunnerChoice()) return "Checking this computer's runner…";
    if (ownedInstalledRunner()?.attachIfEmpty === false) return "This server doesn't support attaching only to Spaces without a runner.";
    if (!installedRunnerChoice()) return "This computer's runner isn't available to attach for this account.";
    if (eligibleSpaces().length === 0) return spaces().some((s) => s.role === "owner" && s.runnerPaired === null)
      ? "Some runner assignments haven’t been confirmed yet." : "Every Space you own has a runner.";
    return "Agents in the Spaces you choose work in their own folders on this computer.";
  };
  return (
    <SettingRow label="Spaces without a runner" description={state()} stacked testID="runner-attach-list">
      <If when={eligibleSpaces().length > 0}>
        <Column gap={2}>
          <Each of={eligibleSpaces()} key={(s) => s.id}>
            {(s) => <Checkbox testID={`runner-attach-pick:${s().id}`} label={s().name} checked={props.picks.checked().has(s().id)}
              disabled={attachBusy()} onChange={(checked) => props.picks.setChecked(s().id, checked)} />}
          </Each>
          <Action testID="runner-attach-many" label={`Attach to ${selected().length} ${selected().length === 1 ? "Space" : "Spaces"}`}
            variant="primary" busy={runnerAttachBusy()} busyLabel="Attaching…" disabled={attachBusy() || !selected().length}
            onPress={() => { if (!attachBusy()) void attachMany(selected()); }} />
        </Column>
      </If>
    </SettingRow>
  );
}
