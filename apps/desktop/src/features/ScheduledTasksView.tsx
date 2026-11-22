// Scheduled tasks: prompts the server sends to the agent for the user, once
// or on a recurrence, in their personal Chat or a Space room. Each counts as
// the user asking the agent. The list shows where each one goes and when it
// fires next; its history tells "dispatched" (the agent got it) apart from
// work that finished, which only the conversation shows.

import { effect, untracked } from "nsh";
import { spaces } from "../store";
import { Action, Badge, Banner, Column, Field, Label, PageHeader, Picker, Row, ScrollArea, SegmentedControl, SettingRow, SettingsGroup, Surface, Toggle } from "../ui";
import type { PickerItem } from "../ui";
import { ConnectionBanner } from "./Conversation";
import { destinationText, nextFiringText, runStatus, scheduleText, taskBadge, taskState } from "./discovery/present";
import type { SpaceLookup } from "./discovery/present";
import {
  askDelete,
  atTaskLimit,
  cancelDelete,
  closeForm,
  confirmingDelete,
  deleteTask,
  destinationRooms,
  deviceZone,
  formCron,
  formCronPreview,
  formDestination,
  formEnabled,
  formError,
  formName,
  formOnceDate,
  formOnceTime,
  formOnceZone,
  formOpen,
  formPrompt,
  formRecurTime,
  formRecurrence,
  formTimezone,
  formWeekday,
  formWhen,
  histories,
  historyOpen,
  loadHistory,
  loadSchedules,
  MAX_NAME_CHARS,
  MAX_ONCE_AHEAD_DAYS,
  MAX_PROMPT_CHARS,
  MAX_TASKS,
  onceZoneName,
  openEditTask,
  openNewTask,
  roomsFor,
  saveForm,
  scheduleBusy,
  scheduleNotice,
  setDestinationChat,
  setDestinationRoom,
  setDestinationSpace,
  setEnabled,
  tasks,
  tasksError,
  toggleHistory,
} from "./discovery/scheduleState";
import type { RecurrenceKind, ScheduledTask, ScheduleRun } from "./discovery/scheduleState";
import { describeInstant, onceInstant, parseStamp, WEEKDAY_NAMES } from "./discovery/time";

const lookup: SpaceLookup = {
  spaceName: (sid) => spaces().find((s) => s.id === sid)?.name ?? null,
  roomName: (sid, rid) => roomsFor(sid)?.find((r) => r.id === rid)?.name ?? null,
};

export function ScheduledTasksView() {
  // Opening the page loads the list fresh.
  effect(() => untracked(() => void loadSchedules()));
  return (
    <Surface variant="panel" grow testID="schedules-view">
      <ConnectionBanner />
      <ScrollArea pad={6}>
        <Surface variant="content" gap={6}>
          <PageHeader
            title="Scheduled tasks"
            subtitle="Prompts the Metro server sends to the agent for you, once or on a schedule, in your personal Chat or a Space room. Each one counts as you asking the agent, with your model connection. Only you see your tasks."
          >
            <If when={!formOpen()}>
              <Action testID="schedule-new" label="New task" variant="primary" disabled={atTaskLimit()} onPress={openNewTask} />
            </If>
          </PageHeader>
          <Messages />
          <If when={formOpen()}>
            <TaskForm />
          </If>
          <TaskList />
        </Surface>
      </ScrollArea>
    </Surface>
  );
}

function Messages() {
  return (
    <Column gap={2}>
      <If when={tasksError()}>
        {(message) => (
          <Banner tone="danger" testID="schedules-error">
            <Column grow>
              <Label color="danger">{message()}</Label>
            </Column>
            <Action testID="schedules-reload" label="Reload" onPress={() => void loadSchedules()} />
          </Banner>
        )}
      </If>
      <If when={scheduleNotice()}>
        {(message) => (
          <Label variant="caption" color="muted" testID="schedules-notice">
            {message()}
          </Label>
        )}
      </If>
    </Column>
  );
}

