import type { MentionDraft, MentionSelection } from "./draft";
import {
  closesMention, isRange, mentionSyntaxRanges, opensMention, overlapsRanges, saneTypedLabel,
} from "./syntax";

/** Editing affordance only. Recipient matching and send syntax live in draft.ts. */
export function mentionSuggestion(value: MentionDraft, caret: number): (MentionSelection & { query: string }) | null {
  const { text, anchors } = value;
  if (!isRange(text, { start: caret, end: caret }) || caret < 1 || !closesMention(text.slice(caret))) return null;
  const start = text.lastIndexOf("@", caret - 1);
  if (start < 0 || !opensMention(text, start)) return null;
  const query = text.slice(start + 1, caret);
  const label = query.trimEnd();
  if (label ? !saneTypedLabel(label) : query !== "") return null;
  const end = start + label.length + 1;
  if (overlapsRanges({ start, end }, [...anchors, ...(value.decisions ?? []), ...mentionSyntaxRanges(text, anchors).ignored])) return null;
  return { start, end, literal: text.slice(start, end), query };
}
