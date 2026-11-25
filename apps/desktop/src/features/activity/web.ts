import { WEB_OUTCOMES, WEB_PROVIDERS } from "../../api/webSearch";
import type { WebTool, WebToolNote } from "../../api/webSearch";

export const webToolLabel = (name: WebTool) => name === "web_search" ? "Web search" : "Read web pages";
export const webRefusal = (note: WebToolNote) => note.failure === "not_allowed" || note.failure === "invalid";

export function webAttemptRows(note: WebToolNote) {
  const visible = note.attempts.map((attempt, index) => ({ attempt, index })).filter(({ attempt }) => attempt.outcome !== "skipped_disabled");
  return visible.map(({ attempt: a, index }, i) => {
    const provider = WEB_PROVIDERS[a.provider];
    const name = a.outcome === "skipped_no_key" ? provider : `${provider} (${a.mode === "key" ? "your key" : "free"})`;
    // The connector normalizes several vendor codes (and MCP error bodies)
    // to the same outcome. Don't invent an exact HTTP status here.
    const limit = a.outcome === "limited" || a.outcome === "out_of_credits";
    const until = a.until && (limit || a.outcome === "blocked" || a.outcome === "skipped_cooldown")
      ? ` · retry after ${a.until.slice(0, 10)} ${a.until.slice(11, 16)} UTC` : "";
    const next = visible[i + 1]?.attempt;
    const switched = limit && !!next && next.provider !== a.provider &&
      next.outcome !== "skipped_no_key" && next.outcome !== "skipped_cooldown";
    const text = limit ? `${name} ${WEB_OUTCOMES[a.outcome]}` : `${name}: ${WEB_OUTCOMES[a.outcome]}`;
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
    case "exhausted": return `${tool} failed: ${note.attempts.length && note.attempts.every((a) => a.outcome === "skipped_disabled") ? "all providers were off" : "every provider was at its limit or unavailable"}.`;
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
  const m = /^(?:web_search|web_fetch) via (?:Exa|Parallel|Tavily|Firecrawl|TinyFish) \((?:free|your key)\): (\d{1,2} (?:results?|pages?)(?: \(as text\))?|results as text)(?:, (\d{1,2}) failed)?(?=$|\n| \()/.exec(text);
  if (!m) return "";
  return `${m[1]}${m[2] ? ` · ${m[2]} failed` : ""}`;
}
