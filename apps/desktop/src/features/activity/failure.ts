// What a failure says to people: one plain sentence, with the technical text
// (a provider's error, a status code, a stack) kept for a "Details" fold.
// Domain mapping only, no UI.

/** Markers of text written for developers rather than people. */
const TECHNICAL = /(ProviderError|CompletionError|RequestError|HttpError|\bHTTP\s?\d{3}\b|status(?: code)?:?\s*\d{3}|\{\s*"|Traceback|panicked at|\w::\w|\bat [\w$.]+ ?\(|\berror\[|\bErr\(|\bstack\b)/i;

const HINTS: [RegExp, string][] = [
  [/\b(401|403)\b|unauthori[sz]ed|forbidden|invalid[_ ]api[_ ]key|authentication/i, "The model provider refused the sign-in or key."],
  [/\b429\b|rate.?limit|quota|too many requests/i, "The model provider is limiting requests right now."],
  [/time(d)?.?out|deadline/i, "The model provider didn't answer in time."],
  [/device.*offline|connector.*offline|not connected/i, "The device that runs this model isn't connected."],
  [/connection refused|unreachable|dns|could not connect|failed to connect|connect error/i, "The model connection couldn't be reached."],
  [/\b5\d\d\b|internal server error|bad gateway|service unavailable|overloaded/i, "The model provider had a problem answering."],
];

export type FailureText = {
  /** One sentence for people. */
  sentence: string;
  /** The text as it came, when it says more than the sentence; null otherwise. */
  details: string | null;
};

/** Plain text stays as it is; technical text becomes one sentence and keeps its words for "Details". */
export function failureText(raw: string): FailureText {
  const text = raw.trim();
  if (!text) return { sentence: "Something went wrong.", details: null };
  const technical = TECHNICAL.test(text) || text.length > 240 || text.split("\n").length > 3;
  if (!technical) return { sentence: text, details: null };
  for (const [re, sentence] of HINTS) if (re.test(text)) return { sentence, details: text };
  // "The agent failed: <technical>" keeps its plain opening.
  const head = /^([^:\n{]{3,80}):/.exec(text)?.[1]?.trim();
  if (head && !TECHNICAL.test(head)) return { sentence: `${head.replace(/[.!]+$/, "")}.`, details: text };
  return { sentence: "Something went wrong.", details: text };
}
