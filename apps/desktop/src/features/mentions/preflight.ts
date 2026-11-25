import { mentionIdentityKey, scanMentionDraft } from "./draft";
import type { MentionDirectory, MentionDraft, MentionRecipient } from "./draft";

/** Do not apply fresh aliases to a draft; require a deliberate pick for each changed binding. */
export function confirmChangedAutomaticMentions(draft: MentionDraft, directory: MentionDirectory): MentionDraft {
  const automatic = draft.anchors.filter(a => !a.picked);
  if (!automatic.length) return draft;
  const current = new Map(scanMentionDraft(
    { ...draft, anchors: draft.anchors.filter(a => a.picked) }, directory, { finalize: true },
  ).map(match => [match.start, match]));
  const people: MentionRecipient[] = directory.members.map(m => ({ kind: "user", id: m.id }));
  const agents: MentionRecipient[] = directory.agents.map(a => ({ kind: "agent", agentId: a.id }));
  const live = new Map([...people, ...agents].map(mention => [mentionIdentityKey(mention), mention]));
  const changed = new Set<number>();
  const decisions = [...(draft.decisions ?? [])];
  for (const a of automatic) {
    const match = current.get(a.start);
    const pinned = mentionIdentityKey(a.mention);
    if (match?.kind === "resolved" && match.end === a.end && mentionIdentityKey(match.mention) === pinned) continue;
    const matches = match?.kind === "resolved" ? [match.mention] : match?.kind === "ambiguous" ? match.choices : [];
    const choices = [...new Set([...matches.map(mentionIdentityKey), pinned])]
      .flatMap(key => live.has(key) ? [live.get(key)!] : []);
    decisions.push({
      kind: "ambiguous", preflight: true, start: a.start, end: a.end, literal: a.literal,
      choices: choices.length ? choices : a.mention.kind === "user" ? people : agents,
    });
    changed.add(a.start);
  }
  return changed.size ? {
    text: draft.text,
    anchors: draft.anchors.filter(a => !changed.has(a.start)),
    decisions: decisions.sort((a, b) => a.start - b.start),
  } : draft;
}
