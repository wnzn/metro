// What scheduled tasks and search answers say, in words. Pure functions of
// their inputs (plus the Spaces the user belongs to), no UI.

import type { SearchRanking, ScheduleTarget, ScheduledTask } from "../../api/discovery";
import type { Tone } from "../../ui";
import { cronOf, describeInstant, describeStamp, formatWallShort, parseStamp, recurrenceOf, relativeTime, wallIn, WEEKDAY_NAMES, zoneFormattable } from "./time";
import type { Recurrence } from "./time";

export type SpaceLookup = { spaceName: (spaceId: string) => string | null; roomName: (spaceId: string, roomId: string) => string | null };

/** "Your personal Chat", "Acme · #lobby", or why the destination is no longer reachable. */
export function destinationText(target: ScheduleTarget, lookup: SpaceLookup): { text: string; lost: boolean } {
  if (target.kind === "chat") return { text: "Your personal Chat", lost: false };
  const space = lookup.spaceName(target.spaceId);
  if (!space) return { text: "A Space you're no longer in. Its next firing will be refused and the task paused.", lost: true };
  const room = lookup.roomName(target.spaceId, target.roomId) ?? target.roomId;
  return { text: `${space} · #${room}`, lost: false };
}

/** "Every day", "Weekdays", "Fridays", "Cron 30 7 1 * *": the repeat without its time. */
function repeatWords(r: Recurrence): string {
  switch (r.kind) {
    case "daily":
      return "Every day";
    case "weekdays":
      return "Weekdays";
    case "weekly":
      return `${WEEKDAY_NAMES[r.weekday]}s`;
    case "custom":
      return `Cron ${cronOf(r)}`;
  }
}

const hm = (h: number, m: number) => `${String(h).padStart(2, "0")}:${String(m).padStart(2, "0")}`;

/**
 * A compact summary of when a task fires: "Every day at 09:00 · Asia/Manila",
 * "Fridays at 17:00 · UTC", "Cron 30 7 1 * * · UTC", "Once · Sun, Nov 22 · 18:00 UTC".
 * A one-time task is shown in the device's zone (the server stores UTC).
 */
export function scheduleText(task: ScheduledTask, deviceZone = "UTC", nowMs = Date.now()): string {
  if (task.schedule.kind === "once") {
    const at = parseStamp(task.schedule.at);
    return at ? `Once · ${shortInstant(at.instantMs, deviceZone, nowMs)}` : `Once · ${task.schedule.at}`;
  }
  const r = recurrenceOf(task.schedule.expression);
  const when = r.kind === "custom" ? repeatWords(r) : `${repeatWords(r)} at ${hm(r.hour, r.minute)}`;
  return `${when} · ${task.schedule.timezone}`;
}

/** "Mon, Nov 23 · 09:00 Asia/Manila" (the year only when it isn't this year). */
export function shortInstant(instantMs: number, zone: string, nowMs: number): string {
  const z = zone === "UTC" || zoneFormattable(zone) ? zone : "UTC";
  return `${formatWallShort(wallIn(instantMs, z), wallIn(nowMs, z).year)} ${z}`;
}

export type TaskState = "active" | "paused" | "finished";

/**
 * `finished`: an enabled one-time task with nothing left to fire (no next
 * firing and an outcome on record). It says nothing about how the agent's
 * work went; that is the occurrence's status, and the conversation.
 */
export function taskState(task: ScheduledTask): TaskState {
  if (!task.enabled) return "paused";
  if (task.schedule.kind === "once" && !task.nextRunAt && task.lastStatus) return "finished";
  return "active";
}

export function taskBadge(task: ScheduledTask): { label: string; tone: Tone } {
  const state = taskState(task);
  if (state === "finished") return { label: "No more firings", tone: "neutral" };
  return state === "paused" ? { label: "Paused", tone: "neutral" } : { label: "Active", tone: "success" };
}

/**
 * When a task fires next, for its card. `text` is one short line: how far
 * off, then the time in the task's own zone (recurring) or the device's
 * (one-time). `exact` holds the same moment in UTC, with the offset, and on
 * this device when that's another zone: the card shows it on hover or focus,
 * and in full once the task is expanded. `null` when a finished task has
 * nothing left to fire (its last firing says what happened).
 */
export type NextFiring = { text: string; exact: string[] } | null;

