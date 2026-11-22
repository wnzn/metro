// Scheduled tasks: prompts the server sends to the agent for the user, once
// or on a recurrence, in their personal Chat or a Space room. Each counts as
// the user asking the agent. This file is the page and its form; the list of
// tasks, their cards and history are in ScheduledTaskList.tsx.

import { cleanup, effect, untracked } from "nsh";
import { spaces } from "../store";
import { Action, Banner, Column, Field, Label, PageHeader, Picker, Row, ScrollArea, SegmentedControl, SettingRow, SettingsGroup, Surface, Toggle } from "../ui";
import type { PickerItem } from "../ui";
import { ConnectionBanner } from "./Conversation";
import { TaskList } from "./ScheduledTaskList";
import {
  atTaskLimit,
  chooseZone,
  closeForm,
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
  formZoneChoice,
  loadSchedules,
  MAX_NAME_CHARS,
  MAX_ONCE_AHEAD_DAYS,
  MAX_PROMPT_CHARS,
  onceZoneName,
  openNewTask,
  roomsFor,
  saveForm,
  scheduleBusy,
  scheduleNotice,
  setDestinationChat,
  setDestinationRoom,
  setDestinationSpace,
  tasksError,
  tickNow,
  typeZone,
} from "./discovery/scheduleState";
import type { RecurrenceKind, ZoneChoice } from "./discovery/scheduleState";
import { describeInstant, onceInstant, WEEKDAY_NAMES } from "./discovery/time";

/** How often "Next in …" is brought up to date while the page is shown. */
const TICK_MS = 30_000;

export function ScheduledTasksView() {
  // Opening the page loads the list fresh, and keeps "in 3 h" current.
  effect(() => untracked(() => void loadSchedules()));
  tickNow();
  const timer = setInterval(tickNow, TICK_MS);
  cleanup(() => clearInterval(timer));
  return (
    <Surface variant="panel" grow testID="schedules-view">
      <ConnectionBanner />
      <ScrollArea pad={6}>
        <Surface variant="content" gap={6}>
          <PageHeader
            title="Scheduled tasks"
            subtitle="Prompts the server sends to the agent for you, once or on a schedule. Each counts as you asking, with your model connection. Only you see them."
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
          <Banner
            tone="danger"
            variant="inline"
            testID="schedules-error"
            message={message()}
            action={<Action testID="schedules-reload" label="Reload" variant="secondary" compact onPress={() => void loadSchedules()} />}
          />
        )}
      </If>
      <If when={scheduleNotice()}>{(message) => <Banner tone="success" variant="inline" testID="schedules-notice" message={message()} />}</If>
    </Column>
  );
}

// ------------------------------------------------------------------ the form

function TaskForm() {
  const editing = () => formOpen()?.mode === "edit";
  return (
    <SettingsGroup
      title={editing() ? "Edit task" : "New task"}
      description="The agent gets the prompt at each firing, as if you'd asked it there."
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
      <SettingRow label="Active" description={formEnabled() ? "It fires on schedule." : "Saved paused. It won't fire until you resume it."}>
        <Toggle testID="schedule-enabled" label="Active" checked={formEnabled()} onChange={formEnabled.set} />
      </SettingRow>
      <If when={formError()}>
        {(message) => (
          <Column padX={4} padY={2}>
            <Banner tone="danger" variant="inline" testID="schedule-form-error" message={message()} />
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
    (roomsFor(sid) ?? []).map((r) => ({ value: r.id, label: r.parentRoomId ? `Thread: ${r.name}` : `#${r.name}` }));
  const recipient = () => {
    const d = formDestination();
    if (d.kind === "chat") return "To: the agent in your personal Chat";
    const space = spaces().find((s) => s.id === d.spaceId);
    if (!space) return "You're not a member of that Space now. Pick another destination.";
    const room = d.roomId ? roomsFor(d.spaceId)?.find((r) => r.id === d.roomId) : null;
    return room ? `To: the agent in ${space.name} · #${room.name}` : `Pick a room in ${space.name}.`;
  };
  return (
    <SettingRow label="Where" description="Your personal Chat, or a room or thread of a Space you're in." stacked>
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
        <Label variant="caption" color="muted" testID="schedule-recipient">
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
      <SettingRow label="Time zone" description="The server stores the exact moment, in UTC.">
        <SegmentedControl
          testID="schedule-once-zone"
          accessibilityLabel="Time zone of the date and time"
          value={formOnceZone()}
          onChange={formOnceZone.set}
          options={[
            { value: "device", label: `This device (${deviceZone()})` },
            { value: "utc", label: "UTC" },
          ]}
        />
      </SettingRow>
      <Column padX={4} padY={2}>
        <Label variant="caption" color={preview().ok ? "muted" : "danger"} testID="schedule-once-preview">
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
          description="Minute, hour, day of month, month, weekday: 30 8 * * 1-5 is 08:30 on weekdays. Names like mon work. At most every 15 minutes."
          stacked
        >
          <Field testID="schedule-cron" accessibilityLabel="Cron expression" value={formCron()} onChange={formCron.set} placeholder="0 9 * * 1-5" maxLength={100} />
        </SettingRow>
      </If>
      <SettingRow label="Time zone" description="The time above is read in it, across clock changes." stacked>
        <Column gap={2}>
          <SegmentedControl<ZoneChoice>
            testID="schedule-zone"
            accessibilityLabel="Time zone of the schedule"
            value={formZoneChoice()}
            onChange={chooseZone}
            options={[
              { value: "device", label: `This device (${deviceZone()})` },
              { value: "utc", label: "UTC" },
              { value: "other", label: "Other" },
            ]}
          />
          <Field testID="schedule-timezone" accessibilityLabel="Time zone (IANA name)" value={formTimezone()} onChange={typeZone} placeholder="An IANA name, such as Asia/Manila" maxLength={64} />
        </Column>
      </SettingRow>
      <Column padX={4} padY={2}>
        <Label variant="caption" color={formCronPreview() ? "muted" : "danger"} testID="schedule-cron-preview">
          {formCronPreview()
            ? `Sends cron “${formCronPreview()}” in ${zone() || "(no time zone)"}. The server works out each firing; on a clock change a skipped time fires at the gap's end, a repeated one once.`
            : "Enter the time as HH:MM, for example 09:00."}
        </Label>
      </Column>
    </Column>
  );
}
