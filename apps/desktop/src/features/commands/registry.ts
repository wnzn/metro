import { ApiError } from "../../api/client";
import type { Client, RoomEvent } from "../../api/client";

export type CommandPlace = "chat" | "room" | "thread";
type CommandScope = { kind: "chat" } | { kind: "room"; spaceId: string; roomId: string };
type Command = {
  name: string;
  args: string;
  description: string;
  places: readonly CommandPlace[];
  privacy: (place: CommandPlace) => string;
  validate: (args: string) => string | null;
  run: (client: Client, scope: CommandScope, args: string) => Promise<RoomEvent>;
};

const focusError = "Use at most 200 printable characters for the focus.";
const registry: readonly Command[] = [{
  name: "compact",
  args: "[focus]",
  description: "Summarize earlier messages",
  places: ["chat", "room", "thread"],
  privacy: place => place === "chat"
    ? "Uses your Chat connection. A shared connection’s owner can see the request."
    : "Uses your connection for this Space’s default agent. Its owner can see the request if shared. Space members can read the summary.",
  validate: (focus) => [...focus].length > 200 || /[\p{Cc}\p{Cs}]/u.test(focus) ? focusError : null,
  run: (client, scope, focus) => client.compact(scope, focus),
}];

export const isCommandDraft = (draft: string) => draft.startsWith("/") && !draft.startsWith("//");

export function parseCommand(draft: string, place: CommandPlace) {
  if (!isCommandDraft(draft)) return null;
  const name = /^\/(\S*)/.exec(draft)![1];
  return { name, command: registry.find(c => c.name === name && c.places.includes(place)), args: draft.slice(name.length + 1).trim() };
}

export function commandSuggestions(draft: string, place: CommandPlace) {
  if (!isCommandDraft(draft) || /\s/.test(draft)) return [];
  return registry.filter(c => c.places.includes(place) && c.name !== draft.slice(1) && c.name.startsWith(draft.slice(1)));
}

export function commandError(error: unknown): string {
  if (error instanceof ApiError) {
    if (error.status === 404 || error.status === 403) return "This conversation is no longer available.";
    if (error.status === 400) return focusError;
    if (error.status === 409) {
      if (error.details === "reply_running") return "Wait for the current reply to finish.";
      if (error.details === "nothing_new") return "Nothing new to summarize yet.";
      if (error.details === "compaction_running") return "A summary is already being written.";
    }
    if (error.status === 502 && error.details === "summary_failed") return `Couldn't summarize: ${error.message}`;
    if (error.kind === "timeout") return "Timed out waiting for the summary. Check the conversation before trying again.";
  }
  return "Couldn't summarize this conversation. Try again.";
}
