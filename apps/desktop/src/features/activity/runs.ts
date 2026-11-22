// How a run reads in the Chat's runs list: a title without the addressing
// ("@agent please list…" reads "Please list…") and a tone for its status.
// Presentation only: the task the server stored is never changed.

import type { Tone } from "../../ui";

/** The agent mention as a whole word: "@agent", not "@agents" or "me@agent.dev". */
const AGENT_MENTION = /(^|\s)@agent(?=$|[\s.,;:!?)])[,:]?/gi;

/** A run's task as a title: the @agent addressing removed, the first letter capitalized. */
export function runTitle(task: string): string {
  const stripped = task.replace(AGENT_MENTION, "$1").replace(/[ \t]{2,}/g, " ").trim();
  if (!stripped) return task.trim();
  return stripped.charAt(0).toUpperCase() + stripped.slice(1);
}

const ACTIVE = /^(queued|pending|running|active|in_progress|started|waiting)$/i;

/** The status badge's tone. */
export function runTone(status: string): Tone {
  if (ACTIVE.test(status)) return "runner";
  if (/(fail|error)/i.test(status)) return "danger";
  if (/(stop|cancel)/i.test(status)) return "warning";
  if (/(done|complete|succe|finish)/i.test(status)) return "success";
  return "neutral";
}

/** The status as the badge says it: "in_progress" reads "in progress". */
export function runStatusLabel(status: string): string {
  return status.trim().replace(/_/g, " ") || "unknown";
}
