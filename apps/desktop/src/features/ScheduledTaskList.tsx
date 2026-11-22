// The list of the user's scheduled tasks: one card each, with when it fires
// next, where it goes and its prompt on one line; Edit, and a More menu for
// History, Pause or Resume and Delete…. A task's history says "dispatched"
// (the agent got it) apart from work that finished, which only the
// conversation shows, and explains each status once.

import { spaces } from "../store";
import { Action, ActionsMenu, Badge, Banner, Column, Divider, Icon, Label, Row, SettingRow, SettingsGroup, Surface } from "../ui";
import type { ActionsMenuItem } from "../ui";
import { destinationText, nextFiring, runDetail, runStatus, scheduleText, shortInstant, taskBadge, taskState } from "./discovery/present";
import type { SpaceLookup } from "./discovery/present";
import {
  askDelete,
  cancelDelete,
  confirmingDelete,
  deleteTask,
  deviceZone,
  histories,
  historyOpen,
  loadHistory,
  MAX_TASKS,
  nowMs,
  openEditTask,
  roomsFor,
  scheduleBusy,
  setEnabled,
  tasks,
  tasksError,
  toggleHistory,
} from "./discovery/scheduleState";
import type { ScheduledTask, ScheduleRun } from "./discovery/scheduleState";
import { parseStamp } from "./discovery/time";

const lookup: SpaceLookup = {
  spaceName: (sid) => spaces().find((s) => s.id === sid)?.name ?? null,
  roomName: (sid, rid) => roomsFor(sid)?.find((r) => r.id === rid)?.name ?? null,
};

export function TaskList() {
  return (
    <SettingsGroup title="Your tasks" description={`Times come from the server. Up to ${MAX_TASKS} tasks; each keeps its last 100 firings.`} testID="schedule-list">
      <If when={tasks()} else={<SettingRow label={tasksError() ? "Couldn't load your tasks" : "Loading scheduled tasks…"} />}>
        {(list) => (
          <Column>
            <If when={list().length === 0}>
              <SettingRow label="No scheduled tasks yet" description="New task sends a prompt once, or every day, on weekdays, weekly or on a cron schedule." />
            </If>
            <Each of={list()} key={(t) => t.id}>
              {(t) => <TaskCard task={t()} />}
            </Each>
          </Column>
        )}
      </If>
    </SettingsGroup>
  );
}

function stampText(stamp: string): string {
  const parsed = parseStamp(stamp);
  return parsed ? shortInstant(parsed.instantMs, deviceZone(), nowMs()) : stamp;
}

function TaskCard(props: { task: ScheduledTask }) {
  const t = () => props.task;
  const busy = () => scheduleBusy();
  const dest = () => destinationText(t().target, lookup);
  const next = () => nextFiring(t(), deviceZone(), nowMs());
  const open = () => historyOpen() === t().id;
  // History, Pause or Resume, and Delete… (which asks first) fold into "More".
  const menu = (): ActionsMenuItem[] => {
    const task = t();
    const id = task.id;
    const items: ActionsMenuItem[] = [{ id: `schedule-history:${id}`, label: open() ? "Hide history" : "History", onPress: () => void toggleHistory(id) }];
    const state = taskState(task);
    if (state === "active") items.push({ id: `schedule-pause:${id}`, label: "Pause", disabled: !!busy(), onPress: () => void setEnabled(task, false) });
    if (state === "paused") items.push({ id: `schedule-resume:${id}`, label: "Resume", disabled: !!busy(), onPress: () => void setEnabled(task, true) });
    items.push({ id: `schedule-delete:${id}`, label: "Delete…", destructive: true, disabled: !!busy(), onPress: () => askDelete(id) });
    return items;
  };
  const busyText = () => {
    const b = busy();
    const id = t().id;
    return b === `pause:${id}` ? "Pausing…" : b === `resume:${id}` ? "Resuming…" : null;
  };
  return (
    <Column testID={`schedule:${t().id}`}>
      <Divider />
      <Column padX={4} padY={3} gap={2}>
        <Row align="start" gap={3}>
          <Column grow>
            <Label variant="heading" lines={1}>
              {t().name}
            </Label>
            <Label variant="caption" color="muted" testID={`schedule-summary:${t().id}`}>
              {scheduleText(t(), deviceZone(), nowMs())}
            </Label>
          </Column>
          <Badge testID={`schedule-state:${t().id}`} tone={taskBadge(t()).tone} label={taskBadge(t()).label} />
        </Row>
        <Column gap={1}>
          <If when={next()}>
            {(n) => (
              <Row gap={2} align="center">
                <Icon name="clock" size={14} />
                <Label
                  variant="caption"
                  testID={`schedule-next:${t().id}`}
                  accessibilityLabel={n().exact.length ? `${n().text} Exactly: ${n().exact.join("; ")}.` : n().text}
                >
                  {n().text}
                </Label>
              </Row>
            )}
          </If>
          <Row gap={2} align="center">
            <Icon name={t().target.kind === "chat" ? "chat" : "hash"} size={14} />
            <Label variant="caption" color={dest().lost ? "danger" : "muted"} testID={`schedule-destination:${t().id}`} accessibilityLabel={`Sends to the agent in ${dest().text}`}>
              {dest().text}
            </Label>
          </Row>
          <Label variant="caption" color="muted" lines={1} testID={`schedule-prompt:${t().id}`} accessibilityLabel={`Prompt: ${t().prompt}`}>
            {oneLine(t().prompt)}
          </Label>
          <If when={!open() && t().lastStatus}>
            {(status) => <LastFiring task={t()} status={status() as string} />}
          </If>
        </Column>
        <Row gap={2} justify="end" align="center">
          <If when={busyText()}>
            {(text) => (
              <Label variant="meta" testID={`schedule-busy:${t().id}`}>
                {text()}
              </Label>
            )}
          </If>
          <Action testID={`schedule-edit:${t().id}`} label="Edit" variant="secondary" compact disabled={!!busy()} onPress={() => openEditTask(t())} />
          <ActionsMenu testID={`schedule-more:${t().id}`} label="More" accessibilityLabel={`More actions for ${t().name}`} items={menu()} />
        </Row>
      </Column>
      <If when={confirmingDelete() === t().id}>
        <Column padX={4} padY={2} gap={2} testID={`schedule-confirm:${t().id}`}>
          <Banner
            tone="warning"
            variant="inline"
            title={`Delete “${t().name}”?`}
            message="It won't fire again and its history goes too. A prompt the agent already has keeps running."
          />
          <Row gap={2} justify="end">
            <Action testID="schedule-cancel-delete" label="Cancel" variant="ghost" onPress={cancelDelete} />
            <Action
              testID={`schedule-confirm-delete:${t().id}`}
              label="Delete task"
              busyLabel="Deleting…"
              busy={busy() === `delete:${t().id}`}
              variant="danger"
              onPress={() => void deleteTask(t())}
            />
          </Row>
        </Column>
      </If>
      <If when={open()}>
        <Column padX={4} padY={2} gap={3}>
          <If when={next() && next()!.exact.length > 0}>
            <Column gap={1} testID={`schedule-exact:${t().id}`}>
              <Label variant="strong">Next firing, exactly</Label>
              <Each of={next()!.exact} key={(line) => line}>
                {(line) => (
                  <Label variant="caption" color="muted">
                    {line()}
                  </Label>
                )}
              </Each>
            </Column>
          </If>
          <History id={t().id} />
        </Column>
      </If>
    </Column>
  );
}