// ------------------------------------------------------------------ the form

function TaskForm() {
  const editing = () => formOpen()?.mode === "edit";
  return (
    <SettingsGroup
      title={editing() ? "Edit task" : "New task"}
      description="The agent gets the prompt at each firing, as if you had asked it there."
      testID="schedule-form"
    >
      <SettingRow label="Name" stacked>
        <Field testID="schedule-name" accessibilityLabel="Task name" value={formName()} onChange={formName.set} placeholder="Morning summary" maxLength={MAX_NAME_CHARS} />
      </SettingRow>
      <SettingRow label="Prompt" description="What the agent is asked each time." stacked>
        <Field
          testID="schedule-prompt"
          accessibilityLabel="Prompt"
          value={formPrompt()}
          onChange={formPrompt.set}
          placeholder="Summarise yesterday's messages and list open questions."
          multiline
          maxLength={MAX_PROMPT_CHARS}
        />
      </SettingRow>
      <DestinationRows />
      <SettingRow label="When">
        <SegmentedControl
          testID="schedule-when"
          accessibilityLabel="When it fires"
          value={formWhen()}
          onChange={formWhen.set}
          options={[
            { value: "once", label: "Once" },
            { value: "recurring", label: "Repeats" },
          ]}
        />
      </SettingRow>
      <If when={formWhen() === "once"} else={<RecurringRows />}>
        <OnceRows />
      </If>
      <SettingRow label="Active" description={formEnabled() ? "It fires on schedule." : "Saved paused: it won't fire until you resume it."}>
        <Toggle testID="schedule-enabled" label="Active" checked={formEnabled()} onChange={formEnabled.set} />
      </SettingRow>
      <If when={formError()}>
        {(message) => (
          <Column padX={4} padY={2}>
            <Banner tone="danger" testID="schedule-form-error">
              <Label color="danger">{message()}</Label>
            </Banner>
          </Column>
        )}
      </If>
      <Row padX={4} padY={3} gap={2} justify="end">
        <Action testID="schedule-cancel" label="Cancel" variant="ghost" onPress={closeForm} />
        <Action
          testID="schedule-save"
          label={editing() ? "Save changes" : "Schedule"}
          busyLabel="Saving…"
          busy={scheduleBusy() === "save"}
          variant="primary"
          onPress={() => void saveForm()}
        />
      </Row>
    </SettingsGroup>
  );
}

function DestinationRows() {
  const items = (): PickerItem[] => [{ value: "chat", label: "Your personal Chat" }, ...spaces().map((s) => ({ value: `space:${s.id}`, label: s.name, group: "A room in a Space you're in" }))];
  const value = () => {
    const d = formDestination();
    return d.kind === "chat" ? "chat" : `space:${d.spaceId}`;
  };
  const onChange = (v: string | null) => {
    if (v?.startsWith("space:")) setDestinationSpace(v.slice("space:".length));
    else setDestinationChat();
  };
  const spaceDest = () => {
    const d = formDestination();
    return d.kind === "room" ? d : null;
  };
  const roomItems = (sid: string): PickerItem[] =>
    (roomsFor(sid) ?? []).map((r) => ({ value: r.id, label: r.parentRoomId ? `› ${r.name} (thread)` : `#${r.name}` }));
  const recipient = () => {
    const d = formDestination();
    if (d.kind === "chat") return "To: agent in your personal Chat";
    const space = spaces().find((s) => s.id === d.spaceId);
    if (!space) return "That Space isn't one you're a member of now. Pick another destination.";
    const room = d.roomId ? roomsFor(d.spaceId)?.find((r) => r.id === d.roomId) : null;
    return room ? `To: agent in ${space.name} › #${room.name}` : `Pick a room in ${space.name}.`;
  };
  return (
    <SettingRow label="Where" description="Your personal Chat, or a room or thread of a Space you're a member of." stacked>
      <Column gap={2}>
        <Row gap={2} wrap>
          <Picker testID="schedule-destination" accessibilityLabel="Where the prompt goes" items={items()} value={value()} onChange={onChange} />
          <If when={spaceDest()}>
            {(d) => {
              const failed = () => {
                const own = destinationRooms()[d().spaceId];
                return own && "error" in own ? own.error : null;
              };
              return (
                <If
                  when={roomsFor(d().spaceId)}
                  else={
                    <Label variant="caption" color={failed() ? "danger" : "muted"} testID="schedule-rooms-state">
                      {failed() ?? "Loading rooms…"}
                    </Label>
                  }
                >
                  <Picker
                    testID="schedule-room"
                    accessibilityLabel="Room or thread"
                    items={roomItems(d().spaceId)}
                    value={d().roomId}
                    onChange={(v) => v && setDestinationRoom(v)}
                    placeholder="Pick a room"
                  />
                </If>
              );
            }}
          </If>
        </Row>
        <Label variant="caption" testID="schedule-recipient">
          {recipient()}
        </Label>
      </Column>
    </SettingRow>
  );
}

