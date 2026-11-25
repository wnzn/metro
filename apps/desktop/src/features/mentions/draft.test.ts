import { expect, test } from "nsh/test";
import {
  editMentionDraft, MAX_MENTION_RECIPIENTS, MAX_MENTION_SPANS, pickMention,
  prepareMentionSend, scanMentionDraft, updateMentionDraft,
} from "./draft";
import type { MentionDirectory, MentionDraft, MentionRecipient, MentionSend } from "./draft";

const alice = { kind: "user", id: "auth0|A1" } as const;
const otherAlice = { kind: "user", id: "auth0|A2" } as const;
const coder = { kind: "agent", agentId: "ag-2" } as const;
const generic = { kind: "agent" } as const;
const roster: MentionDirectory = {
  members: [
    { id: alice.id, displayName: "Alice Moreno" },
    { id: "bo.smith", displayName: "Bó Smith" },
    { id: "bo", displayName: "Bo" },
    { id: "member-z", displayName: "雪 🧩" },
  ],
  agents: [
    { id: coder.agentId, handle: "coder", name: "Code Helper" },
    { id: "ag-1", handle: "agent", name: "Research Partner" },
    { id: "ag-3", handle: "alice", name: "Another Helper" },
  ],
};
const draft = (text: string): MentionDraft => ({ text, anchors: [] });
const send = (text: string, directory = roster) => prepareMentionSend(draft(text), directory);
const body = (result: MentionSend) => {
  if (!result.ok) throw new Error(`Unexpected send error: ${result.reason}`);
  return result.body;
};
const pick = (source: MentionDraft, start: number, end: number, mention: MentionRecipient, dir = roster) => {
  const result = pickMention(source, { start, end, literal: source.text.slice(start, end) }, mention, dir);
  if (!result) throw new Error("Picker rejected the test selection");
  return result;
};

test("longest completed name supersedes a shorter agent handle; full names, ids and handles are case-insensitive", () => {
  const text = "(@aLICE mORENO), @AUTH0|a1 @BO.SMITH @bÓ sMITH @cOdEr @CODE HELPER!";
  const matches = scanMentionDraft(draft(text), roster);
  expect(matches.map(m => m.literal)).toEqual([
    "@aLICE mORENO", "@AUTH0|a1", "@BO.SMITH", "@bÓ sMITH", "@cOdEr", "@CODE HELPER",
  ]);
  const out = body(send(text));
  expect(out.mentions).toEqual([alice, { kind: "user", id: "bo.smith" }, { kind: "agent", agent_id: "ag-2" }]);
  expect(out.spans?.map(s => s.index)).toEqual([0, 0, 1, 1, 2, 2]);
});

test("same full label across handle, member id and display name asks in priority order instead of running an agent", () => {
  const directory: MentionDirectory = {
    ...roster,
    members: [...roster.members, { id: "CoDeR", displayName: "Coder" }, { id: "human", displayName: "Coder" }],
  };
  const result = send("@Code Helper and @coder", directory);
  expect(result).toEqual({
    ok: false, reason: "ambiguous",
    match: {
      kind: "ambiguous", start: 17, end: 23, literal: "@coder",
      choices: [coder, { kind: "user", id: "CoDeR" }, { kind: "user", id: "human" }],
    },
  });
});

test("duplicate aliases for one stable identity do not create ambiguity; user and agent namespaces are distinct", () => {
  const directory: MentionDirectory = {
    members: [{ id: "coder", displayName: "CODER" }, { id: "coder", displayName: "Coder" }],
    agents: [{ id: "coder", handle: "robot", name: "Robot" }, { id: "coder", handle: "robot", name: "Robot" }],
  };
  const out = body(send("@coder @Robot @CODER", directory));
  expect(out.mentions).toEqual([{ kind: "user", id: "coder" }, { kind: "agent", agent_id: "coder" }]);
  expect(out.spans?.map(s => s.index)).toEqual([0, 1, 0]);
});

test("bare @agent is reserved in typed text; the original agent can still be addressed by full name", () => {
  const directory = {
    ...roster,
    members: [
      ...roster.members, { id: "agent", displayName: "Agent" }, { id: "person", displayName: "Agent Smith" },
    ],
  };
  const out = body(send("@aGeNt @agent. @Research Partner @Agent Smith", directory));
  expect(out.mentions).toEqual([generic, { kind: "agent", agent_id: "ag-1" }, { kind: "user", id: "person" }]);
  expect(out.spans?.map(s => s.index)).toEqual([0, 0, 1, 2]);
  // Explicitly picking the human is not the same as typing the reserved token.
  const picked = pick(draft("@Ag"), 0, 3, { kind: "user", id: "agent" }, directory);
  expect(body(prepareMentionSend(picked, directory))).toEqual({
    text: "@Agent ", mentions: [{ kind: "user", id: "agent" }], spans: [{ start: 0, end: 6, index: 0 }],
  });
});

