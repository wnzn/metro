import { expect, test } from "nsh/test";
import { captureTypedMentions, pickMention, prepareMentionSend } from "./draft";
import { confirmChangedAutomaticMentions } from "./preflight";

const a = { id: "ag-a", handle: "coder", name: "Original Coder" };
const b = { id: "ag-b", handle: "review", name: "Other Coder" };
const initial = { members: [], agents: [a, b] };
const text = "@coder please check";

test("fresh preflight does not silently substitute a moved, missing, or newly ambiguous alias", () => {
  const typed = captureTypedMentions({ text, anchors: [] }, initial);
  expect(confirmChangedAutomaticMentions(typed, initial)).toBe(typed);
  const named = (agentId: string) => ({ kind: "agent", agentId });
  for (const [agents, choices] of [
    [[{ ...a, handle: "former" }, { ...b, handle: "coder" }], [named(b.id), named(a.id)]],
    [[{ ...a, handle: "former" }, b], [named(a.id)]],
    [[a, { ...b, name: "Coder" }], [named(a.id), named(b.id)]],
    [[b], [named(b.id)]],
  ] as const) {
    const directory = { members: [], agents };
    const changed = confirmChangedAutomaticMentions(typed, directory);
    expect(changed.text).toBe(text);
    expect(changed.anchors).toEqual([]);
    expect(prepareMentionSend(changed, directory)).toMatchObject({ ok: false, reason: "ambiguous", match: { choices } });
  }
});

test("preflight checks a shared display name against both people and named agents", () => {
  const member = { id: "bob", displayName: "River" };
  const directory = { members: [member], agents: [] };
  const typed = captureTypedMentions({ text: "@River please check", anchors: [] }, directory);
  const fresh = { ...directory, agents: [{ ...a, name: "River" }] };
  const changed = confirmChangedAutomaticMentions(typed, fresh);
  expect(prepareMentionSend(changed, fresh)).toMatchObject({
    ok: false, reason: "ambiguous",
    match: { choices: [{ kind: "agent", agentId: a.id }, { kind: "user", id: member.id }] },
  });
});

test("an explicit pick retains its live stable ID even if its displayed name and handle move", () => {
  const typed = captureTypedMentions({ text, anchors: [] }, initial);
  const picked = pickMention(typed, typed.anchors[0], { kind: "agent", agentId: a.id }, initial)!;
  const directory = { members: [], agents: [
    { ...a, handle: "former", name: "Renamed" }, { ...b, handle: "coder", name: a.name },
  ] };
  expect(confirmChangedAutomaticMentions(picked, directory)).toBe(picked);
  expect(prepareMentionSend(picked, directory)).toMatchObject({
    ok: true, body: { mentions: [{ kind: "agent", agent_id: a.id }] },
  });
});