export function nextFiring(task: ScheduledTask, deviceZone: string, nowMs: number): NextFiring {
  if (!task.enabled) return { text: "Won't fire while paused.", exact: [] };
  if (taskState(task) === "finished") return null;
  if (!task.nextRunAt) return { text: "No firing planned.", exact: [] };
  const next = parseStamp(task.nextRunAt);
  if (!next) return { text: `Next: ${task.nextRunAt}`, exact: [] };
  const rel = relativeTime(next.instantMs, nowMs);
  const utc = describeInstant(next.instantMs, "UTC");
  const exact: string[] = [];
  let local: string;
  if (task.schedule.kind === "cron") {
    const zone = task.schedule.timezone;
    const stamp = task.nextRunLocal ? parseStamp(task.nextRunLocal) : null;
    // The server's own wall time in the task's zone; never a guess made here.
    local = stamp ? `${formatWallShort(stamp.wall, wallIn(nowMs, "UTC").year)} ${zone}` : shortInstant(next.instantMs, "UTC", nowMs);
    if (stamp && zone !== "UTC") exact.push(`${describeStamp(task.nextRunLocal!, zone)}`);
    if (zone !== deviceZone && deviceZone !== "UTC") exact.push(`On this device: ${describeInstant(next.instantMs, deviceZone)}`);
  } else {
    local = shortInstant(next.instantMs, deviceZone, nowMs);
    if (deviceZone !== "UTC") exact.push(describeInstant(next.instantMs, deviceZone));
  }
  exact.push(utc);
  return { text: `Next ${rel} · ${local}`, exact };
}

/**
 * Each occurrence status the server records, in words. `dispatched` only
 * means the agent accepted the prompt; whether the work succeeded is in the
 * conversation, and nothing here says it did. Statuses this app doesn't
 * know are shown as the server names them.
 */
export function runStatus(status: string): { label: string; tone: Tone; explain: string } {
  switch (status) {
    case "dispatching":
      return { label: "Dispatching", tone: "system", explain: "Being handed to the agent now." };
    case "dispatched":
      return { label: "Dispatched", tone: "agent", explain: "The agent accepted the prompt. This doesn't mean it succeeded: its answer, or a model or tool error, is in the conversation." };
    case "error":
      return { label: "Not dispatched", tone: "danger", explain: "Refused before it reached the agent, for example because your model connection can't be used or your device is offline." };
    case "unknown":
      return { label: "Unknown", tone: "warning", explain: "The hand-off didn't answer within 60 seconds. It may still be running and this status may change. It isn't retried." };
    case "missed":
      return { label: "Missed", tone: "warning", explain: "The server wasn't running when it was due. Skipped firings aren't run late." };
    case "cancelled":
      return { label: "Cancelled", tone: "neutral", explain: "The task was paused, edited or deleted just before it fired." };
    case "revoked":
      return { label: "Refused: no access", tone: "danger", explain: "You'd left the Space or the room was gone, so it didn't run. The task was paused and a notice went to your personal Chat." };
    case "interrupted":
      return { label: "Interrupted", tone: "warning", explain: "The server stopped during the hand-off, so the prompt may or may not have been posted. It isn't retried." };
    default:
      return { label: status, tone: "neutral", explain: "" };
  }
}

/** The server's fixed wording for a status, which `runStatus(..).explain` already says better. */
const BOILERPLATE: Record<string, RegExp> = {
  dispatched: /^the prompt was posted and agent work was accepted\b/i,
  unknown: /^the submission had not returned after \d+s\b/i,
  interrupted: /^the server stopped while submitting this occurrence\b/i,
};

/**
 * What an occurrence's detail adds to its status, or `null` when it adds
 * nothing (the server's fixed wording for that status). A reason of its own
 * (a refusal, a model error, what was cancelled) is always kept, as a
 * sentence; a missed run says how many firings it stands for.
 */
export function runDetail(status: string, detail: string | null | undefined): string | null {
  const d = (detail ?? "").trim();
  if (!d) return null;
  if (BOILERPLATE[status]?.test(d)) return null;
  if (status === "missed") {
    const m = /; (more than \d+|\d+) occurrence\(s\) since \S+ were skipped and not run late; (?:next at (\S+)|no further occurrence)$/i.exec(d);
    if (m) {
      const count = m[1] === "1" ? "1 firing" : `${m[1]} firings`;
      return `${count} skipped.`;
    }
  }
  const sentence = d[0].toUpperCase() + d.slice(1);
  return /[.!?]$/.test(sentence) ? sentence : `${sentence}.`;
}

/** How results are ordered, without claiming a System One rerank that didn't happen. */
export function rankingText(ranking: SearchRanking): { label: string; tone: Tone; explain: string } {
  switch (ranking) {
    case "classifier":
      return { label: "Reranked by System One", tone: "success", explain: "Every result was scored by your System One connection." };
    case "mixed":
      return { label: "Partly reranked", tone: "warning", explain: "System One scored the top results; the rest follow in similarity order." };
    case "embeddings":
      return { label: "Similarity order", tone: "neutral", explain: "Ordered by embedding similarity only. System One didn't rerank these." };
  }
}

/**
 * A result's score as what it is: System One's relevance (a noul) when every
 * result was reranked, embedding similarity when none was, else just a score.
 */
export function scoreText(score: number, ranking: SearchRanking): string {
  const value = score.toFixed(2);
  if (ranking === "classifier") return `System One relevance ${value}`;
  if (ranking === "embeddings") return `similarity ${value}`;
  return `score ${value}`;
}