test("await a typed boundary without prematurely anchoring a short alias; preflight checks EOF", () => {
  const directory = { ...roster, members: [...roster.members, { id: otherAlice.id, displayName: "Alice Moreno" }] };
  expect(scanMentionDraft(draft("@Alice"), directory)).toEqual([]);
  expect(scanMentionDraft(draft("@Alice "), directory)[0]?.kind).toBe("resolved");
  expect(scanMentionDraft(draft("@Alice Moreno"), directory)).toEqual([]);
  expect(scanMentionDraft(draft("@Alice Moreno"), directory, { finalize: true })[0]?.kind).toBe("ambiguous");
  expect(scanMentionDraft(draft("@Alice Moreno!"), directory)[0]?.kind).toBe("ambiguous");
  const atSend = send("@Alice Moreno", directory);
  expect(atSend.ok ? null : atSend.reason).toBe("ambiguous");
  expect(body(send("@Alice Moreno")).mentions).toEqual([alice]);
});

test("an ambiguous full name becomes a stable anchor when a suggestion is tapped", () => {
  const directory = { ...roster, members: [...roster.members, { id: otherAlice.id, displayName: "Alice Moreno" }] };
  const source = draft("🙂 @alice moreno, next");
  const [match] = scanMentionDraft(source, directory);
  expect(match.kind).toBe("ambiguous");
  const picked = pickMention(source, match, otherAlice, directory)!;
  expect(picked.text).toBe("🙂 @Alice Moreno, next");
  expect(picked.anchors).toEqual([{ start: 3, end: 16, literal: "@Alice Moreno", mention: otherAlice }]);
  expect(body(prepareMentionSend(picked, directory))).toEqual({
    text: picked.text, mentions: [otherAlice], spans: [{ start: 5, end: 18, index: 0 }],
  });
});

test("picker handles bare @, the @ button, punctuation and opaque ids; rejects stale selections", () => {
  const at = pick(draft("say (@), please"), 5, 6, coder);
  expect(at.text).toBe("say (@Code Helper), please");
  const appended = pick(draft("say"), 3, 3, alice);
  expect(appended.text).toBe("say @Alice Moreno ");
  const inserted = pick(draft("say now"), 4, 4, alice);
  expect(inserted.text).toBe("say @Alice Moreno now");
  const fallback = { ...roster, members: [{ id: alice.id }] };
  expect(pick(draft("@a"), 0, 2, alice, fallback).text).toBe("@auth0|A1 ");
  expect(pickMention(draft("@bo"), { start: 0, end: 3, literal: "@al" }, alice, roster)).toBe(null);
  expect(pickMention(draft("`@a`"), { start: 1, end: 3, literal: "@a" }, alice, roster)).toBe(null);
  expect(pickMention(draft("@ali@foo"), { start: 0, end: 4, literal: "@ali" }, alice, roster)).toBe(null);
});

test("unresolved and lookalike text stays unchanged and has no addressing metadata", () => {
  const text = "Hey @Alise, @boo! mail x@coder or @coder@example.org; \\@agent @bo.smi @bo@ @ alice.";
  expect(scanMentionDraft(draft(text), roster).map(m => [m.kind, m.literal])).toEqual([
    ["unresolved", "@Alise"], ["unresolved", "@boo"], ["unresolved", "@bo.smi"],
  ]);
  expect(send(text)).toEqual({ ok: true, body: { text } });
  expect(scanMentionDraft(draft("@Alise"), roster, { finalize: true })[0]?.kind).toBe("unresolved");
  expect(send("@Alise")).toEqual({ ok: true, body: { text: "@Alise" } });
  expect(send(" \t\n ")).toEqual({ ok: true, body: { text: " \t\n " } });
});

test("pasted inline and fenced code is ignored, including open fences and a fence exposed by server trim", () => {
  const text = "    ~~~ js\n@Agent\n~~~\n@bo\n`` @coder ` @agent ``\n```sh\n@alice\n```~~\n@coder\n```\n@agent\n~~~\n@coder";
  expect(scanMentionDraft(draft(text), roster).map(m => m.literal)).toEqual(["@bo", "@agent"]);
  expect(body(send(text)).mentions).toEqual([{ kind: "user", id: "bo" }, generic]);
  expect(body(send("text ` unmatched @coder")).mentions).toEqual([{ kind: "agent", agent_id: "ag-2" }]);
});