function OnceRows() {
  const preview = () => {
    const at = onceInstant(formOnceDate(), formOnceTime(), onceZoneName());
    if (!at.ok) return { ok: false, text: at.error };
    const zone = onceZoneName();
    const utc = describeInstant(at.instantMs, "UTC");
    return { ok: true, text: zone === "UTC" ? `Fires ${utc}.` : `Fires ${describeInstant(at.instantMs, zone)}, which is ${utc}.` };
  };
  return (
    <Column>
      <SettingRow label="Date and time" description={`24-hour clock. In the future, at most ${MAX_ONCE_AHEAD_DAYS} days ahead.`} stacked>
        <Row gap={2} align="center">
          <Field testID="schedule-once-date" accessibilityLabel="Date, YYYY-MM-DD" value={formOnceDate()} onChange={formOnceDate.set} placeholder="YYYY-MM-DD" maxLength={10} />
          <Field testID="schedule-once-time" accessibilityLabel="Time, HH:MM" value={formOnceTime()} onChange={formOnceTime.set} placeholder="HH:MM" maxLength={5} />
        </Row>
      </SettingRow>
      <SettingRow label="Read the time in" description="The server stores the exact moment, in UTC.">
        <SegmentedControl
          testID="schedule-once-zone"
          accessibilityLabel="Time zone of the date and time"
          value={formOnceZone()}
          onChange={formOnceZone.set}
          options={[
            { value: "device", label: deviceZone() === "UTC" ? "This device (UTC)" : `This device (${deviceZone()})`, disabled: deviceZone() === "UTC" },
            { value: "utc", label: "UTC" },
          ]}
        />
      </SettingRow>
      <Column padX={4} padY={2}>
        <Label variant="caption" color={preview().ok ? "default" : "danger"} testID="schedule-once-preview">
          {preview().text}
        </Label>
      </Column>
    </Column>
  );
}

