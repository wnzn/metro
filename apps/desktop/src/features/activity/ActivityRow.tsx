// One agent turn's activity in the transcript, as one block.
//
// Collapsed (the default) it is a single summary line: the agent's name (and
// picture, once events carry one), then "thought 12s · 2 tools · Routed: …",
// and a pill only when the turn failed. While the turn is live the line says
// what it is doing now ("Reading README.md…") with a working indicator and,
// when the run is known, Stop; once it ends it settles into the summary.
//
// Opened, it is a vertical timeline: the route the turn took (which model
// answered and why), then each tool call with its short arguments and its
// output in a bounded code view that can be copied whole, the reasoning
// summaries the model provider shared as plain text, and any failure as one
// sentence with the technical text folded under "Details".
//
// Opened with a press, Space or Return; the open state lives in the
// disclosure store under the group's stable key, so polls never reset it.

import { Clipboard, announce, signal } from "nsh";
import type { JSX } from "nsh";
import { isActiveRun, me, runBusy, runs, stopRun } from "../../store";
import { webSearchSettingsAction } from "../../webSearch";
import { WEB_PROVIDERS } from "../../api/webSearch";
import type { WebToolNote } from "../../api/webSearch";
import { Action, Badge, Column, Label, Row } from "../../ui";
import { AgentMark, CodeOutput, DisclosureToggle, SummaryParts, ThinkingIndicator, TimelineEntry } from "../../ui/activity";
import { stepPhase } from "./classify";
import { disclosureKey, disclosureScope, isOpen, toggle } from "./disclosure";
import { failureText } from "./failure";
import type { ActivityGroup, Step, TimelineItem } from "./group";
import { groupAccessibleLabel, liveTitle, groupOutcomeText, routeText, summaryParts, timeline } from "./group";
import { FOLD_LINES, outputView } from "./output";
import { webAttemptRows, webFailureText } from "./web";

/** Old events without the server's durable attribution never get a shortcut. */
const ownWebFailure = (note: WebToolNote) => !!note.starterId && note.starterId === me();

export function ActivityGroupRow(props: {
  group: ActivityGroup;
  /** The agent working this turn, once events name one per agent; else the group's own name. */
  agentName?: string;
  /** Its picture (an avatar element); else the agent glyph. */
  agentAvatar?: JSX.Element;
}) {
  const g = () => props.group;
  const key = () => disclosureKey(disclosureScope(), g().key);
  const expanded = () => isOpen(key());
  const id = () => g().key;
  const name = () => (g().source === "agent" ? props.agentName || g().name : "Runner activity");
  /** Something to open: its steps, or the route it took. */
  const hasDetails = () => g().steps.length > 0 || !!g().route || !!g().pick;
  /** Still working, and nothing has failed: the live line shows. */
  const working = () => g().outcome === "live" && !g().failed;
  const failed = () => groupOutcomeText(g()) === "failed";
  const title = () => (working() ? liveTitle(g(), name()) : name());
  const parts = () => {
    if (working()) return [];
    const p = summaryParts(g());
    return [
      p.pick && { text: p.pick, testID: `activity-pick:${id()}` },
      p.duration && { text: p.duration },
      p.tools && { text: p.tools, testID: `activity-tools:${id()}` },
      p.route && { text: p.route, testID: `activity-route-badge:${id()}` },
      p.stopped && { text: p.stopped, testID: `activity-outcome:${id()}` },
    ].filter((x): x is { text: string; testID?: string } => !!x);
  };
  // While live, a route still shows: it says which model is answering.
  const liveRoute = () => (working() ? summaryParts(g()).route : null);
  const livePick = () => (working() ? summaryParts(g()).pick : null);
  /** The Space run behind this turn, while it can still be stopped. */
  const run = () => {
    const aid = g().activityId;
    return working() && aid ? runs().find((r) => r.id === aid && isActiveRun(r)) ?? null : null;
  };
  const label = () => groupAccessibleLabel(g(), name());
  const mark = () => (g().source === "agent" ? <AgentMark picture={props.agentAvatar} /> : undefined);
  const webFailure = () => g().steps.find((s) => s.activity.web?.note.failure)?.activity.web;
  return (
    <Column gap={2} padY={1} testID={`activity:${id()}`}>
      <Row align="center" gap={2}>
        <If
          when={hasDetails()}
          else={
            <Row align="center" gap={2}>
              {mark()}
              <Label variant="caption" color="muted" testID={`activity-title:${id()}`} accessibilityLabel={label()}>
                {title()}
              </Label>
              <SummaryParts parts={parts()} />
            </Row>
          }
        >
          <DisclosureToggle
            testID={`activity-toggle:${id()}`}
            label={title()}
            leading={mark()}
            expanded={expanded()}
            accessibilityLabel={`${expanded() ? "Hide" : "Show"} details: ${label()}`}
            onPress={() => toggle(key())}
          >
            <SummaryParts parts={parts()} />
            <If when={livePick()}>{(p) => <SummaryParts parts={[{ text: p(), testID: `activity-pick:${id()}` }]} />}</If>
            <If when={liveRoute()}>{(r) => <SummaryParts parts={[{ text: r(), testID: `activity-route-badge:${id()}` }]} />}</If>
          </DisclosureToggle>
        </If>
        <If when={working()}>
          <Row testID={`activity-live:${id()}`}>
            <ThinkingIndicator tone={g().source === "agent" ? "agent" : "runner"} testID={`activity-live-dots:${id()}`} />
          </Row>
        </If>
        <If when={failed()}>
          <Badge testID={`activity-outcome:${id()}`} tone="danger" label="failed" />
        </If>
        <If when={run()}>
          {(r) => (
            <Action
              testID={`activity-stop:${id()}`}
              label="Stop"
              busyLabel="Stopping…"
              busy={runBusy() === r().id}
              variant="destructive"
              accessibilityLabel={`Stop ${name()}`}
              onPress={() => void stopRun(r())}
            />
          )}
        </If>
      </Row>
      <If when={expanded() ? null : webFailure()}>
        {(f) => <Label variant="caption" color="danger" testID={`activity-web-failure:${id()}`}>
          {webFailureText(f().note, ownWebFailure(f().note), f().offlineOwner)}
        </Label>}
      </If>
      <If when={hasDetails() && expanded()}>
        <Column gap={0} selectable testID={`activity-details:${id()}`}>
          <If when={g().pick}>
            {(p) => (
              <TimelineEntry phase="progress" last={g().steps.length === 0 && !g().route}>
                <Label variant="caption" color="muted" testID={`activity-pick-text:${id()}`}>
                  {p()}
                </Label>
              </TimelineEntry>
            )}
          </If>
          <If when={g().route}>
            {(r) => (
              <TimelineEntry phase="progress" last={g().steps.length === 0}>
                <Label variant="caption" color="muted" testID={`activity-route:${id()}`}>
                  {routeText(r())}
                </Label>
              </TimelineEntry>
            )}
          </If>
          <Each of={timeline(g())} key={(item) => item.key}>
            {(item, index) => <TimelineView groupKey={id()} item={item()} last={index() === timeline(g()).length - 1} />}
          </Each>
        </Column>
      </If>
    </Column>
  );
}