test("interior @ is allowed in selected labels, not inferred from typed email-like text", () => {
  const support = { kind: "user", id: "support" } as const;
  const directory = {
    ...roster, members: [...roster.members, { id: support.id, displayName: "Support@Home" }],
  };
  const typed = "@Support@Home and Support@Home";
  expect(send(typed, directory)).toEqual({ ok: true, body: { text: typed } });
  const selected = pick(draft("@Supp @agent"), 0, 5, support, directory);
  expect(body(prepareMentionSend(selected, directory))).toEqual({
    text: "@Support@Home @agent", mentions: [support, generic],
    spans: [{ start: 0, end: 13, index: 0 }, { start: 14, end: 20, index: 1 }],
  });
});

test("selected backticks do not open inline code or hide the adjacent @agent", () => {
  const dev = { kind: "user", id: "devops" } as const;
  const directory = {
    ...roster, members: [...roster.members, { id: dev.id, displayName: "Dev`Ops" }],
  };
  expect(body(send("@Dev`Ops", directory)).mentions).toBe(undefined);
  const selected = pick(draft("@Dev @agent ` @coder `"), 0, 4, dev, directory);
  const moved = updateMentionDraft(selected, `🌿 ${selected.text}`);
  expect(moved.anchors.map(a => a.literal)).toEqual(["@Dev`Ops"]);
  expect(scanMentionDraft(moved, directory).map(m => m.literal)).toEqual(["@Dev`Ops", "@agent"]);
  expect(body(prepareMentionSend(moved, directory))).toEqual({
    text: "🌿 @Dev`Ops @agent ` @coder `", mentions: [dev, generic],
    spans: [{ start: 5, end: 13, index: 0 }, { start: 14, end: 20, index: 1 }],
  });
});

test("an explicit label cannot swallow a standalone @ token", () => {
  const support = { kind: "user", id: "support" } as const;
  const directory = {
    ...roster, members: [...roster.members, { id: support.id, displayName: "Support: @agent" }],
  };
  expect(pick(draft("@Supp"), 0, 5, support, directory).text).toBe("@support ");
  const text = "@Support: @agent";
  expect(prepareMentionSend({
    text, anchors: [{ start: 0, end: text.length, literal: text, mention: support }],
  }, directory)).toEqual({ ok: false, reason: "invalid-anchor" });
  expect(body(send(text, directory)).mentions).toEqual([support, generic]);
});

test("UTF-8 byte ranges use exact untrimmed text through multiline and multibyte paste", () => {
  const text = "\u3000  👋 é\n(@雪 🧩), @Bo!\u0085@雪 🧩  \n";
  const out = body(send(text));
  expect(out).toEqual({
    text,
    mentions: [{ kind: "user", id: "member-z" }, { kind: "user", id: "bo" }],
    spans: [{ start: 14, end: 23, index: 0 }, { start: 26, end: 29, index: 1 }, { start: 32, end: 41, index: 0 }],
  });
  const bytes = new TextEncoder().encode(text);
  expect(out.spans?.map(s => new TextDecoder().decode(bytes.slice(s.start, s.end))))
    .toEqual(["@雪 🧩", "@Bo", "@雪 🧩"]);
  expect(send("hi \ud801 @bo")).toEqual({ ok: false, reason: "invalid-text" });
});

test("edits before and after anchors keep chosen IDs and shift only the affected UTF-16 coordinates", () => {
  const first = pick(draft("x @al, @cod"), 2, 5, alice);
  const both = pick(first, first.text.indexOf("@cod"), first.text.length, coder);
  const before = updateMentionDraft(both, `🌿\n${both.text}`);
  const after = updateMentionDraft(before, `${before.text}résumé`);
  expect(after.anchors.map(a => a.start)).toEqual(both.anchors.map(a => a.start + 3));
  expect(after.anchors.map(a => a.mention)).toEqual([alice, coder]);
  const middle = editMentionDraft(after, { start: after.anchors[0].end, end: after.anchors[0].end, replacement: " —" })!;
  expect(middle.anchors[0]).toEqual(after.anchors[0]);
  expect(middle.anchors[1].start).toBe(after.anchors[1].start + 2);
});

