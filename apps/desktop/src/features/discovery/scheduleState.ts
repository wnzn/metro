// Scheduled prompts: the user's own tasks, the create/edit form, pause and
// resume, delete with confirmation, and each task's firing history. Only the
// creator ever sees a task (the server answers 404 for anyone else's). All
// of it is dropped when the session ends.

import { batch, computed, localeSettings, signal } from "nsh";
import type { ScheduleInput, ScheduleRun, ScheduleSpec, ScheduleTarget, ScheduledTask } from "../../api/discovery";
import { apiClient, onSessionEnd, roomsOf, sessionGuard, spaces } from "../../store";
import type { Room } from "../../store";
import { discovery, failure, serverDetail, statusOf } from "./access";
import { cronOf, cronShapeError, looksLikeZone, onceInstant, pickDeviceZone, recurrenceOf, todayIn } from "./time";
import type { Recurrence } from "./time";

export type { ScheduleRun, ScheduleTarget, ScheduledTask };

export const MAX_NAME_CHARS = 100;
export const MAX_PROMPT_CHARS = 4000;
/** The server's limits (crates/server/src/schedules.rs). */
export const MAX_TASKS = 50;
export const MAX_ONCE_AHEAD_DAYS = 366;

/** The device's zone (an IANA name this app can format), else UTC. Shown wherever it's used. */
export const deviceZone = computed(() => pickDeviceZone(localeSettings().timeZone));

export const tasks = signal<ScheduledTask[] | null>(null);
export const tasksError = signal<string | null>(null);
export const scheduleBusy = signal<string | null>(null);
export const scheduleNotice = signal<string | null>(null);
export const confirmingDelete = signal<string | null>(null);
export const historyOpen = signal<string | null>(null);
export const histories = signal<Record<string, ScheduleRun[] | { error: string }>>({});

// ------------------------------------------------------------------ the form

export type When = "once" | "recurring";
export type RecurrenceKind = Recurrence["kind"];
export type OnceZone = "device" | "utc";
export type Destination = { kind: "chat" } | { kind: "room"; spaceId: string; roomId: string | null };

export const formOpen = signal<null | { mode: "new" } | { mode: "edit"; id: string }>(null);
export const formName = signal("");
export const formPrompt = signal("");
export const formDestination = signal<Destination>({ kind: "chat" });
export const formWhen = signal<When>("recurring");
export const formOnceDate = signal("");
export const formOnceTime = signal("09:00");
export const formOnceZone = signal<OnceZone>("device");
export const formRecurrence = signal<RecurrenceKind>("daily");
export const formRecurTime = signal("09:00");
export const formWeekday = signal(1);
export const formCron = signal("");
export const formTimezone = signal("");
export const formEnabled = signal(true);
export const formError = signal<string | null>(null);

/** Rooms of Spaces picked as a destination, loaded on demand. */
export const destinationRooms = signal<Record<string, readonly Room[] | { error: string }>>({});

let listSeq = 0;
let roomsSeq = 0;
const historySeq = new Map<string, number>();

export const onceZoneName = () => (formOnceZone() === "utc" ? "UTC" : deviceZone());

export const atTaskLimit = computed(() => (tasks()?.length ?? 0) >= MAX_TASKS);

export function openNewTask() {
  if (atTaskLimit()) {
    tasksError.set(`You have ${MAX_TASKS} scheduled tasks, the most one person can have. Delete one to add another.`);
    return;
  }
  batch(() => {
    resetForm();
    formOpen.set({ mode: "new" });
    scheduleNotice.set(null);
  });
}

export function openEditTask(task: ScheduledTask) {
  batch(() => {
    resetForm();
    formOpen.set({ mode: "edit", id: task.id });
    formName.set(task.name);
    formPrompt.set(task.prompt);
    formEnabled.set(task.enabled);
    formDestination.set(task.target.kind === "chat" ? { kind: "chat" } : { kind: "room", spaceId: task.target.spaceId, roomId: task.target.roomId });
    if (task.schedule.kind === "once") {
      formWhen.set("once");
      // Shown in UTC: exactly what the server stored. The user may switch to the device's zone.
      formOnceZone.set("utc");
      const at = task.schedule.at;
      const m = /^(\d{4}-\d{2}-\d{2})T(\d{2}:\d{2})/.exec(at);
      if (m && /Z$|\+00:00$/.test(at)) {
        formOnceDate.set(m[1]);
        formOnceTime.set(m[2]);
      }
    } else {
      formWhen.set("recurring");
      formTimezone.set(task.schedule.timezone);
      const r = recurrenceOf(task.schedule.expression);
      formRecurrence.set(r.kind);
      formCron.set(task.schedule.expression);
      if (r.kind !== "custom") formRecurTime.set(`${String(r.hour).padStart(2, "0")}:${String(r.minute).padStart(2, "0")}`);
      if (r.kind === "weekly") formWeekday.set(r.weekday);
    }
    scheduleNotice.set(null);
  });
  if (task.target.kind === "room") void loadDestinationRooms(task.target.spaceId);
}

