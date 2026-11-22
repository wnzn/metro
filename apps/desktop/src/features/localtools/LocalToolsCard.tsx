// The Space owner's switch for local workspace tools, in Manage Space. Shown
// only when the packaged launcher says this session is this computer's own
// local workspace (status `available`). Never shows a token.

import { Clipboard, computed, effect, signal, untracked } from "nsh";
import { activeSpace, connectedTo, me } from "../../store";
import { activeSpaceRunner } from "../../settings";
import { Action, Badge, Banner, Column, Label, Row, Surface } from "../../ui";
import type { Tone } from "../../ui";
import {
  disableLocalTools,
  enableLocalTools,
  followStarting,
  localTools,
  localToolsBusy,
  localToolsError,
  localToolsFor,
  localToolsNotice,
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

export function LocalToolsCard() {
  const confirm = signal(false);
  const copied = signal(false);
  const sid = () => activeSpace()?.id;
  const entry = () => localToolsFor(sid());
  const badge = () => localToolsBadge(entry());
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
      followStarting(id, () => startingKey() === key && activeSpace()?.id === id && !!localTools()?.available),
    );
  });
  // Another runner already serves this Space (not this computer's pairing).
  const otherRunner = () => {
    const r = activeSpaceRunner();
    return !!r && entry()?.state !== "on";
  };
  return (
    <If when={localTools()?.available}>
      <Surface variant="card" pad={4} gap={3} testID="local-tools-card">
        <Row align="center" gap={2}>
          <Column grow gap={1}>
            <Label variant="heading">Local tools on this computer</Label>
            <Label variant="caption" color="muted">
              Agents in this Space can list and read files in one private folder on this computer, while Metro is open. They can't change files, run commands or see anything else on your disk.
            </Label>
          </Column>
          <Badge testID="local-tools-state" tone={badge().tone} label={badge().label} />
        </Row>

        <If when={localToolsError()}>
          {(m) => (
            <Banner tone="danger" testID="local-tools-error">
              <Label color="danger">{m()}</Label>
            </Banner>
          )}
        </If>
        <If when={localToolsNotice()}>
          {(m) => (
            <Label variant="caption" color="muted" testID="local-tools-notice">
              {m()}
            </Label>
          )}
        </If>

        <If when={entry()}>
          {(e) => (
            <Column gap={2} testID="local-tools-on">
              <If when={e().detail}>
                {(d) => (
                  <Label variant="caption" color={e().state === "revoked" ? "danger" : "muted"} testID="local-tools-detail">
                    {d()}
                  </Label>
                )}
              </If>
              <Column gap={1}>
                <Label variant="strong">Folder</Label>
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
              <If when={e().state === "revoked"}>
                <Row>
                  <Action
                    testID="local-tools-enable"
                    label="Pair again"
                    busyLabel="Pairing…"
                    busy={localToolsBusy() === "enable"}
                    disabled={!!localToolsBusy()}
                    onPress={() => void enableLocalTools(e().spaceId)}
                  />
                </Row>
              </If>
              <If
                when={confirm()}
                else={
                  <Row>
                    <Action testID="local-tools-disable" label="Turn off…" variant="ghost" disabled={!!localToolsBusy()} onPress={() => confirm.set(true)} />
                  </Row>
                }
              >
                <Row gap={2} wrap>
                  <Action
                    testID="local-tools-confirm-disable"
                    label="Turn off: this computer stops serving this Space"
                    busyLabel="Turning off…"
                    busy={localToolsBusy() === "disable"}
                    variant="danger"
                    onPress={() => {
                      confirm.set(false);
                      void disableLocalTools(e().spaceId);
                    }}
                  />
                  <Action label="Cancel" variant="ghost" onPress={() => confirm.set(false)} />
                </Row>
              </If>
            </Column>
          )}
        </If>

        <If when={!entry()}>
          <If
            when={!otherRunner()}
            else={
              <Label variant="caption" color="muted" testID="local-tools-runner-present">
                This Space already has a runner. Local tools never replace it: detach or revoke it below first.
              </Label>
            }
          >
            <Row>
              <Action
                testID="local-tools-enable"
                label="Turn on local tools"
                busyLabel="Turning on…"
                busy={localToolsBusy() === "enable"}
                disabled={!!localToolsBusy() || activeSpaceRunner() === undefined}
                variant="primary"
                onPress={() => {
                  const id = sid();
                  if (id) void enableLocalTools(id);
                }}
              />
            </Row>
          </If>
        </If>
      </Surface>
    </If>
  );
}