test("edits through or inside a mention drop it; a new token is assessed on its own", () => {
  const first = pick(draft("x @al, @cod"), 2, 5, alice);
  const both = pick(first, first.text.indexOf("@cod"), first.text.length, coder);
  const touched = editMentionDraft(both, { start: 5, end: 5, replacement: "x" })!;
  expect(touched.anchors.map(a => a.mention)).toEqual([coder]);
  expect(body(prepareMentionSend(touched, roster)).mentions).toEqual([{ kind: "agent", agent_id: "ag-2" }]);
  const replaced = editMentionDraft(both, { start: 3, end: 4, replacement: both.text[3] })!;
  expect(replaced.anchors.map(a => a.mention)).toEqual([coder]);
  const pasted = updateMentionDraft(both, "x @雪 🧩");
  expect(pasted.anchors).toEqual([]);
  expect(body(prepareMentionSend(pasted, roster)).mentions).toEqual([{ kind: "user", id: "member-z" }]);
});

test("adjacent edits that change token boundaries or put a chosen mention in code drop the anchor", () => {
  const source = pick(draft("x @cod"), 2, 6, coder);
  const a = source.anchors[0];
  for (const edit of [
    { start: a.start, end: a.start, replacement: "\\" },
    { start: a.end, end: a.end, replacement: "y" },
    { start: 0, end: 0, replacement: "```\n" },
  ]) {
    const next = editMentionDraft(source, edit)!;
    expect(next.anchors).toEqual([]);
    expect(body(prepareMentionSend(next, roster)).mentions).toBe(undefined);
  }
  expect(editMentionDraft(draft("🧩"), { start: 1, end: 2, replacement: "" })).toBe(null);
});

test("identical labels cannot inherit each other's IDs on a value-only edit; exact ranges preserve the survivor", () => {
  const directory = { ...roster, members: [...roster.members, { id: otherAlice.id, displayName: "Alice Moreno" }] };
  const first = pick(draft("@al @al"), 0, 3, alice, directory);
  const second = pick(first, first.text.lastIndexOf("@al"), first.text.length, otherAlice, directory);
  const snapshot = updateMentionDraft(second, "@Alice Moreno ");
  expect(snapshot.anchors).toEqual([]);
  const snapshotSend = prepareMentionSend(snapshot, directory);
  expect(snapshotSend.ok ? null : snapshotSend.reason).toBe("ambiguous");
  const exact = editMentionDraft(second, { start: 0, end: second.anchors[1].start, replacement: "" })!;
  expect(exact.anchors.map(a => a.mention)).toEqual([otherAlice]);
  expect(body(prepareMentionSend(exact, directory)).mentions).toEqual([otherAlice]);
});

test("stale or malformed anchors fail closed rather than falling back to textual routing", () => {
  const picked = pick(draft("@al"), 0, 3, alice);
  expect(prepareMentionSend(picked, { ...roster, members: [] })).toEqual({ ok: false, reason: "stale-recipient" });
  expect(prepareMentionSend({ ...picked, anchors: [...picked.anchors, ...picked.anchors] }, roster))
    .toEqual({ ok: false, reason: "invalid-anchor" });
  expect(prepareMentionSend({ ...picked, text: "@Another Other " }, roster))
    .toEqual({ ok: false, reason: "invalid-anchor" });
});

test("exact span and recipient caps pass; overflow blocks instead of dropping a suffix", () => {
  const repeated = "@bo ".repeat(MAX_MENTION_SPANS);
  const out = body(send(repeated));
  expect(out.spans?.length).toBe(MAX_MENTION_SPANS);
  expect(out.mentions).toEqual([{ kind: "user", id: "bo" }]);
  expect(out.spans?.every(s => s.index === 0)).toBe(true);
  expect(send(`${repeated}@bo`)).toEqual({ ok: false, reason: "too-many-spans" });
  const members = Array.from({ length: MAX_MENTION_RECIPIENTS + 1 }, (_, i) => ({ id: `p${i}` }));
  const directory = { members, agents: [] };
  const full = members.slice(0, -1).map(m => `@${m.id}`).join(" ");
  expect(body(send(full, directory)).mentions?.length).toBe(MAX_MENTION_RECIPIENTS);
  expect(send(`${full} @${members[members.length - 1].id}`, directory))
    .toEqual({ ok: false, reason: "too-many-recipients" });
});

test("display-name limit counts Unicode scalars, not code units or wire bytes", () => {
  const name = "🧩".repeat(64);
  const tooLong = `${name}🧩`;
  const directory = { members: [{ id: "short", displayName: name }, { id: "long", displayName: tooLong }], agents: [] };
  expect(body(send(`@${name}`, directory)).spans).toEqual([{ start: 0, end: 257, index: 0 }]);
  expect(body(send(`@${tooLong}`, directory)).mentions).toBe(undefined);
  expect(pick(draft("@lo"), 0, 3, { kind: "user", id: "long" }, directory).text).toBe("@long ");
});
