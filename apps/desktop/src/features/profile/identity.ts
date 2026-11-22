// How a person or the agent is drawn when there is no picture: initials and a
// tone picked from their stable id (never from the display name, which can
// change or be shared). Pure, no UI.

import type { Tone } from "../../ui";

/** Tones people's initials may use. The agent's tone and danger are never a person's. */
const PERSON_TONES: readonly Tone[] = ["runner", "success", "warning", "system", "neutral"];

/** Up to two initials: "Ada Lovelace" → "AL", "bob" → "B", nothing usable → "?". */
export function initialsOf(name: string | null | undefined): string {
  const words = (name ?? "")
    .trim()
    .split(/[\s._@-]+/)
    .filter((w) => /[\p{L}\p{N}]/u.test(w));
  const first = (w: string) => [...w].find((c) => /[\p{L}\p{N}]/u.test(c)) ?? "";
  const letters = words.length >= 2 ? first(words[0]!) + first(words[words.length - 1]!) : first(words[0] ?? "");
  return letters ? letters.toUpperCase() : "?";
}

/** The same tone for the same id, everywhere and every time. */
export function personTone(stableId: string): Tone {
  let h = 0;
  for (let i = 0; i < stableId.length; i++) h = (h * 31 + stableId.charCodeAt(i)) >>> 0;
  return PERSON_TONES[h % PERSON_TONES.length]!;
}
