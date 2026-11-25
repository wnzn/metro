// The Space owner's switch for local workspace tools, in Runner › Advanced. Shown
// only when the packaged launcher says this session is this computer's own
// local workspace (status `available`). Never shows a token.

import { Clipboard, cleanup, computed, effect, signal, untracked } from "nsh";
import { connectedTo, me } from "../../store";
import type { SpaceRunner } from "../../api/client";
import { beginSpaceRunnerWrite } from "../../settings";
import { Action, Badge, Column, Label, Row, SettingRow, SettingsGroup, Spacer } from "../../ui";
import type { Tone } from "../../ui";
import {
  disableLocalTools,
  enableLocalTools,
  followStarting,
  loadLocalTools,
  localTools,
  localToolsBusy,
  localToolsFor,
} from "./localTools";
import type { LocalToolsSpace } from "./localTools";

export function localToolsBadge(e: LocalToolsSpace | null): { tone: Tone; label: string } {
  if (!e) return { tone: "neutral", label: "Off" };
  if (e.state === "revoked" || e.worker === "revoked") return { tone: "danger", label: "Pairing revoked" };
  switch (e.worker) {
    case "online":
      return { tone: "success", label: "On" };
    case "offline":
      return { tone: "warning", label: "Can't reach the local server" };
    case "starting":
      return { tone: "neutral", label: "Starting…" };
    default:
      return { tone: "neutral", label: "Paused" };
  }
}

export function LocalToolsCard(props: { spaceId: string | null; runner: SpaceRunner | null | undefined; disabled: boolean; refreshRunner: (spaceId: string, stillLatest: () => boolean) => Promise<void> }) {
  const confirm = signal(false);
  const copied = signal(false);
  const sid = () => props.spaceId ?? undefined;
  const entry = () => localToolsFor(sid());
  const badge = () => localToolsBadge(entry());
  let open = true;
  cleanup(() => { open = false; });
  effect(() => {
    const id = sid();
    if (id) untracked(() => void loadLocalTools(() => open && sid() === id));
  });
  const blocked = () => props.disabled || !!localToolsBusy();
  const change = async (enable: boolean) => {
    const id = sid();
    if (!open || !id || blocked() || (enable && props.runner !== null)) return;
    const stillOpen = () => open && sid() === id;
    const latest = beginSpaceRunnerWrite(id);
    if (enable) await enableLocalTools(id, stillOpen);
    else await disableLocalTools(id, stillOpen);
    if (latest()) await props.refreshRunner(id, latest);
  };
  // While this Space's worker is starting, follow it here (bounded), so the
  // owner sees "On" without leaving the page. Keyed by Space, owner and
  // server: a new key (or leaving, or signing out) ends the old follow-up.
  const startingKey = computed(() => {
    const id = sid();
    const e = entry();
    if (!id || !e || e.state !== "on" || e.worker !== "starting") return null;
    return `${connectedTo() ?? ""}\n${me() ?? ""}\n${id}`;
  });
  effect(() => {
    const key = startingKey();
    if (!key) return;
    const id = untracked(sid)!;
    return untracked(() =>
      followStarting(id, () => open && startingKey() === key && sid() === id && !!localTools()?.available),
    );
  });
  // Another runner already serves this Space (not this computer's pairing).
  const otherRunner = () => {
    const r = props.runner;
    return !!r && entry()?.state !== "on";
  };
  return (
    <If when={localTools()?.available}>
      <SettingsGroup title="Local tools on this computer" testID="local-tools-card"
        description="Older single-Space pairing for this computer. Prefer this computer's runner above.">
        <SettingRow label="Local pairing" description="Lets agents list and read files in one private folder while Metro is open. No file changes or commands.">
          <Badge testID="local-tools-state" tone={badge().tone} label={badge().label} />
        </SettingRow>

        <If when={entry()}>
          {(e) => (
            <SettingRow label="Workspace folder" testID="local-tools-on" stacked>
              <If when={e().detail}>
                {(d) => (
                  <Label variant="caption" color={e().state === "revoked" ? "danger" : "muted"} testID="local-tools-detail">
                    {d()}
                  </Label>
                )}
              </If>
              <Column gap={1}>
                <Row align="center" gap={2}>
                  <Column grow>
                    <Label variant="mono" lines={2} testID="local-tools-folder">
                      {e().folder}
                    </Label>
                  </Column>
                  <Action
                    testID="local-tools-copy-folder"
                    label={copied() ? "Copied" : "Copy path"}
                    variant="ghost"
                    onPress={() => {
                      Clipboard.writeText(e().folder);
                      copied.set(true);
                    }}
                  />
                </Row>
                <Label variant="caption" color="muted">
                  Put the files this Space's agent may read in this folder. Links inside it are never followed.
                </Label>
              </Column>
              {/* Pair again (when revoked) on the leading edge; Turn off, and its confirmation, on the trailing one. */}
              <Row align="center" gap={2} wrap>
                <If when={e().state === "revoked" && props.runner === null}>
                  <Action
                    testID="local-tools-enable"
                    label="Pair again"
                    busyLabel="Pairing…"
                    busy={localToolsBusy() === "enable"}
                    disabled={blocked()}
                    onPress={() => void change(true)}
                  />
                </If>
                <Spacer />
                <If
                  when={confirm()}
                  else={<Action variant="destructive" testID="local-tools-disable" label="Turn off…" disabled={blocked()} onPress={() => confirm.set(true)} />}
                >
                  <Action label="Cancel" variant="ghost" onPress={() => confirm.set(false)} />
                  <Action
                    testID="local-tools-confirm-disable"
                    label="Turn off: this computer stops serving this Space"
                    busyLabel="Turning off…"
                    busy={localToolsBusy() === "disable"}
                    disabled={blocked()}
                    variant="danger"
                    onPress={() => {
                      confirm.set(false);
                      void change(false);
                    }}
                  />
                </If>
              </Row>
            </SettingRow>
          )}
        </If>

        <If when={!entry()}>
          <If
            when={!otherRunner()}
            else={
              <SettingRow testID="local-tools-runner-present" label="This Space already has a runner"
                description="Local tools never replace it. Detach the runner or revoke its token first." />
            }
          >
            <SettingRow label="Turn on local tools">
              <Action
                testID="local-tools-enable"
                label="Turn on local tools"
                busyLabel="Turning on…"
                busy={localToolsBusy() === "enable"}
                disabled={blocked() || props.runner === undefined}
                variant="secondary"
                onPress={() => void change(true)}
              />
            </SettingRow>
          </If>
        </If>
      </SettingsGroup>
    </If>
  );
}