export function closeForm() {
  batch(() => {
    formOpen.set(null);
    formError.set(null);
  });
}

function resetForm() {
  formName.set("");
  formPrompt.set("");
  formDestination.set({ kind: "chat" });
  formWhen.set("recurring");
  formOnceDate.set(todayIn(Date.now(), deviceZone()));
  formOnceTime.set("09:00");
  formOnceZone.set("device");
  formRecurrence.set("daily");
  formRecurTime.set("09:00");
  formWeekday.set(1);
  formCron.set("0 9 * * *");
  formTimezone.set(deviceZone());
  formEnabled.set(true);
  formError.set(null);
}

export function setDestinationChat() {
  formDestination.set({ kind: "chat" });
}

export function setDestinationSpace(spaceId: string) {
  if (!spaces().some((s) => s.id === spaceId)) return;
  const cached = roomsFor(spaceId);
  const lobby = cached ? (cached.find((r) => r.id === "lobby") ?? cached[0])?.id ?? null : null;
  formDestination.set({ kind: "room", spaceId, roomId: lobby });
  if (!cached) void loadDestinationRooms(spaceId);
}

export function setDestinationRoom(roomId: string) {
  const d = formDestination();
  if (d.kind === "room") formDestination.set({ ...d, roomId });
}

/** The rooms (and threads) of a Space, from the store's cache or this form's own load. */
export function roomsFor(spaceId: string): readonly Room[] | null {
  const own = destinationRooms()[spaceId];
  if (own && !("error" in own)) return own;
  const cached = roomsOf(spaceId);
  return cached.length ? cached : null;
}

async function loadDestinationRooms(spaceId: string): Promise<void> {
  const c = apiClient();
  if (!c) return;
  const ok = sessionGuard(spaceId);
  const mine = ++roomsSeq;
  try {
    const list = await c.listRooms(spaceId);
    if (!ok()) return;
    destinationRooms.set({ ...destinationRooms(), [spaceId]: list });
    const d = formDestination();
    if (mine === roomsSeq && d.kind === "room" && d.spaceId === spaceId && d.roomId === null) {
      formDestination.set({ ...d, roomId: (list.find((r) => r.id === "lobby") ?? list[0])?.id ?? null });
    }
  } catch (error) {
    if (!ok()) return;
    const message = statusOf(error) === 403 || statusOf(error) === 404 ? "You're not a member of that Space (any more)." : failure(error, "Couldn't load its rooms");
    if (message) destinationRooms.set({ ...destinationRooms(), [spaceId]: { error: message } });
  }
}

