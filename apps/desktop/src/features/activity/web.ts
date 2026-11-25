import { WEB_OUTCOMES, WEB_PROVIDERS } from "../../api/webSearch";
import type { WebTool, WebToolNote } from "../../api/webSearch";

const META_LABELS: Record<string, string> = {
  spaces_list: "List your Spaces", space_read: "Read a Space", spaces_catch_up: "Catch up in your Spaces",
  spaces_search: "Search your Spaces", waiting_on_me: "Find follow-ups", space_send: "Send a Space message",
  runs_list: "Check runs", run_cancel: "Stop a run", run_retry: "Retry a run",
  schedules_list: "List scheduled prompts", schedule_create: "Schedule a prompt",
  schedule_set_enabled: "Change a scheduled prompt", pages_find: "Find Pages",
  page_propose: "Propose a page draft", settings_explain: "Explain settings",
};
export const metaToolLabel = (tool: string) => META_LABELS[tool] ?? "Metro tool";

export const webToolLabel = (name: WebTool) => name === "web_search" ? "Web search" : "Read web pages";
export const webRefusal = (note: WebToolNote) => note.failure === "not_allowed" || note.failure === "invalid";

export function webAttemptRows(note: WebToolNote) {
  const visible = note.attempts.map((attempt, index) => ({ attempt, index })).filter(({ attempt }) => attempt.outcome !== "skipped_disabled");
  return visible.map(({ attempt: a, index }, i) => {
    const provider = WEB_PROVIDERS[a.provider];
    const name = a.outcome === "skipped_no_key" ? provider : `${provider} (${a.mode === "key" ? "saved key" : "no key"})`;
    // The connector normalizes several vendor codes (and MCP error bodies)
    // to the same outcome. Don't invent an exact HTTP status here.
    const limit = a.outcome === "limited" || a.outcome === "out_of_credits";
    const until = a.until && (limit || a.outcome === "blocked" || a.outcome === "skipped_cooldown")
      ? ` · retry after ${a.until.slice(0, 10)} ${a.until.slice(11, 16)} UTC` : "";
    // A skipped candidate is still a row, but no request went to it.
    const next = visible.slice(i + 1).find(({ attempt }) => !attempt.outcome.startsWith("skipped_") && attempt.contacted !== false)?.attempt;
    // Old key_rejected rows cannot tell us whether this candidate was tried.
    // Don't claim the immediate destination past that ambiguity, either.
    const switched = limit && !!next && next.provider !== a.provider &&
      (next.outcome !== "key_rejected" || next.contacted === true);
    const outcome = a.outcome !== "key_rejected" || a.contacted === true ? WEB_OUTCOMES[a.outcome] :
      a.contacted === false ? "skipped (saved key was already rejected)" : "key rejected (contact not recorded)";
    const text = limit ? `${name} ${outcome}` : `${name}: ${outcome}`;
    return { index, switched, text: `${text}${until}.${switched ? ` Switched to ${WEB_PROVIDERS[next.provider]}.` : ""}` };
  });
}

/** The only text we extract from a failure is the name in the server's fixed offline template. */
export function webOfflineOwner(text: string): string | null {
  const match = /^(?:Web search|Reading web pages) couldn't run: Metro isn't running on ([^\r\n]+)'s computer, so it can't search for them\./.exec(text);
  return match?.[1]?.slice(0, 100) ?? null;
}

export function webFailureText(note: WebToolNote, own: boolean, offlineOwner?: string | null): string {
  const tool = webToolLabel(note.name);
  switch (note.failure) {
    case "offline": return `${tool} needs ${own ? "your" : offlineOwner ? `${offlineOwner}'s` : "the starter's"} Metro app, which is offline. Nothing was sent.`;
    case "no_device": return `${tool} couldn't run: ${own ? "you haven't" : "the starter hasn't"} set up a computer for web search.`;
    case "exhausted": return "No provider returned a usable result.";
    case "busy": return `${tool} couldn't run: the starter's computer is busy. Try again shortly.`;
    case "timed_out": return `${tool} couldn't run: the starter's computer didn't answer in time.`;
    case "revoked": return `${tool} was interrupted because access changed.`;
    case "refused": return `${tool} was refused by the starter's computer.`;
    case "bad_response": return `${tool} couldn't run: Metro couldn't use the answer.`;
    case "not_allowed": return `${tool} was not allowed. Nothing was sent.`;
    case "invalid": return `${tool} received an invalid request. Nothing was sent.`;
    default: return `${tool} failed.`;
  }
}

/** The server's public result summary, not result titles, page text or diagnostics. */
export function webResultCount(text: string): string {
  // The stored wire copy predates the neutral attempt labels above.
  const m = /^(?:web_search|web_fetch) via (?:Exa|Parallel|Tavily|Firecrawl|TinyFish) \((?:free|your key)\): (\d{1,2} (?:results?|pages?)(?: \(as text\))?|results as text)(?:, (\d{1,2}) failed)?(?=$|\n| \()/.exec(text);
  if (!m) return "";
  return `${m[1]}${m[2] ? ` · ${m[2]} failed` : ""}`;
}