function RecurringRows() {
  const weekdayItems: PickerItem[] = WEEKDAY_NAMES.map((name, i) => ({ value: String(i), label: name }));
  const zone = () => formTimezone().trim();
  return (
    <Column>
      <SettingRow label="Repeats">
        <SegmentedControl<RecurrenceKind>
          testID="schedule-repeat"
          accessibilityLabel="How it repeats"
          value={formRecurrence()}
          onChange={formRecurrence.set}
          options={[
            { value: "daily", label: "Every day" },
            { value: "weekdays", label: "Weekdays" },
            { value: "weekly", label: "Weekly" },
            { value: "custom", label: "Cron" },
          ]}
        />
      </SettingRow>
      <If
        when={formRecurrence() === "custom"}
        else={
          <SettingRow label="At" description="24-hour clock, in the time zone below." stacked>
            <Row gap={2} align="center">
              <Field testID="schedule-repeat-time" accessibilityLabel="Time, HH:MM" value={formRecurTime()} onChange={formRecurTime.set} placeholder="HH:MM" maxLength={5} />
              <If when={formRecurrence() === "weekly"}>
                <Picker testID="schedule-weekday" accessibilityLabel="Day of the week" items={weekdayItems} value={String(formWeekday())} onChange={(v) => v !== null && formWeekday.set(Number(v))} />
              </If>
            </Row>
          </SettingRow>
        }
      >
        <SettingRow
          label="Cron expression"
          description="Five fields: minute hour day-of-month month day-of-week, such as 0 9 * * * (every day at 09:00) or 30 8 * * 1-5 (08:30 on weekdays). Names like mon and jan work. It may fire at most every 15 minutes."
          stacked
        >
          <Field testID="schedule-cron" accessibilityLabel="Cron expression" value={formCron()} onChange={formCron.set} placeholder="0 9 * * 1-5" maxLength={100} />
        </SettingRow>
      </If>
      <SettingRow label="Time zone" description={`An IANA name, such as Asia/Manila. This device: ${deviceZone()}.`} stacked>
        <Row gap={2} align="center" wrap>
          <Field testID="schedule-timezone" accessibilityLabel="Time zone (IANA name)" value={formTimezone()} onChange={formTimezone.set} placeholder="Asia/Manila" maxLength={64} />
          <Action testID="schedule-timezone-device" label="This device's" variant="ghost" disabled={zone() === deviceZone()} onPress={() => formTimezone.set(deviceZone())} />
          <Action testID="schedule-timezone-utc" label="UTC" variant="ghost" disabled={zone() === "UTC"} onPress={() => formTimezone.set("UTC")} />
        </Row>
      </SettingRow>
      <Column padX={4} padY={2}>
        <Label variant="caption" color={formCronPreview() ? "muted" : "danger"} testID="schedule-cron-preview">
          {formCronPreview()
            ? `Sends cron “${formCronPreview()}” in ${zone() || "(no time zone)"}. The server works out the next firing when you save. On a daylight-saving change, a skipped time fires at the end of the gap and a repeated time fires once.`
            : "Enter the time as HH:MM, for example 09:00."}
        </Label>
      </Column>
    </Column>
  );
}

// ------------------------------------------------------------------ the list

function TaskList() {
  return (
    <SettingsGroup
      title="Your tasks"
      description={`Next firings come from the server. Paused tasks don't fire. Up to ${MAX_TASKS} tasks; each keeps its last 100 firings.`}
      testID="schedule-list"
    >
      <If when={tasks()} else={<SettingRow label={tasksError() ? "Couldn't load your tasks" : "Loading scheduled tasks…"} />}>
        {(list) => (
          <Column>
            <If when={list().length === 0}>
              <SettingRow label="No scheduled tasks yet" description="New task schedules a prompt once, or every day, on weekdays, weekly or on a cron schedule, for example every day at 09:00 in Asia/Manila." />
            </If>
            <Each of={list()} key={(t) => t.id}>
              {(t) => <TaskRow task={t()} />}
            </Each>
          </Column>
        )}
      </If>
    </SettingsGroup>
  );
}