/** The form as the server's input, or why it can't be saved yet. */
export function formInput(nowMs = Date.now()): { ok: true; input: ScheduleInput } | { ok: false; error: string } {
  const name = formName().trim();
  if (!name) return { ok: false, error: "Give the task a name." };
  if (name.length > MAX_NAME_CHARS) return { ok: false, error: `Names are at most ${MAX_NAME_CHARS} characters.` };
  const prompt = formPrompt().trim();
  if (!prompt) return { ok: false, error: "Write the prompt the agent should get." };
  if (prompt.length > MAX_PROMPT_CHARS) return { ok: false, error: `Prompts are at most ${MAX_PROMPT_CHARS} characters.` };

  const d = formDestination();
  let target: ScheduleTarget;
  if (d.kind === "chat") target = { kind: "chat" };
  else {
    if (!spaces().some((s) => s.id === d.spaceId)) return { ok: false, error: "You're not a member of that Space (any more). Pick another destination." };
    if (!d.roomId) return { ok: false, error: "Pick the room or thread the prompt goes to." };
    target = { kind: "room", spaceId: d.spaceId, roomId: d.roomId };
  }

  let schedule: ScheduleSpec;
  if (formWhen() === "once") {
    const at = onceInstant(formOnceDate(), formOnceTime(), onceZoneName());
    if (!at.ok) return { ok: false, error: at.error };
    if (at.instantMs <= nowMs) return { ok: false, error: "That time has already passed. Pick a time in the future." };
    if (at.instantMs > nowMs + MAX_ONCE_AHEAD_DAYS * 86_400_000) return { ok: false, error: `A one-time task can be at most ${MAX_ONCE_AHEAD_DAYS} days ahead.` };
    schedule = { kind: "once", at: at.at };
  } else {
    const zone = formTimezone().trim();
    if (!looksLikeZone(zone)) return { ok: false, error: "Enter an IANA time zone, such as Asia/Manila, Europe/Berlin or UTC." };
    let expression: string;
    if (formRecurrence() === "custom") {
      const shape = cronShapeError(formCron());
      if (shape) return { ok: false, error: shape };
      expression = cronOf({ kind: "custom", expression: formCron() });
    } else {
      const m = /^(\d{1,2}):(\d{2})$/.exec(formRecurTime().trim());
      const hour = m ? Number(m[1]) : NaN;
      const minute = m ? Number(m[2]) : NaN;
      if (!m || hour > 23 || minute > 59) return { ok: false, error: "Enter the time as HH:MM on a 24-hour clock, for example 09:00." };
      const kind = formRecurrence();
      expression = cronOf(kind === "weekly" ? { kind, weekday: formWeekday(), hour, minute } : { kind: kind as "daily" | "weekdays", hour, minute });
    }
    schedule = { kind: "cron", expression, timezone: zone };
  }
  return { ok: true, input: { name, prompt, target, schedule, enabled: formEnabled() } };
}

/** The cron expression the form would send now (shown under the simple choices). */
export const formCronPreview = computed(() => {
  if (formRecurrence() === "custom") return formCron().trim();
  const m = /^(\d{1,2}):(\d{2})$/.exec(formRecurTime().trim());
  if (!m || Number(m[1]) > 23 || Number(m[2]) > 59) return null;
  const hour = Number(m[1]);
  const minute = Number(m[2]);
  const kind = formRecurrence();
  return cronOf(kind === "weekly" ? { kind, weekday: formWeekday(), hour, minute } : { kind: kind as "daily" | "weekdays", hour, minute });
});

// ------------------------------------------------------------------ calls

export async function loadSchedules(): Promise<void> {
  const ok = sessionGuard();
  const mine = ++listSeq;
  try {
    const api = discovery();
    if (!api) return;
    const list = await api.listSchedules();
    if (!ok() || mine !== listSeq) return;
    batch(() => {
      tasks.set(list);
      tasksError.set(null);
    });
    // Room names for the destinations, in Spaces the user is still in.
    const joined = new Set(spaces().map((s) => s.id));
    const wanted = new Set(list.flatMap((t) => (t.target.kind === "room" && joined.has(t.target.spaceId) ? [t.target.spaceId] : [])));
    for (const sid of wanted) if (!roomsFor(sid)) void loadDestinationRooms(sid);
  } catch (error) {
    if (!ok() || mine !== listSeq) return;
    const message = failure(error, "Couldn't load scheduled tasks");
    if (message) tasksError.set(message);
  }
}

function replaceTask(task: ScheduledTask) {
  const list = tasks() ?? [];
  tasks.set(list.some((t) => t.id === task.id) ? list.map((t) => (t.id === task.id ? task : t)) : [...list, task]);
}

function dropTask(id: string) {
  tasks.set((tasks() ?? []).filter((t) => t.id !== id));
  const { [id]: _gone, ...rest } = histories();
  histories.set(rest);
  if (historyOpen() === id) historyOpen.set(null);
  if (confirmingDelete() === id) confirmingDelete.set(null);
}

/** 400s carry the server's reason (bad cron, unknown zone, a time in the past): show it as it is. */
function saveError(error: unknown, what: string): string | null {
  const status = statusOf(error);
  const detail = serverDetail(error);
  if ((status === 400 || status === 409 || status === 422) && detail) return `${what}: ${detail}.`;
  return failure(error, what);
}

