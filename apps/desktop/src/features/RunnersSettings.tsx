// Settings › Runners: register a runner (personal, or organization-wide for
// org admins), get its token once with Docker setup steps, and rotate or
// delete the runners this user manages. Attaching happens per Space, in the
// Space owner's Manage view, from the same list.

import { batch, cleanup, Clipboard, effect, openUrl, signal } from "nsh";
import type { ViewRef } from "nsh";
import { orgAdmin, sessionGuard } from "../store";
import {
  cancelRunnerAction,
  confirmRunnerAction,
  createRunner,
  deleteRunner,
  loadRunners,
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
import { Action, Badge, Banner, Column, compactWidth, ConfirmInline, Disclosure, FactList, Field, Label, LearnMore, PageHeader, PageLayout, ProblemNote, Row, SegmentedControl, SettingRow, SettingsGroup, StatusSummary } from "../ui";
import { pinnedRunnerImage, requestRunnerUpdate, runnerAttachBusy, runnerAttachFeedback, runnerSessionKey, runnerUpdateBusy, runnerUpdateErrors, runnerUpdateTarget, selfUpdateTarget, watchInstalledRunner } from "../runnersState";
import { TokenReveal } from "./TokenReveal";
import { InstalledRunnerSection } from "./installedRunner/InstalledRunnerSection";
import { installedBusy, installedRunner, ownedInstalledRunner, refreshAfterInstalledRunnerDeletion } from "./installedRunner/installedRunner";
import { relativeTime } from "./discovery/time";
import { RELEASES_URL } from "./setupTemplates";

export function RunnersSettings() {
  watchInstalledRunner(true);
  const adding = signal(false);
  const token = () => revealed()?.kind === "runner";
  const hasMessages = () => !token() && !!(runnersError() || runnerNotice() || runnerAttachFeedback());
  const installedRegistration = () => runners()?.find((r) => r.visibility === "personal" && r.canManage && r.id === ownedInstalledRunner()?.id);
  effect(() => {
    runnerSessionKey();
    adding.set(false);
  });
  // Pending means accepted, not necessarily delivered yet. Only read while
  // waiting for a runner to report success or failure; no update is triggered here.
  effect(() => {
    if (!runners()?.some((r) => r.update?.pending)) return;
    const timer = setInterval(() => { if (!runnerBusy() && !runnerUpdateBusy()) void loadRunners(); }, 5000);
    cleanup(() => clearInterval(timer));
  });
  return (
    <PageLayout testID="runners-settings" pad={compactWidth() ? 4 : 6} gap={6} footer={hasMessages() ? <Messages /> : undefined}>
      <If
        when={token()}
        else={
          <Column gap={6}>
            <Column gap={2}>
              <PageHeader title="Runners" subtitle="A runner runs workspace tools for the Spaces you attach it to.">
                <Action testID="runner-add-toggle" label={adding() ? "Cancel" : "Add runner"} variant={adding() ? "ghost" : "primary"}
                  disabled={!!runnerBusy()} onPress={() => adding.set(!adding())} />
              </PageHeader>
              <LearnMore testID="runners-learn-more">
                <FactList
                  facts={[
                    { label: "Setup", value: "Register one, start it, then attach it to Spaces you own." },
                    { label: "Spaces", value: "One runner can serve several Spaces, each in its own folder. Each Space uses at most one runner." },
                  ]}
                />
              </LearnMore>
            </Column>
            <If when={adding()}><AddRunner onAdded={() => adding.set(false)} /></If>
            <InstalledRunnerSection management={
              <If when={installedRegistration()}>
                {(runner) => <Column padX={4} gap={2}><RunnerManagement runner={runner()} installed /></Column>}
              </If>
            } />
            <RunnerList visibility="personal" />
            <RunnerList visibility="organization" />
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
      <If when={runnerAttachFeedback()}>{(result) => (
        <Banner variant="inline" tone={result().incomplete ? "warning" : "success"} testID="runner-attach-result" message={result().message} />
      )}</If>
    </Column>
  );
}

function AddRunner(props: { onAdded: () => void }) {
  const name = signal("");
  const visibility = signal<RunnerVisibility>("personal");
  const submit = async () => {
    if (await createRunner(name(), visibility())) {
      props.onAdded();
    }
  };
  return (
    <SettingsGroup title="Add a runner" description="You'll get its token once, with the steps to start it." testID="add-runner-group">
      <SettingRow label="Name" description="Such as the machine it runs on." stacked>
        <Field testID="runner-name" accessibilityLabel="Runner name" value={name()} onChange={name.set} placeholder="Build box" maxLength={80} onSubmit={() => void submit()} />
      </SettingRow>
      <SettingRow
        label="Who can attach it"
        description={
          orgAdmin()
            ? "Personal: only you, to Spaces you own. Organization: any Space owner; org admins manage it."
            : "Personal: only you, to Spaces you own. Org admins add organization runners."
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

function RunnerList(props: { visibility: RunnerVisibility }) {
  const personal = () => props.visibility === "personal";
  const group = (list: RegisteredRunner[]) => list.filter((r) => r.visibility === props.visibility &&
    (!personal() || r.id !== ownedInstalledRunner()?.id));
  return (
    <SettingsGroup
      title={personal() ? "Your runners" : "Organization runners"}
      description={personal() ? "Only you can attach these to Spaces you own." : "Any Space owner can attach these. Organization admins manage them."}
      testID={personal() ? "runner-list" : "organization-runner-list"}
    >
      <If
        when={runners()}
        else={
          <SettingRow label="Loading runners…" />
        }
      >
        {(list) => (
          <Column>
            <If when={group(list()).length === 0}>
              <SettingRow label={personal() ? "No other personal runners" : "No organization runners"} />
            </If>
            <Each of={group(list())} key={(r) => r.id}>
              {(r) => <RunnerRow runner={r()} />}
            </Each>
          </Column>
        )}
      </If>
    </SettingsGroup>
  );
}

export function runnerStatus(r: RegisteredRunner, now = Date.now()): string {
  const p = r.presence;
  const version = p?.version ? `v${p.version}` : p?.seenAt != null ? "Version not reported" : "Version unknown until it connects";
  if (p?.online) return `${version} · Online`;
  return p?.seenAt != null ? `${version} · Last seen ${relativeTime(Math.min(p.seenAt, now), now)}` : version;
}

const registrationScope = (r: RegisteredRunner) => JSON.stringify([runnerSessionKey(), r.id, r.visibility, r.canManage]);

function RunnerRow(props: { runner: RegisteredRunner }) {
  const r = () => props.runner;
  const target = () => runnerUpdateTarget(r());
  return (
    <Column testID={`runner:${r().id}`}>
      <SettingRow label={r().name} description={runnerStatus(r())}>
        <Row align="center" gap={2} wrap justify="end">
          <If when={r().visibility === "organization"}>
            <Badge tone="agent" label="Organization" />
            <If when={r().canManage}><Badge tone="neutral" label="Org admin" /></If>
          </If>
          <If when={target()}>{(v) => <Badge tone="info" label={`Update ${v()}`} />}</If>
        </Row>
      </SettingRow>
      <Column padX={4} gap={2}>
        <RunnerUpdateControls runner={r()} scope={registrationScope(r())} />
        <If when={r().canManage}><RunnerManagement runner={r()} /></If>
      </Column>
    </Column>
  );
}

function RunnerManagement(props: { runner: RegisteredRunner; installed?: boolean }) {
  const r = () => props.runner;
  const confirming = () => (runnerConfirm()?.runnerId === r().id ? runnerConfirm()!.action : null);
  const canRotate = () => !props.installed || !installedRunner()?.canReconnect;
  const busy = () => !!runnerBusy() || !!runnerUpdateBusy() || (props.installed && (!!installedBusy() || runnerAttachBusy()));
  // Moving between installed and standalone controls invalidates the old panel.
  const scope = () => JSON.stringify([registrationScope(r()), !!props.installed]);
  const changeConfirmation = (action: "rotate" | "delete", open: boolean) => {
    if (open) void confirmRunnerAction(action, r().id);
    else if (confirming() === action) cancelRunnerAction();
  };
  effect(() => {
    if (!canRotate() && confirming() === "rotate") cancelRunnerAction();
  });
  cleanup(() => { if (confirming()) cancelRunnerAction(); });
  const remove = async () => {
    const id = r().id;
    const valid = sessionGuard();
    await deleteRunner(id);
    if (valid() && props.installed && !runners()?.some((runner) => runner.id === id)) {
      refreshAfterInstalledRunnerDeletion(id);
    }
  };
  return (
    <Column gap={2}>
      <If when={canRotate()}>
        <ConfirmInline testID={`rotate-runner:${r().id}`} triggerLabel="Rotate token…" confirmLabel="Rotate token"
          message={impactText("rotate", r(), props.installed)} scopeKey={scope()}
          open={confirming() === "rotate"} onOpenChange={(open) => changeConfirmation("rotate", open)}
          busyLabel="Rotating…" busy={runnerBusy() === `rotate:${r().id}`} disabled={busy()}
          onConfirm={() => void rotateRunner(r().id)} />
      </If>
      <ConfirmInline testID={`delete-runner:${r().id}`} triggerLabel="Delete…" confirmLabel="Delete runner"
        message={impactText("delete", r())} scopeKey={scope()}
        open={confirming() === "delete"} onOpenChange={(open) => changeConfirmation("delete", open)}
        busyLabel="Deleting…" busy={runnerBusy() === `delete:${r().id}`} disabled={busy()}
        onConfirm={() => void remove()} />
    </Column>
  );
}

function RunnerUpdateControls(props: { runner: RegisteredRunner; scope: string }) {
  const dockerOpen = signal(false);
  const copied = signal<string | null>(null);
  const r = () => props.runner;
  const u = () => r().update;
  const target = () => runnerUpdateTarget(r());
  const failure = () => u()?.lastResult?.state === "failed" && u()?.lastResult?.target === target() && !u()?.pending ? u()?.lastResult : null;
  const error = () => runnerUpdateErrors()[r().id];
  const eligible = () => selfUpdateTarget(r());
  const intent = signal<{ target: string; scope: string } | null>(null);
  const confirmOpen = signal(false);
  const submitting = signal(false);
  const pending = () => u()?.pending;
  const pendingText = () => { const p = pending(); return p ? `Update to ${p.target} requested` : null; };
  const resultText = () => pendingText() ?? error();
  let result: ViewRef | undefined;
  let mounted = true;
  cleanup(() => { mounted = false; });
  // Keep the captured target, not the live offer, wired to the mounted
  // panel. Eligibility is rechecked before sending and never retargets it.
  const confirmationKey = () => JSON.stringify([runnerSessionKey(), r().id, intent()]);
  const changeConfirmation = (open: boolean) => {
    if (!open) {
      confirmOpen.set(false);
      return;
    }
    const target = eligible();
    if (!target || runnerUpdateBusy() || runnerBusy()) return;
    batch(() => {
      intent.set({ target, scope: props.scope });
      confirmOpen.set(true);
    });
  };
  const confirm = async () => {
    const request = intent();
    if (!request || !confirmOpen() || submitting() || runnerUpdateBusy() || runnerBusy()
        || request.scope !== props.scope || request.target !== eligible()) return;
    const valid = sessionGuard();
    submitting.set(true);
    try {
      await requestRunnerUpdate(r().id, request.target);
    } finally {
      if (mounted && valid() && request === intent()) {
        batch(() => {
          confirmOpen.set(false);
          submitting.set(false);
        });
      }
    }
  };
  effect(() => {
    const request = intent();
    if (confirmOpen() && !submitting() && (request?.scope !== props.scope || request?.target !== eligible())) confirmOpen.set(false);
  });
  effect(() => {
    props.scope;
    target();
    dockerOpen.set(false);
    copied.set(null);
  });
  return (
    <Column gap={2}>
      <If when={r().presence?.install === "desktop"}>
        <Label variant="caption" color="muted">Updates with Metro.</Label>
      </If>
      <If when={target() && !r().canManage && r().visibility === "organization"}>
        <Label variant="caption" color="muted">An org admin can update it.</Label>
      </If>
      <If when={target() && r().canManage && r().presence?.install === "container" && u()?.action === "container"}>
        <Disclosure title="How to update" testID={`runner-update-docker:${r().id}`} open={dockerOpen()} onToggle={dockerOpen.set}>
          <If when={pinnedRunnerImage(r())} else={<ProblemNote text="The server did not provide a valid pinned Metro runner image. Check the official release before updating." />}>
            {(image) => (
              <Column gap={2}>
                <Label variant="mono">{`docker pull ${image()}`}</Label>
                <Label variant="caption" color="muted">Pull the pinned image, then recreate the runner container with that image. Keep its existing configuration and volume mounts. Restarting the old container won't change its image. Metro can't reach Docker from here.</Label>
                <Action testID={`runner-copy-image:${r().id}`} label={copied() === image() ? "Copied" : "Copy image"} onPress={() => {
                  Clipboard.writeText(image());
                  copied.set(image());
                }} />
              </Column>
            )}
          </If>
        </Disclosure>
      </If>
      <If when={target() && r().canManage && u()?.action === "install_script" &&
        (r().presence?.install === "managed" || r().presence?.install === "unmanaged")}>
        <Row align="center" gap={2} wrap>
          <Label variant="caption" color="muted">Update manually on the runner's host using the verified Metro runner release and install instructions.</Label>
          <Action testID={`runner-update-release:${r().id}`} label="Open Metro release" variant="ghost"
            onPress={() => {
              const version = target();
              if (version) void openUrl(`${RELEASES_URL}/tag/v${version}`).catch(() => {});
            }} />
        </Row>
      </If>
      <If when={target() && r().canManage && u()?.action === "self_update" && !r().presence?.online && !u()?.pending}>
        <Label variant="caption" color="muted">Bring this runner online to request an update.</Label>
      </If>
      <StatusSummary viewRef={(node) => { result = node; }} focusable={!eligible() && !!resultText() && !submitting()}
        accessibilityLabel={!eligible() && resultText() ? `${r().name}: ${resultText()}` : undefined} testID={`runner-update-result:${r().id}`}>
        <If when={pendingText()}>{(text) => <Label variant="caption" color="muted">{text()}</Label>}</If>
        <If when={error()}>{(text) => <ProblemNote testID={`runner-update-error:${r().id}`} text={`${r().name}: ${text()}`} />}</If>
      </StatusSummary>
      <If when={failure()}>
        {(last) => <ProblemNote testID={`runner-update-failed:${r().id}`}
          text={`Update to ${last().target} failed on ${r().name}: ${updateFailure(last().code)}${r().presence?.version ? ` Last reported version: v${r().presence?.version}.` : ""}`} />}
      </If>
      <ConfirmInline testID={`runner-update:${r().id}`}
        triggerLabel={eligible() || confirmOpen() ? (failure() || error() ? "Try again…" : "Update…") : ""}
        confirmLabel="Update runner" variant="primary" scopeKey={confirmationKey()}
        open={confirmOpen()} onOpenChange={changeConfirmation}
        disabled={!eligible() || !!runnerBusy() || !!runnerUpdateBusy()} busy={submitting()} busyLabel="Requesting…"
        message={`${r().name} downloads ${intent()?.target} from GitHub, verifies the signature itself and restarts. Jobs pause for a moment.`}
        returnFocusAfterBusy={() => !eligible() && resultText() ? result : undefined}
        onConfirm={() => void confirm()} />
    </Column>
  );
}

function updateFailure(code: string | null): string {
  switch (code) {
    case "verify_failed": return "The runner could not verify the release.";
    case "trust_root_stale": return "The runner's verification trust data is out of date.";
    case "download_failed": return "The runner could not download the release.";
    case "start_failed": case "start_check_failed": case "exec_failed": return "The new version did not start.";
    case "manual_migration_required": return "Update on the runner's host to migrate this installation.";
    default: return "The runner did not complete the update.";
  }
}

function impactText(action: "rotate" | "delete", r: RegisteredRunner, installed = false): string {
  const impact = runnerImpact();
  const known = impact?.runnerId === r.id ? impact : null;
  let where: string;
  if (!known || known.spaces === null) where = "Checking which of your Spaces use it…";
  else if (known.spaces.length === 0) where = known.partial ? "None of the Spaces you belong to use it, but other Spaces in the organization may." : "None of your Spaces use it.";
  else where = `Spaces you belong to that use it: ${known.spaces.join(", ")}.${known.partial ? " Other Spaces in the organization may use it too." : ""}`;
  return action === "rotate"
    ? `Rotating the token for “${r.name}” stops the current token at once, in every Space this runner serves. ${installed
      ? "This computer's runner will disconnect. Reopen Runners to check its status and use Reconnect when it becomes available."
      : "Put the new token in its runner.env and restart it."} ${where}`
    : `Deleting “${r.name}” revokes its token and detaches it from every Space it serves; those Spaces have no runner until their owners attach another. ${where}`;
}