function TimelineView(props: { groupKey: string; item: TimelineItem; last: boolean }) {
  const as = <T extends TimelineItem["type"]>(type: T) => (props.item.type === type ? (props.item as Extract<TimelineItem, { type: T }>) : null);
  return (
    <If when={as("tool")} else={<If when={as("thought")} else={<NoteView groupKey={props.groupKey} step={(props.item as Extract<TimelineItem, { type: "note" }>).step} last={props.last} />}>{(t) => <ThoughtView groupKey={props.groupKey} step={t().step} last={props.last} />}</If>}>
      {(t) => <ToolView groupKey={props.groupKey} item={t()} last={props.last} />}
    </If>
  );
}

/** A tool call: its name and short arguments, then its output (or failure) in a bounded code view. */
function ToolView(props: { groupKey: string; item: Extract<TimelineItem, { type: "tool" }>; last: boolean }) {
  const first = () => (props.item.call ?? props.item.result)!;
  const phase = () => (props.item.failed ? "failed" : props.item.result ? "complete" : "called");
  const stateKey = (s: Step) => disclosureKey(disclosureScope(), props.groupKey, `all:${s.eventId}`);
  return (
    <TimelineEntry
      phase={phase()}
      iconTestID={`activity-step-icon:${first().eventId}:${phase()}`}
      last={props.last}
      testID={`activity-step:${first().eventId}`}
    >
      <If when={first().activity.web} else={<>
      <Row align="center" gap={2} wrap>
        <Label variant="strong">{props.item.name}</Label>
        <If when={props.item.args}>
          {(a) => (
            <Label variant="mono" color="muted" lines={1} testID={`activity-args:${first().eventId}`}>
              {a()}
            </Label>
          )}
        </If>
        <If when={props.item.failed}>
          <Label variant="caption" color="danger">
            failed
          </Label>
        </If>
      </Row>
      <If when={props.item.result && props.item.call ? props.item.result : null}>
        {(r) => (
          <Column testID={`activity-step:${r().eventId}`}>
            <If when={r().activity.details.length > 0}>
              <StepOutput text={r().activity.details} stateKey={stateKey(r())} testID={`activity-output:${r().eventId}`} />
            </If>
          </Column>
        )}
      </If>
      <If when={!props.item.call && props.item.result && props.item.result.activity.details.length > 0 ? props.item.result : null}>
        {(r) => <StepOutput text={r().activity.details} stateKey={stateKey(r())} testID={`activity-output:${r().eventId}`} />}
      </If>
      </>}>
        <WebToolView item={props.item} groupKey={props.groupKey} />
      </If>
    </TimelineEntry>
  );
}