function TaskRow(props: { task: ScheduledTask }) {
  const t = () => props.task;
  const busy = () => scheduleBusy();
  const dest = () => destinationText(t().target, lookup);
  return (
    <Column testID={`schedule:${t().id}`}>
      <SettingRow label={t().name} description={scheduleText(t())}>
        <Badge testID={`schedule-state:${t().id}`} tone={taskBadge(t()).tone} label={taskBadge(t()).label} />
      </SettingRow>
      <Column padX={4} gap={1}>
        <Label variant="caption" testID={`schedule-next:${t().id}`}>
          {nextFiringText(t(), deviceZone())}
        </Label>
        <Label variant="caption" color={dest().lost ? "danger" : "muted"} testID={`schedule-destination:${t().id}`}>
          {`To: agent in ${dest().text}`}
        </Label>
        <Label variant="caption" color="muted" lines={2}>
          {`Prompt: ${t().prompt}`}
        </Label>
        <If when={t().lastStatus}>
          {(status) => (
            <Row gap={2} align="center" testID={`schedule-last:${t().id}`}>
              <Label variant="caption" color="muted">
                {t().lastRunAt ? `Last firing ${stampText(t().lastRunAt!)}` : "Last firing"}
              </Label>
              <Badge tone={runStatus(status()).tone} label={runStatus(status()).label} />
              <If when={status() !== "dispatched" && t().lastDetail}>
                <Label variant="caption" color={runStatus(status()).tone === "danger" ? "danger" : "muted"} lines={2}>
                  {t().lastDetail ?? ""}
                </Label>
              </If>
            </Row>
          )}
        </If>
      </Column>
      <Row padX={4} padY={2} gap={2} justify="end" wrap>
        <Action
          testID={`schedule-history:${t().id}`}
          label="History"
          expanded={historyOpen() === t().id}
          accessibilityLabel={`History of ${t().name}, ${historyOpen() === t().id ? "shown" : "hidden"}`}
          variant="ghost"
          onPress={() => void toggleHistory(t().id)}
        />
        <Action testID={`schedule-edit:${t().id}`} label="Edit" variant="ghost" disabled={!!busy()} onPress={() => openEditTask(t())} />
        <If when={taskState(t()) === "paused"}>
          <Action
            testID={`schedule-resume:${t().id}`}
            label="Resume"
            busyLabel="Resuming…"
            busy={busy() === `resume:${t().id}`}
            disabled={!!busy()}
            variant="ghost"
            onPress={() => void setEnabled(t(), true)}
          />
        </If>
        <If when={taskState(t()) === "active"}>
          <Action
            testID={`schedule-pause:${t().id}`}
            label="Pause"
            busyLabel="Pausing…"
            busy={busy() === `pause:${t().id}`}
            disabled={!!busy()}
            variant="ghost"
            onPress={() => void setEnabled(t(), false)}
          />
        </If>
        <Action testID={`schedule-delete:${t().id}`} label="Delete…" variant="ghost" disabled={!!busy()} onPress={() => askDelete(t().id)} />
      </Row>
      <If when={confirmingDelete() === t().id}>
        <Column padX={4} padY={2} gap={2} testID={`schedule-confirm:${t().id}`}>
          <Banner tone="warning">
            <Label color="warning">{`Delete “${t().name}”? It won't fire again, and its history goes with it. A prompt already handed to the agent keeps running.`}</Label>
          </Banner>
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
      <If when={historyOpen() === t().id}>
        <History id={t().id} />
      </If>
    </Column>
  );
}

function stampText(stamp: string): string {
  const parsed = parseStamp(stamp);
  return parsed ? describeInstant(parsed.instantMs, deviceZone()) : stamp;
}

function History(props: { id: string }) {
  const entry = () => histories()[props.id];
  return (
    <Column padX={4} padY={2} gap={2} testID={`schedule-runs:${props.id}`}>
      <Row align="center" gap={2}>
        <Label variant="caption" color="muted">
          “Dispatched” means the agent accepted the prompt, not that it succeeded. Its answer or error is in the conversation.
        </Label>
        <Action testID={`schedule-history-refresh:${props.id}`} label="Refresh" variant="ghost" onPress={() => void loadHistory(props.id)} />
      </Row>
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
  return (
    <Surface variant="card" pad={3} gap={1} testID={`schedule-run:${props.taskId}:${props.run.id}`}>
      <Row align="center" gap={2} wrap>
        <Badge tone={s().tone} label={s().label} />
        <Label variant="caption">{`Due ${stampText(props.run.dueAt)}`}</Label>
      </Row>
      <If when={s().explain}>
        <Label variant="caption" color="muted">
          {s().explain}
        </Label>
      </If>
      <If when={props.run.detail}>
        {(d) => (
          <Label variant="caption" color={s().tone === "danger" ? "danger" : "muted"}>
            {d()}
          </Label>
        )}
      </If>
    </Surface>
  );
}
