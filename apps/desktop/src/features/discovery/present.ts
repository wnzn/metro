// What scheduled tasks and search answers say, in words. Pure functions of
// their inputs (plus the Spaces the user belongs to), no UI.

import type { SearchRanking, ScheduleTarget, ScheduledTask } from "../../api/discovery";
import type { Tone } from "../../ui";
import { describeInstant, describeRecurrence, describeStamp, parseStamp, recurrenceOf } from "./time";

export type SpaceLookup = { spaceName: (spaceId: string) => string | null; roomName: (spaceId: string, roomId: string) => string | null };

/** "Your personal Chat", "Acme › #lobby", or why the destination is no longer reachable. */
export function destinationText(target: ScheduleTarget, lookup: SpaceLookup): { text: string; lost: boolean } {
  if (target.kind === "chat") return { text: "Your personal Chat", lost: false };
  const space = lookup.spaceName(target.spaceId);
  if (!space) return { text: "A Space you're no longer a member of. Its next firing will be refused and the task paused.", lost: true };
  const room = lookup.roomName(target.spaceId, target.roomId) ?? target.roomId;
  return { text: `${space} › #${room}`, lost: false };
}

export function scheduleText(task: ScheduledTask): string {
  if (task.schedule.kind === "once") {
    const at = parseStamp(task.schedule.at);
    return at ? `Once, ${describeInstant(at.instantMs, "UTC")}` : `Once, at ${task.schedule.at}`;
  }
  return `${describeRecurrence(recurrenceOf(task.schedule.expression))} · ${task.schedule.timezone}`;
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

/** When it fires next, in the task's zone (recurring) or the device's (once), plus UTC. */
export function nextFiringText(task: ScheduledTask, deviceZone: string): string {
  if (!task.enabled) return "Paused. It won't fire until you resume it.";
  if (taskState(task) === "finished") return "It won't fire again: a one-time task fires once. Its outcome is below and in History.";
  if (!task.nextRunAt) return task.schedule.kind === "once" ? "No firing planned. A one-time task fires only once." : "No firing planned.";
  const next = parseStamp(task.nextRunAt);
  if (!next) return `Next: ${task.nextRunAt}`;
  const utc = describeInstant(next.instantMs, "UTC");
  if (task.schedule.kind === "cron") {
    const local = task.nextRunLocal ? describeStamp(task.nextRunLocal, task.schedule.timezone) : null;
    const parts = [local ?? utc];
    if (task.schedule.timezone !== deviceZone && deviceZone !== "UTC") parts.push(`on this device ${describeInstant(next.instantMs, deviceZone)}`);
    if (local && task.schedule.timezone !== "UTC") parts.push(utc);
    return `Next: ${parts.join(" · ")}`;
  }
  return deviceZone === "UTC" ? `Next: ${utc}` : `Next: ${describeInstant(next.instantMs, deviceZone)} · ${utc}`;
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
      return { label: "Missed", tone: "warning", explain: "Found more than 5 minutes late (the server wasn't running). It isn't run late; one row counts every skipped firing." };
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