function WebToolView(props: { item: Extract<TimelineItem, { type: "tool" }>; groupKey: string }) {
  const call = () => props.item.call;
  const result = () => props.item.result;
  const web = () => (result() ?? call())!.activity.web!;
  const provider = () => web().note.provider;
  const args = () => call()?.activity.details ?? "";
  const own = () => ownWebFailure(web().note);
  const foldKey = () => disclosureKey(disclosureScope(), props.groupKey, `web-args:${call()?.eventId}`);
  return (
    <Column gap={1}>
      <Label variant="strong">{props.item.name}{provider() ? ` · ${WEB_PROVIDERS[provider()!]}` : ""}</Label>
      <If when={args()}>
        <Row>
          <DisclosureToggle label={web().note.name === "web_search" ? "Query details" : "Page addresses"}
            testID={`activity-web-args:${call()?.eventId}`} expanded={isOpen(foldKey())} onPress={() => toggle(foldKey())} />
        </Row>
        <If when={isOpen(foldKey())}>
          <Label variant="body" testID={`activity-web-query:${call()?.eventId}`}>{args()}</Label>
        </If>
      </If>
      <If when={result() && !web().note.failure && result()?.activity.details}>
        {(count) => <Label variant="caption" testID={`activity-web-count:${result()?.eventId}`}>{count()}</Label>}
      </If>
      <Each of={webAttemptRows(web().note)} key={(a) => a.index}>
        {(a) => <Label variant="caption" testID={a().switched ? "activity-websearch-rotated" : `activity-web-attempt:${a().index}`}>
          {a().text}
        </Label>}
      </Each>
      <If when={result() && web().note.failure}>
        <Label variant="caption" color="danger" testID={`activity-web-failure:${result()?.eventId}`}>
          {webFailureText(web().note, own(), web().offlineOwner)}
        </Label>
        <If when={own() && web().note.failure === "exhausted" &&
          web().note.attempts.some((a) => a.mode === "keyless" && (a.outcome === "limited" || a.outcome === "out_of_credits"))}>
          <Label variant="caption" color="muted">Adding a key may give you higher limits.</Label>
        </If>
        <If when={own() && (web().note.failure === "exhausted" || web().note.failure === "no_device")}>
          <If when={webSearchSettingsAction()}>
            {(action) => <Row><Action label="Open Web search" variant="ghost" testID="activity-open-websearch" onPress={() => { if (own()) action().open(); }} /></Row>}
          </If>
        </If>
      </If>
    </Column>
  );
}

/** A reasoning summary the model provider shared: readable text, never a code box. */
function ThoughtView(props: { groupKey: string; step: Step; last: boolean }) {
  return (
    <TimelineEntry phase="thinking" iconTestID={`activity-step-icon:${props.step.eventId}:thinking`} last={props.last} testID={`activity-step:${props.step.eventId}`}>
      <Label variant="caption" color="muted">
        Reasoning summary from the model provider
      </Label>
      <If when={props.step.activity.details.length > 0}>
        <FoldableText text={props.step.activity.details} mono={false} stateKey={disclosureKey(disclosureScope(), props.groupKey, `all:${props.step.eventId}`)} />
      </If>
    </TimelineEntry>
  );
}

/** Progress and lifecycle notes; a failure says one sentence and folds the rest under Details. */
function NoteView(props: { groupKey: string; step: Step; last: boolean }) {
  const a = () => props.step.activity;
  const phase = () => stepPhase(a());
  const stateKey = () => disclosureKey(disclosureScope(), props.groupKey, `all:${props.step.eventId}`);
  return (
    <TimelineEntry phase={phase()} iconTestID={`activity-step-icon:${props.step.eventId}:${phase()}`} last={props.last} testID={`activity-step:${props.step.eventId}`}>
      <If
        when={a().tone === "danger"}
        else={
          <>
            <Label variant="caption" color="muted">
              {a().summary}
            </Label>
            <If when={a().details.length > 0}>
              <If when={a().mono} else={<FoldableText text={a().details} mono={false} stateKey={stateKey()} />}>
                <StepOutput text={a().details} stateKey={stateKey()} testID={`activity-output:${props.step.eventId}`} />
              </If>
            </If>
          </>
        }
      >
        <FailureBody {...failureText(a().details || a().summary)} stateKey={disclosureKey(disclosureScope(), props.groupKey, `details:${props.step.eventId}`)} testID={`activity-failure:${props.step.eventId}`} />
      </If>
    </TimelineEntry>
  );
}

/** A failure: one plain sentence; the technical text, when there is more, under a "Details" fold. */
export function FailureBody(props: { sentence: string; details: string | null; stateKey: string; testID?: string }) {
  const open = () => isOpen(props.stateKey);
  return (
    <Column gap={1} testID={props.testID}>
      <Label color="danger">{props.sentence}</Label>
      <If when={props.details}>
        {(details) => (
          <Column gap={1}>
            <Row>
              <DisclosureToggle
                label="Details"
                expanded={open()}
                accessibilityLabel={`${open() ? "Hide" : "Show"} the technical details`}
                testID={props.testID ? `${props.testID}:details` : undefined}
                onPress={() => toggle(props.stateKey)}
              />
            </Row>
            <If when={open()}>
              <StepOutput text={details()} stateKey={`${props.stateKey}:all`} testID={props.testID ? `${props.testID}:text` : "failure-details"} />
            </If>
          </Column>
        )}
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