/** The prompt as one line: line breaks become spaces (the label truncates the rest). */
function oneLine(text: string): string {
  return text.replace(/\s+/g, " ").trim();
}

/** The latest firing on the card: when, its status, and a reason of its own if it has one. Hidden while the history (which lists it) is open. */
function LastFiring(props: { task: ScheduledTask; status: string }) {
  const s = () => runStatus(props.status);
  const detail = () => runDetail(props.status, props.task.lastDetail);
  return (
    <Column gap={1} testID={`schedule-last:${props.task.id}`}>
      <Row gap={2} align="center" wrap>
        <Label variant="caption" color="muted">
          {props.task.lastRunAt ? `Last firing ${stampText(props.task.lastRunAt)}` : "Last firing"}
        </Label>
        <Badge tone={s().tone} label={s().label} />
      </Row>
      <If when={detail()}>
        {(d) => (
          <Label variant="caption" color={s().tone === "danger" ? "danger" : "muted"} lines={2}>
            {d()}
          </Label>
        )}
      </If>
    </Column>
  );
}

function History(props: { id: string }) {
  const entry = () => histories()[props.id];
  // Each status that appears, explained once above the list.
  const legend = () => {
    const value = entry();
    if (!value || "error" in value) return [];
    const seen = [...new Set(value.map((r) => r.status))];
    return seen.map((status) => ({ status, ...runStatus(status) })).filter((s) => s.explain);
  };
  return (
    <Column gap={2} testID={`schedule-runs:${props.id}`}>
      <Row align="center" gap={2} justify="between">
        <Label variant="strong">History</Label>
        <Action testID={`schedule-history-refresh:${props.id}`} label="Refresh" variant="ghost" onPress={() => void loadHistory(props.id)} />
      </Row>
      <If when={legend().length > 0}>
        <Column gap={1} testID={`schedule-legend:${props.id}`}>
          <Each of={legend()} key={(s) => s.status}>
            {(s) => (
              <Label variant="caption" color="muted">
                {`${s().label}: ${s().explain}`}
              </Label>
            )}
          </Each>
        </Column>
      </If>
      <If when={entry()} else={<Label variant="caption" color="muted">Loading history…</Label>}>
        {(e) => {
          const value = e();
          if ("error" in value) {
            return (
              <Label variant="caption" color="danger" testID={`schedule-runs-error:${props.id}`}>
                {value.error}
              </Label>
            );
          }
          return (
            <Column gap={2}>
              <If when={value.length === 0}>
                <Label variant="caption" color="muted" testID={`schedule-runs-empty:${props.id}`}>
                  It hasn't fired yet.
                </Label>
              </If>
              <Each of={value} key={(r) => r.id}>
                {(r) => <RunRow taskId={props.id} run={r()} />}
              </Each>
            </Column>
          );
        }}
      </If>
    </Column>
  );
}

function RunRow(props: { taskId: string; run: ScheduleRun }) {
  const s = () => runStatus(props.run.status);
  const detail = () => runDetail(props.run.status, props.run.detail);
  return (
    <Surface variant="card" pad={3} gap={1} testID={`schedule-run:${props.taskId}:${props.run.id}`}>
      <Row align="center" gap={2} wrap>
        <Badge tone={s().tone} label={s().label} />
        <Label variant="caption">{`Due ${stampText(props.run.dueAt)}`}</Label>
      </Row>
      <If when={detail()}>
        {(d) => (
          <Label variant="caption" color={s().tone === "danger" ? "danger" : "muted"}>
            {d()}
          </Label>
        )}
      </If>
    </Surface>
  );
}