export async function saveForm(): Promise<boolean> {
  const mode = formOpen();
  if (!mode || scheduleBusy()) return false;
  const parsed = formInput();
  if (!parsed.ok) {
    formError.set(parsed.error);
    return false;
  }
  const ok = sessionGuard();
  batch(() => {
    scheduleBusy.set("save");
    formError.set(null);
  });
  try {
    const api = discovery();
    if (!api) return false;
    const saved = mode.mode === "new" ? await api.createSchedule(parsed.input) : await api.updateSchedule(mode.id, parsed.input);
    if (!ok()) return false;
    batch(() => {
      replaceTask(saved);
      formOpen.set(null);
      scheduleNotice.set(mode.mode === "new" ? `Scheduled “${saved.name}”.` : `Saved “${saved.name}”.`);
    });
    return true;
  } catch (error) {
    if (!ok()) return false;
    if (mode.mode === "edit" && statusOf(error) === 404) {
      batch(() => {
        dropTask(mode.id);
        formOpen.set(null);
        tasksError.set("That task no longer exists.");
      });
      return false;
    }
    const message = saveError(error, mode.mode === "new" ? "Couldn't schedule it" : "Couldn't save it");
    if (message) formError.set(message);
    return false;
  } finally {
    if (ok()) scheduleBusy.set(null);
  }
}

export async function setEnabled(task: ScheduledTask, enabled: boolean): Promise<void> {
  if (scheduleBusy()) return;
  const ok = sessionGuard();
  batch(() => {
    scheduleBusy.set(`${enabled ? "resume" : "pause"}:${task.id}`);
    tasksError.set(null);
    scheduleNotice.set(null);
  });
  try {
    const api = discovery();
    if (!api) return;
    const saved = await api.updateSchedule(task.id, { enabled });
    if (!ok()) return;
    batch(() => {
      replaceTask(saved);
      scheduleNotice.set(enabled ? `Resumed “${saved.name}”.` : `Paused “${saved.name}”. It won't fire until you resume it.`);
    });
  } catch (error) {
    if (!ok()) return;
    if (statusOf(error) === 404) {
      dropTask(task.id);
      tasksError.set("That task no longer exists.");
      return;
    }
    const message = saveError(error, enabled ? "Couldn't resume it" : "Couldn't pause it");
    if (message) tasksError.set(message);
  } finally {
    if (ok()) scheduleBusy.set(null);
  }
}

export function askDelete(id: string) {
  confirmingDelete.set(id);
}

export function cancelDelete() {
  confirmingDelete.set(null);
}

export async function deleteTask(task: ScheduledTask): Promise<void> {
  if (scheduleBusy()) return;
  const ok = sessionGuard();
  batch(() => {
    scheduleBusy.set(`delete:${task.id}`);
    tasksError.set(null);
    scheduleNotice.set(null);
  });
  try {
    const api = discovery();
    if (!api) return;
    await api.deleteSchedule(task.id);
    if (!ok()) return;
    batch(() => {
      dropTask(task.id);
      if (formOpen()?.mode === "edit" && (formOpen() as { id: string }).id === task.id) formOpen.set(null);
      scheduleNotice.set(`Deleted “${task.name}”. It won't fire again.`);
    });
  } catch (error) {
    if (!ok()) return;
    if (statusOf(error) === 404) {
      dropTask(task.id);
      return;
    }
    const message = failure(error, "Couldn't delete it");
    if (message) tasksError.set(message);
  } finally {
    if (ok()) scheduleBusy.set(null);
  }
}

export async function toggleHistory(id: string): Promise<void> {
  if (historyOpen() === id) {
    historyOpen.set(null);
    return;
  }
  historyOpen.set(id);
  await loadHistory(id);
}

export async function loadHistory(id: string): Promise<void> {
  const ok = sessionGuard();
  const mine = (historySeq.get(id) ?? 0) + 1;
  historySeq.set(id, mine);
  try {
    const api = discovery();
    if (!api) return;
    const runs = await api.scheduleHistory(id);
    if (!ok() || historySeq.get(id) !== mine) return;
    histories.set({ ...histories(), [id]: runs });
  } catch (error) {
    if (!ok() || historySeq.get(id) !== mine) return;
    if (statusOf(error) === 404) {
      dropTask(id);
      tasksError.set("That task no longer exists.");
      return;
    }
    const message = failure(error, "Couldn't load its history");
    if (message) histories.set({ ...histories(), [id]: { error: message } });
  }
}

onSessionEnd(() => {
  listSeq++;
  roomsSeq++;
  historySeq.clear();
  batch(() => {
    tasks.set(null);
    tasksError.set(null);
    scheduleBusy.set(null);
    scheduleNotice.set(null);
    confirmingDelete.set(null);
    historyOpen.set(null);
    histories.set({});
    destinationRooms.set({});
    formOpen.set(null);
    resetForm();
  });
});
