// One agent turn's activity in the transcript: a single collapsed disclosure
// ("Metro is thinking…" while live, "Metro’s activity" once ended) that opens
// to every step in order — tool calls with their arguments, results, and
// summaries the model provider shared — each shown in full (very long output
// still folds). Opened with a press, Space or Return; the open state lives in
// the disclosure store under the group's stable key, so polls never reset it.

import { Action, Badge, Column, Label, Rail, Row } from "../../ui";
import type { Tone } from "../../ui";
import { disclosureKey, disclosureScope, isOpen, toggle } from "./disclosure";
import type { ActivityGroup, Step } from "./group";
import { groupAccessibleLabel, groupOutcomeText, groupTitle, routeBadge, routeText } from "./group";

const CLAMP_LINES = 6;

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
  const id = () => props.group.key;
  return (
    <Row gap={3} testID={`activity:${id()}`}>
      <Rail tone={railTone(props.group)} />
      <Column grow gap={1} padY={1}>
        <Row align="center" gap={2} wrap>
          <If
            when={hasDetails()}
            else={
              <Label variant="caption" color="muted" testID={`activity-title:${id()}`} accessibilityLabel={groupAccessibleLabel(props.group)}>
                {groupTitle(props.group)}
              </Label>
            }
          >
            <Action
              testID={`activity-toggle:${id()}`}
              variant="ghost"
              label={groupTitle(props.group)}
              expanded={expanded()}
              accessibilityLabel={`${expanded() ? "Hide" : "Show"} details: ${groupAccessibleLabel(props.group)}`}
              onPress={() => toggle(key())}
            />
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
            {(text) => <Badge testID={`activity-route-badge:${id()}`} tone="neutral" label={text()} />}
          </If>
        </Row>
        <If when={hasDetails() && expanded()}>
          <Column gap={2} selectable testID={`activity-details:${id()}`}>
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
  return (
    <Column gap={1} testID={`activity-step:${props.step.eventId}`}>
      <Row align="center" gap={2} wrap>
        <Label variant="caption" color={a().tone === "danger" ? "danger" : "muted"}>
          {a().summary}
        </Label>
        <If when={a().tone === "danger"}>
          <Badge tone="danger" label="failed" />
        </If>
      </Row>
      <If when={a().details.length > 0}>
        <FoldableText
          text={a().details}
          mono={a().mono}
          stateKey={disclosureKey(disclosureScope(), props.groupKey, `all:${props.step.eventId}`)}
        />
      </If>
    </Column>
  );
}

/**
 * Text that folds past a few lines. Its state lives in the disclosure store
 * under `stateKey`, so it survives rerenders and recycled rows.
 */
export function FoldableText(props: { text: string; mono: boolean; stateKey: string }) {
  const all = () => isOpen(props.stateKey);
  const lineCount = () => props.text.split("\n").length;
  const long = () => lineCount() > CLAMP_LINES + 2 || props.text.length > 1200;
  return (
    <Column gap={1}>
      <Label variant={props.mono ? "mono" : "body"} lines={long() && !all() ? CLAMP_LINES : undefined}>
        {props.text}
      </Label>
      <If when={long()}>
        <Row>
          <Action label={all() ? "Show less" : `Show all ${lineCount()} lines`} variant="ghost" onPress={() => toggle(props.stateKey)} />
        </Row>
      </If>
    </Column>
  );
}
