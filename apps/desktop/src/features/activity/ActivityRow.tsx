// One agent turn's activity in the transcript: a single collapsed disclosure
// ("Metro is thinking…" with a live indicator while it works, "Metro’s
// activity" once ended) that opens to every step in order: tool calls with
// their arguments, results, and summaries the model provider shared. Each step
// has a phase icon; tool output sits in a bounded box that folds past a few
// lines and can be copied whole. A routed turn shows a compact "Routed: …"
// badge; the full sentence about why is only in the opened details.
//
// Opened with a press, Space or Return; the open state lives in the
// disclosure store under the group's stable key, so polls never reset it.

import { Clipboard, announce, signal } from "nsh";
import { Action, Badge, Column, Label, Rail, Row } from "../../ui";
import type { Tone } from "../../ui";
import { CodeOutput, DisclosureToggle, StepIcon, ThinkingIndicator } from "../../ui/activity";
import { stepPhase } from "./classify";
import { disclosureKey, disclosureScope, isOpen, toggle } from "./disclosure";
import type { ActivityGroup, Step } from "./group";
import { groupAccessibleLabel, groupOutcomeText, groupTitle, routeBadge, routeText } from "./group";
import { FOLD_LINES, outputView } from "./output";

function railTone(g: ActivityGroup): Tone {
  if (groupOutcomeText(g) === "failed") return "danger";
  if (g.outcome === "cancelled" || g.outcome === "interrupted") return "warning";
  return g.source === "agent" ? "agent" : "runner";
}

export function ActivityGroupRow(props: { group: ActivityGroup }) {
  const key = () => disclosureKey(disclosureScope(), props.group.key);
  const expanded = () => isOpen(key());
  const hasSteps = () => props.group.steps.length > 0;
  /** Something to open: its steps, or the route it took. */
  const hasDetails = () => hasSteps() || !!props.group.route;
  const outcome = () => groupOutcomeText(props.group);
  /** Still working, and nothing has failed: the live indicator shows. */
  const working = () => props.group.outcome === "live" && !props.group.failed;
  const id = () => props.group.key;
  return (
    <Row gap={3} testID={`activity:${id()}`}>
      <Rail tone={railTone(props.group)} />
      <Column grow gap={2} padY={1}>
        <Row align="center" gap={2} wrap>
          <If
            when={hasDetails()}
            else={
              <Row align="center" gap={2}>
                <Label variant="caption" color="muted" testID={`activity-title:${id()}`} accessibilityLabel={groupAccessibleLabel(props.group)}>
                  {groupTitle(props.group)}
                </Label>
                <If when={working()}>
                  <Row testID={`activity-live:${id()}`}>
                    <ThinkingIndicator tone={props.group.source === "agent" ? "agent" : "runner"} testID={`activity-live-dots:${id()}`} />
                  </Row>
                </If>
              </Row>
            }
          >
            <DisclosureToggle
              testID={`activity-toggle:${id()}`}
              label={groupTitle(props.group)}
              expanded={expanded()}
              accessibilityLabel={`${expanded() ? "Hide" : "Show"} details: ${groupAccessibleLabel(props.group)}`}
              onPress={() => toggle(key())}
            >
              <If when={working()}>
                <Row testID={`activity-live:${id()}`}>
                  <ThinkingIndicator tone={props.group.source === "agent" ? "agent" : "runner"} testID={`activity-live-dots:${id()}`} />
                </Row>
              </If>
            </DisclosureToggle>
          </If>
          <If when={outcome()}>
            {(text) => <Badge testID={`activity-outcome:${id()}`} tone={text() === "failed" ? "danger" : text() === "running" ? "runner" : "warning"} label={text()} />}
          </If>
          <If when={hasSteps()}>
            <Label variant="caption" color="muted">
              {props.group.steps.length === 1 ? "1 step" : `${props.group.steps.length} steps`}
            </Label>
          </If>
          <If when={routeBadge(props.group.route)}>
            {(text) => <Badge testID={`activity-route-badge:${id()}`} tone="agent" label={text()} />}
          </If>
        </Row>
        <If when={hasDetails() && expanded()}>
          <Column gap={3} selectable testID={`activity-details:${id()}`}>
            <If when={props.group.route}>
              {(r) => (
                <Label variant="caption" color="muted" testID={`activity-route:${id()}`}>
                  {routeText(r())}
                </Label>
              )}
            </If>
            <Each of={props.group.steps} key={(s) => String(s.eventId)}>
              {(step) => <StepView groupKey={id()} step={step()} />}
            </Each>
          </Column>
        </If>
      </Column>
    </Row>
  );
}

function StepView(props: { groupKey: string; step: Step }) {
  const a = () => props.step.activity;
  const stateKey = () => disclosureKey(disclosureScope(), props.groupKey, `all:${props.step.eventId}`);
  return (
    <Column gap={1} testID={`activity-step:${props.step.eventId}`}>
      <Row align="center" gap={2} wrap>
        <StepIcon phase={stepPhase(a())} testID={`activity-step-icon:${props.step.eventId}:${stepPhase(a())}`} />
        <Label variant="caption" color={a().tone === "danger" ? "danger" : "muted"}>
          {a().summary}
        </Label>
        <If when={a().tone === "danger"}>
          <Badge tone="danger" label="failed" />
        </If>
      </Row>
      <If when={a().details.length > 0}>
        <If when={a().mono} else={<FoldableText text={a().details} mono={false} stateKey={stateKey()} />}>
          <StepOutput text={a().details} stateKey={stateKey()} testID={`activity-output:${props.step.eventId}`} />
        </If>
      </If>
    </Column>
  );
}

/** Tool arguments or output: bounded, folded past a few lines, copied whole. */
function StepOutput(props: { text: string; stateKey: string; testID: string }) {
  const open = () => isOpen(props.stateKey);
  const view = () => outputView(props.text, open());
  const copied = signal(false);
  const copy = () => {
    Clipboard.writeText(props.text)
      .then(() => {
        copied.set(true);
        announce("Copied to the clipboard");
        setTimeout(() => copied.set(false), 1500);
      })
      .catch(() => announce("Couldn't copy"));
  };
  return (
    <CodeOutput
      testID={props.testID}
      text={view().shown}
      lines={view().foldable && !open() ? FOLD_LINES : undefined}
      toggleLabel={view().toggleLabel}
      onToggle={() => toggle(props.stateKey)}
      copyLabel={copied() ? "Copied" : "Copy"}
      onCopy={copy}
      notice={view().notice}
    />
  );
}

/**
 * Text that folds past a few lines (messages and plain step text). Its state
 * lives in the disclosure store under `stateKey`, so it survives rerenders and
 * recycled rows.
 */
export function FoldableText(props: { text: string; mono: boolean; stateKey: string }) {
  const all = () => isOpen(props.stateKey);
  const view = () => outputView(props.text, all(), false);
  return (
    <Column gap={1}>
      <Label variant={props.mono ? "mono" : "body"} lines={view().foldable && !all() ? FOLD_LINES : undefined}>
        {view().shown}
      </Label>
      <If when={view().toggleLabel}>
        {(label) => (
          <Row>
            <Action label={label()} variant="ghost" onPress={() => toggle(props.stateKey)} />
          </Row>
        )}
      </If>
      <If when={view().notice}>
        {(notice) => (
          <Label variant="caption" color="muted">
            {notice()}
          </Label>
        )}
      </If>
    </Column>
  );
}
