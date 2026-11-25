import { expect, test } from "nsh/test";
import {
  captureTypedMentions, editMentionDraft, MAX_MENTION_RECIPIENTS, MAX_MENTION_SPANS, mentionIdentityKey, pickMention,
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
    members: [{ id: "coder", displayName: "MEMBER CODER" }, { id: "coder", displayName: "Member Coder" }],
    agents: [{ id: "coder", handle: "robot", name: "Robot" }, { id: "coder", handle: "robot", name: "Robot" }],
  };
  const out = body(send("@Member Coder @Robot @MEMBER CODER", directory));
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
  expect(scanMentionDraft(draft("@Alice "), directory)).toEqual([]);
  expect(scanMentionDraft(draft("@Alice Moreno"), directory)).toEqual([]);
  expect(scanMentionDraft(draft("@Alice Moreno"), directory, { finalize: true })[0]?.kind).toBe("ambiguous");
  expect(scanMentionDraft(draft("@Alice Moreno!"), directory)[0]?.kind).toBe("ambiguous");
  const atSend = send("@Alice Moreno", directory);
  expect(atSend.ok ? null : atSend.reason).toBe("ambiguous");
  expect(body(send("@Alice Moreno")).mentions).toEqual([alice]);
});

test("capture keeps a short alias provisional through single- and multiword typing", () => {
  let typed = draft("");
  for (const text of ["@Alice", "@Alice ", "@Alice M", "@Alice Moreno"]) {
    typed = captureTypedMentions(updateMentionDraft(typed, text), roster);
    expect(typed.anchors).toEqual([]);
    expect(typed.decisions).toBe(undefined);
  }
  typed = captureTypedMentions(updateMentionDraft(typed, "@Alice Moreno "), roster);
  expect(typed.anchors.map(a => a.mention)).toEqual([alice]);
  const middle = captureTypedMentions(draft("@Alice M existing tail"), roster, { caret: "@Alice M".length });
  expect(middle.anchors).toEqual([]);
  const short = captureTypedMentions(draft("@Alice "), roster, { finalize: true });
  expect(short.anchors[0]?.mention).toEqual({ kind: "agent", agentId: "ag-3" });
  expect(captureTypedMentions(draft("@bo."), roster).anchors).toEqual([]);
});

test("a completed typed person cannot silently retarget to an agent after rename or revocation", () => {
  const ada = { kind: "user", id: "oidc|ada" } as const;
  const first: MentionDirectory = { members: [{ id: ada.id, displayName: "Ada" }], agents: [] };
  const next: MentionDirectory = {
    members: [{ id: ada.id, displayName: "Ada Lovelace" }],
    agents: [{ id: "ag-replacement", name: "Agent", handle: "ada" }],
  };
  const revoked = { ...next, members: [] };
  const typed = captureTypedMentions(draft("@Ada "), first);
  expect(typed.anchors).toEqual([{ start: 0, end: 4, literal: "@Ada", mention: ada }]);
  const prefixed = captureTypedMentions(updateMentionDraft(typed, `👋 ${typed.text}`), next);
  const moved = captureTypedMentions(updateMentionDraft(prefixed, `${prefixed.text}next`), next);
  expect(body(prepareMentionSend(moved, next)).mentions).toEqual([ada]);
  expect(scanMentionDraft(moved, revoked)[0]).toEqual({
    kind: "resolved", start: 3, end: 7, literal: "@Ada", mention: ada,
  });
  expect(prepareMentionSend(moved, revoked)).toEqual({ ok: false, reason: "stale-recipient" });
  // The value-only API cannot prove survival through a broad, two-sided
  // change, but still must not reinterpret the same spelling as the agent.
  const broad = captureTypedMentions(updateMentionDraft(typed, moved.text), next);
  const broadSend = prepareMentionSend(broad, next);
  expect(broadSend.ok ? null : broadSend.reason).toBe("ambiguous");
  // Actually replacing the occurrence (as opposed to refreshing its data)
  // can resolve the same visible spelling to a newly chosen identity.
  const retyped = editMentionDraft(moved, { start: 3, end: 7, replacement: "@Ada" })!;
  expect(body(prepareMentionSend(captureTypedMentions(retyped, revoked), revoked)).mentions)
    .toEqual([{ kind: "agent", agent_id: "ag-replacement" }]);
});

test("completed ambiguity and plain decisions do not change when roster candidates change", () => {
  const directory = {
    members: [{ id: "person-a", displayName: "Review" }, { id: "person-b", displayName: "Review" }],
    agents: [],
  };
  const refresh = {
    members: [],
    agents: [{ id: "ag-new", handle: "review", name: "Latest" }, { id: "ag-other", handle: "spare", name: "Spare" }],
  };
  const ambiguous = captureTypedMentions(draft("@Review "), directory);
  const blocked = prepareMentionSend(ambiguous, refresh);
  expect(blocked.ok ? null : blocked.reason).toBe("ambiguous");
  const plain = captureTypedMentions(draft("@Spare "), directory);
  const moved = captureTypedMentions(updateMentionDraft(plain, `hello ${plain.text}`), refresh);
  expect(scanMentionDraft(moved, refresh)).toEqual([
    { kind: "unresolved", start: 6, end: 12, literal: "@Spare" },
  ]);
  expect(prepareMentionSend(moved, refresh)).toEqual({ ok: true, body: { text: "hello @Spare " } });
});

test("chooser order and focus keys are stable across directory reordering", () => {
  const directory = {
    members: [{ id: "member-z", displayName: "Review" }, { id: "member-a", displayName: "Review" }],
    agents: [{ id: "ag-z", name: "Review", handle: "review" }, { id: "ag-a", name: "Review", handle: "helper" }],
  };
  const source = draft("@Review ");
  const choices = (d: MentionDirectory) => {
    const match = scanMentionDraft(source, d)[0];
    return match.kind === "ambiguous" ? match.choices.map(mentionIdentityKey) : [];
  };
  const expected = ["agent:ag-z", "agent:ag-a", "user:member-a", "user:member-z"];
  expect(choices(directory)).toEqual(expected);
  expect(choices({ members: [...directory.members].reverse(), agents: [...directory.agents].reverse() })).toEqual(expected);
});

test("leading underscore and hyphen handles and raw agent IDs resolve only through the directory", () => {
  const first = { id: "ag-1234567890abcdef", name: "Builder", handle: "_coder" };
  const second = { id: "ag-fedcba0987654321", name: "Tester", handle: "-helper" };
  const directory = { members: [], agents: [first, second] };
  const out = body(send(`@_CoDeR @-helper @${first.id} @${second.id}`, directory));
  expect(out.mentions).toEqual([
    { kind: "agent", agent_id: first.id }, { kind: "agent", agent_id: second.id },
  ]);
  expect(out.spans?.map(s => s.index)).toEqual([0, 1, 0, 1]);
  expect(send(`@${first.id}`, { members: [], agents: [] })).toEqual({ ok: true, body: { text: `@${first.id}` } });
  expect(pickMention(draft("@?"), { start: 0, end: 2, literal: "@?" }, { kind: "agent", agentId: first.id },
    { members: [], agents: [] })).toBe(null);
  const collision = send(`@${first.id}`, { ...directory, members: [{ id: first.id }] });
  expect(collision.ok ? null : collision.reason).toBe("ambiguous");
});

test("an ambiguous full name becomes a stable anchor when a suggestion is tapped", () => {
  const directory = { ...roster, members: [...roster.members, { id: otherAlice.id, displayName: "Alice Moreno" }] };
  const source = draft("🙂 @alice moreno, next");
  const [match] = scanMentionDraft(source, directory);
  expect(match.kind).toBe("ambiguous");
  const picked = pickMention(source, match, otherAlice, directory)!;
  expect(picked.text).toBe("🙂 @Alice Moreno, next");
  expect(picked.anchors).toEqual([{ start: 3, end: 16, literal: "@Alice Moreno", mention: otherAlice, picked: true }]);
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

test("authoritative draft keeps quoted addresses plain while normal quoted mentions still work", () => {
  const mail = `Write to <"local part"@agent> or "with \\"quote\\""@coder or 'local'@agent or "local @coder team"@example.net.`;
  expect(scanMentionDraft(draft(mail), roster)).toEqual([]);
  expect(send(mail)).toEqual({ ok: true, body: { text: mail } });
  const text = `Say "hello!" "@coder" or '@agent' but not \\@agent.`;
  expect(scanMentionDraft(draft(text), roster).map(m => m.literal)).toEqual(["@coder", "@agent"]);
  expect(body(send(text)).mentions).toEqual([{ kind: "agent", agent_id: coder.agentId }, generic]);
  const agent = captureTypedMentions(draft("@agent "), roster);
  const email = editMentionDraft(agent, { start: 0, end: 0, replacement: `"local"` })!;
  expect(email.anchors).toEqual([]);
  expect(prepareMentionSend(email, roster)).toEqual({ ok: true, body: { text: `"local"@agent ` } });
});

test("quotes in inline and fenced code cannot interfere with quoted addresses", () => {
  const text = 'See `say "` then <"local"@agent>';
  expect(scanMentionDraft(draft(text), roster)).toEqual([]);
  expect(send(text)).toEqual({ ok: true, body: { text } });

  const fenced = '```\nsay "\n<"local"@agent>\n```\n<"local"@agent> then "@bo"';
  expect(body(send(fenced)).mentions).toEqual([{ kind: "user", id: "bo" }]);

  const directory = { ...roster, members: [{ id: "member", displayName: 'Bó "Ace" Smith' }] };
  expect(body(send('Then @BÓ "ACE" SMITH, and "@coder".', directory)).mentions).toEqual([
    { kind: "user", id: "member" }, { kind: "agent", agent_id: coder.agentId },
  ]);
});

test("quoted names do not pair backward through inline or fenced code", () => {
  for (const text of [
    '>"`quote"` then "@Alice Moreno"',
    '>"``🌿 quote"`` then "@Alice Moreno"',
    '>"`quote"` then <"local"@agent> and "@Alice Moreno"',
    '>"`quote"`"local"@agent and "@Alice Moreno"',
    '>"prose\n```text\nquote" @agent\n```\nthen <"local"@agent> and "@Alice Moreno"',
  ]) {
    const mention = "@Alice Moreno";
    const start = new TextEncoder().encode(text.slice(0, text.indexOf(mention))).length;
    expect(send(text)).toEqual({
      ok: true, body: { text, mentions: [alice], spans: [{ start, end: start + mention.length, index: 0 }] },
    });
  }
});

test("a picked label is a quote boundary; an address that opens after it still stays plain", () => {
  const ops = { kind: "user", id: "ops" } as const;
  const directory = { ...roster, members: [...roster.members, { id: ops.id, displayName: 'Ops "desk' }] };
  const picked = pick(draft('>" @o'), 3, 5, ops, directory);
  for (const suffix of ['then "@Alice Moreno"', 'then <"local"@agent> and "@Alice Moreno"']) {
    const edited = updateMentionDraft(picked, `${picked.text}${suffix}`);
    expect(edited.anchors[0]?.picked).toBe(true);
    const out = body(prepareMentionSend(edited, directory));
    expect(out.mentions).toEqual([ops, alice]);
    const bytes = new TextEncoder().encode(edited.text);
    expect(out.spans?.map(s => new TextDecoder().decode(bytes.slice(s.start, s.end))))
      .toEqual(['@Ops "desk', "@Alice Moreno"]);
  }
});

test("an agent at an interrupted quote boundary requires an explicit choice", () => {
  const source = draft('>"`quote"` then "@agent"');
  const [match] = scanMentionDraft(source, roster);
  expect(match.kind).toBe("ambiguous");
  const blocked = prepareMentionSend(source, roster);
  expect(blocked.ok ? null : blocked.reason).toBe("ambiguous");
  const chosen = pickMention(source, match, generic, roster)!;
  expect(body(prepareMentionSend(chosen, roster)).mentions).toEqual([generic]);

  const before = '>"`quote"` then @agent ';
  const captured = captureTypedMentions(draft(before), roster);
  const start = before.indexOf("@agent");
  const edited = editMentionDraft(captured, { start, end: start, replacement: '"' })!;
  const afterEdit = prepareMentionSend(edited, roster);
  expect(!afterEdit.ok && afterEdit.reason === "ambiguous" ? afterEdit.match.choices : null).toEqual([generic]);
});

test("agents inside an interrupted quoted local part require an explicit choice", () => {
  for (const [text, expected] of [
    ['Use "local `note` @agent here"@example.org', [["ambiguous", "@agent"]]],
    ['Use "local `note`:@agent here"@example.org', [["ambiguous", "@agent"]]],
    ['Use "done,"local `note` @agent here"@example.org', [["ambiguous", "@agent"]]],
    ['Use "local `note` @agent. Still local"@example.org', [["ambiguous", "@agent"]]],
    ['Use "local `note` @agent here "@example.org', [["ambiguous", "@agent"]]],
    ['Use "local `first` @coder `last` @agent here"@example.org', [["ambiguous", "@coder"], ["ambiguous", "@agent"]]],
    ['Use "hello " `note` @agent here "@example.org', [["ambiguous", "@agent"]]],
    ['Say "hello " then " local `note` @agent here "@example.org', [["ambiguous", "@agent"]]],
    ['Use "local @coder `note` said "hi!" @agent here"@example.org', [["ambiguous", "@coder"], ["ambiguous", "@agent"]]],
    ['Use "local @coder `note` then " here @agent here"@example.org', [["ambiguous", "@coder"], ["ambiguous", "@agent"]]],
    ['@coder then use "local `note` @agent here"@example.org', [["resolved", "@coder"], ["ambiguous", "@agent"]]],
    ['Use "local `note` @agent " @bo', [["resolved", "@agent"], ["resolved", "@bo"]]],
    ['Use `note`. @coder finished. Say "@Alice Moreno"', [["resolved", "@coder"], ["resolved", "@Alice Moreno"]]],
    ['Use `note`. @coder finished. Say "@agent"', [["resolved", "@coder"], ["ambiguous", "@agent"]]],
    ['Use "done," `note`. @coder finished. Say "@agent"', [["ambiguous", "@coder"], ["ambiguous", "@agent"]]],
    ['Use "confirmed"@example.org `note`. @coder finished. Say "@agent"', [["resolved", "@coder"], ["ambiguous", "@agent"]]],
    ['Use `note`. @coder finished. Then "local"@agent and "@Alice Moreno"', [["resolved", "@coder"], ["resolved", "@Alice Moreno"]]],
    ['Use "local `note`\n@coder. Say "@Alice Moreno"', [["resolved", "@coder"], ["resolved", "@Alice Moreno"]]],
    ['Use "local @coder `note`\nUse `other` Say "@agent"', [["resolved", "@coder"], ["ambiguous", "@agent"]]],
  ] as const) {
    const matches = scanMentionDraft(draft(text), roster, { finalize: true }).filter(m => m.kind !== "unresolved");
    expect(matches.map(m => [m.kind, m.literal])).toEqual(expected);
    const result = prepareMentionSend(draft(text), roster);
    expect(result.ok ? null : result.reason).toBe(expected.some(([kind]) => kind === "ambiguous") ? "ambiguous" : null);
  }
});

test("interrupted local parts recheck fresh and captured agents before, between, and after opaque ranges", () => {
  for (const [opener, quote] of [
    ['Use "local ', '"'],
    ['Use "hello " then " local ', '"'],
    ["Use 'hello ' then ' local ", "'"],
  ] as const) {
    for (const [token, recipient] of [["@coder", coder], ["@agent", generic]] as const) {
      for (const placement of [`${token} \`first\` \`last\``, `\`first\` ${token} \`last\``, `\`first\` \`last\` ${token}`]) {
        for (const closing of [quote, ` ${quote}`]) {
          const before = `${opener}${placement} @Alice Moreno `;
          const after = `${before}here${closing}@example.org`;
          const captured = captureTypedMentions(draft(before), roster);
          expect(captured.anchors.map(a => a.mention)).toEqual([recipient, alice]);
          for (const candidate of [draft(after), updateMentionDraft(captured, after)]) {
            const matches = scanMentionDraft(candidate, roster).filter(m => m.kind !== "unresolved");
            expect(matches.map(m => [m.kind, m.literal])).toEqual([["ambiguous", token], ["resolved", "@Alice Moreno"]]);
            const result = prepareMentionSend(candidate, roster);
            expect(!result.ok && result.reason === "ambiguous" ? result.match.choices : null).toEqual([recipient]);
          }
        }
      }
    }
  }
});

test("the interrupted examples block automatic captures after edits", () => {
  for (const [before, suffix, expected] of [
    ['Use "local `first` @coder `last` @agent ', 'here"@example.org', [coder, generic]],
    ['Use "hello " `note` @agent ', 'here "@example.org', [generic]],
  ] as const) {
    const captured = captureTypedMentions(draft(before), roster);
    expect(captured.anchors.map(a => a.mention)).toEqual(expected);
    const edited = updateMentionDraft(captured, `${before}${suffix}`);
    expect(scanMentionDraft(edited, roster).filter(m => m.kind === "ambiguous").flatMap(m => m.choices)).toEqual(expected);
    const blocked = prepareMentionSend(edited, roster);
    if (blocked.ok || blocked.reason !== "ambiguous") throw new Error("Expected an explicit recipient choice");
    const confirmed = pickMention(edited, blocked.match, expected[0], roster)!;
    const afterPick = prepareMentionSend(confirmed, roster);
    if (expected.length > 1) {
      expect(!afterPick.ok && afterPick.reason === "ambiguous" ? afterPick.match.choices : null).toEqual([generic]);
    } else {
      expect(body(afterPick).mentions).toEqual([generic]);
    }
  }
  const before = 'Use "done," `note`. @coder finished. Say @agent ';
  const captured = captureTypedMentions(draft(before), roster);
  expect(captured.anchors.map(a => a.mention)).toEqual([coder, generic]);
  const start = before.indexOf("@agent");
  const opened = editMentionDraft(captured, { start, end: start, replacement: '"' })!;
  const end = opened.text.length - 1;
  const edited = editMentionDraft(opened, { start: end, end: end + 1, replacement: '"' })!;
  expect(edited.text).toBe('Use "done," `note`. @coder finished. Say "@agent"');
  expect(scanMentionDraft(edited, roster).filter(m => m.kind === "ambiguous").flatMap(m => m.choices))
    .toEqual([coder, generic]);
});

test("an edit rechecks typed anchors but preserves a deliberate pick next to an interrupted local part", () => {
  const before = 'Use "local `note` @agent ';
  const after = `${before}here"@example.org`;
  const captured = captureTypedMentions(draft(before), roster);
  expect(body(prepareMentionSend(captured, roster)).mentions).toEqual([generic]);
  const edited = captureTypedMentions(updateMentionDraft(captured, after), roster);
  const blocked = prepareMentionSend(edited, roster);
  expect(!blocked.ok && blocked.reason === "ambiguous" ? blocked.match.choices : null).toEqual([generic]);
  const [match] = scanMentionDraft(edited, roster);
  const confirmed = pickMention(edited, match, generic, roster)!;
  expect(body(prepareMentionSend(confirmed, roster)).mentions).toEqual([generic]);

  const start = before.indexOf("@agent");
  const picked = pick(draft(before), start, start + 6, generic);
  const withSuffix = updateMentionDraft(picked, after);
  expect(withSuffix.anchors[0]?.picked).toBe(true);
  const sent = body(prepareMentionSend(withSuffix, roster));
  expect(sent).toEqual({ text: after, mentions: [generic], spans: [{ start, end: start + 6, index: 0 }] });

  const person = pick(draft('Use "local @al and @agent '), 11, 14, alice);
  const personAndAgent = captureTypedMentions(person, roster);
  const afterPerson = updateMentionDraft(personAndAgent, `${personAndAgent.text}here"@example.org`);
  const recipients = scanMentionDraft(afterPerson, roster).filter(m => m.kind !== "unresolved");
  expect(recipients.map(m => [m.kind, m.literal])).toEqual([["resolved", "@Alice Moreno"], ["ambiguous", "@agent"]]);
  expect(afterPerson.anchors[0]?.picked).toBe(true);
});

test("quoted addresses are recognized independently of unmatched prose quotes", () => {
  for (const text of [
    'Say "`quote`" then <"local"@agent>',
    'Say "`quote"` then <"local"@agent>',
    'Say "something unfinished, then <"local"@agent>',
    "Say 'something unfinished, then <'local'@agent>",
  ]) {
    expect(scanMentionDraft(draft(text), roster)).toEqual([]);
    expect(send(text)).toEqual({ ok: true, body: { text } });
  }
});

test("escaped local parts and multiple quoted addresses stay plain without shifting real mentions", () => {
  const text = String.raw`🌿 "unfinished prose, <"a \"quote\" @agent"@coder> and <"b \\"@agent>, then "@bo".`;
  const out = body(send(text));
  expect(out.text).toBe(text);
  expect(out.mentions).toEqual([{ kind: "user", id: "bo" }]);
  const bytes = new TextEncoder().encode(text);
  expect(out.spans?.map(s => new TextDecoder().decode(bytes.slice(s.start, s.end)))).toEqual(["@bo"]);
});

test("each adjacent quoted address ends at its own immediate separator", () => {
  for (const text of [
    'Use "first"@agent," second"@agent',
    'Use "first"@agent, " second"@agent',
    'Use "first"@coder;"second"@agent',
    'Use <"first"@agent>,<"second"@coder>',
    'Use <"first @agent"@[IPv6:2001:db8::1]>," second"@coder',
    'Use ("first"@agent),(" second"@coder)',
    String.raw`Use "first \"quote\""@agent," second"@coder`,
  ]) {
    expect(scanMentionDraft(draft(text), roster)).toEqual([]);
    expect(send(text)).toEqual({ ok: true, body: { text } });
  }
  expect(body(send('Use "first"@coder,"@agent"')).mentions).toEqual([generic]);
});

test("a prose closing quote is not the opener of an email around a full mention", () => {
  for (const text of [
    'Say "hello!" "@Alice Moreno"',
    '🌿 Say "hello?" "@Alice Moreno", then done.',
    'Say "hello,"then "@Alice Moreno".',
    'Use "first"@agent,"@Alice Moreno"',
  ]) {
    const out = body(send(text));
    expect(out.mentions).toEqual([alice]);
    const bytes = new TextEncoder().encode(text);
    expect(out.spans?.map(s => new TextDecoder().decode(bytes.slice(s.start, s.end))))
      .toEqual(["@Alice Moreno"]);
  }
});

test("an ambiguous quote boundary requires a deliberate agent choice", () => {
  const source = draft('Say "hello " then "@agent"');
  const [match] = scanMentionDraft(source, roster);
  expect(match.kind).toBe("ambiguous");
  const result = prepareMentionSend(source, roster);
  expect(result.ok ? null : result.reason).toBe("ambiguous");
  const chosen = pickMention(source, match, generic, roster)!;
  expect(body(prepareMentionSend(chosen, roster)).mentions).toEqual([generic]);

  // An @ inside the possible local part is also uncertain, even if the
  // domain itself would never resolve to anyone in the Space.
  const inside = draft('Say "hello " then @agent please "@outside');
  const [insideMatch] = scanMentionDraft(inside, roster);
  expect([insideMatch.kind, insideMatch.literal]).toEqual(["ambiguous", "@agent"]);
  const insideResult = prepareMentionSend(inside, roster);
  expect(insideResult.ok ? null : insideResult.reason).toBe("ambiguous");
});

test("a suffix edit can make an already captured agent uncertain without changing its identity", () => {
  const before = 'Say "hello " then @agent ';
  const after = `${before}please "@outside`;
  const captured = captureTypedMentions(draft(before), roster);
  expect(body(prepareMentionSend(captured, roster)).mentions).toEqual([generic]);

  const edited = captureTypedMentions(updateMentionDraft(captured, after), roster);
  const [match] = scanMentionDraft(edited, roster);
  expect(match).toEqual({
    kind: "ambiguous", start: before.indexOf("@agent"), end: before.indexOf("@agent") + 6,
    literal: "@agent", choices: [generic],
  });
  const blocked = prepareMentionSend(edited, roster);
  expect(blocked.ok ? null : blocked.reason).toBe("ambiguous");
  const chosen = pickMention(edited, match, generic, roster)!;
  expect(body(prepareMentionSend(chosen, roster)).mentions).toEqual([generic]);
  expect(body(prepareMentionSend(updateMentionDraft(edited, before), roster)).mentions).toEqual([generic]);
});

test("an uncertain typed named-agent anchor offers its pinned ID, not a replacement handle owner", () => {
  const before = 'Say "hello " then @coder ';
  const captured = captureTypedMentions(draft(before), roster);
  const edited = editMentionDraft(captured, { start: before.length, end: before.length, replacement: 'please "@outside' })!;
  const agents = [
    ...roster.agents.map(a => a.id === coder.agentId ? { ...a, handle: "renamed" } : a),
    { id: "ag-replacement", name: "Replacement", handle: "coder" },
  ];
  const directory = { ...roster, agents };
  const blocked = prepareMentionSend(edited, directory);
  expect(!blocked.ok && blocked.reason === "ambiguous" ? blocked.match.choices : null).toEqual([coder]);
  expect(prepareMentionSend(edited, { ...directory, agents: agents.filter(a => a.id !== coder.agentId) }))
    .toEqual({ ok: false, reason: "stale-recipient" });
});

test("a deliberate agent pick survives an uncertain suffix but not a definite email or stale ID", () => {
  const before = 'Say "hello " then @agent ';
  const start = before.indexOf("@agent");
  const picked = pick(draft(before), start, start + 6, generic);
  const edited = updateMentionDraft(picked, `${before}please "@outside`);
  expect(body(prepareMentionSend(edited, roster)).mentions).toEqual([generic]);

  const domain = pick(draft("@a"), 0, 2, generic);
  const email = editMentionDraft(domain, { start: 0, end: 0, replacement: `"local"` })!;
  expect(prepareMentionSend(email, roster)).toEqual({ ok: true, body: { text: '"local"@agent ' } });
  const named = pick(draft("@co"), 0, 3, coder);
  expect(prepareMentionSend(named, { ...roster, agents: [] })).toEqual({ ok: false, reason: "stale-recipient" });
});

test("wrapped and emphasized quoted email never contributes an agent recipient or shifts a real mention", () => {
  const directory = {
    ...roster, agents: [...roster.agents, { id: "ag-underscore", handle: "agent_", name: "Underscore" }],
  };
  for (const [left, right] of [
    ["", ""], ["(", ")"], ["[", "]"], ["{", "}"], ["<", ">"],
    ["*", "*"], ["**", "**"], ["***", "***"], ["_", "_"], ["__", "__"], ["(*", "*)"],
  ]) {
    for (const separator of ["", ",", ";", ".", "!"]) {
      const text = `🌿 Use ${left}"local"@agent${right}${separator} then "@Alice Moreno"`;
      const out = body(send(text, directory));
      expect(out.text).toBe(text);
      expect(out.mentions).toEqual([alice]);
      const bytes = new TextEncoder().encode(text);
      expect(out.spans?.map(s => new TextDecoder().decode(bytes.slice(s.start, s.end))))
        .toEqual(["@Alice Moreno"]);
    }
  }
});

test("quoted mail can start directly after a Markdown blockquote marker", () => {
  const directory = {
    ...roster, agents: [...roster.agents, { id: "ag-underscore", handle: "agent_", name: "Underscore" }],
  };
  for (const text of ['>"local"@agent', '>_"local"@agent_']) {
    expect(scanMentionDraft(draft(text), directory, { finalize: true })).toEqual([]);
    expect(send(text, directory)).toEqual({ ok: true, body: { text } });
    const start = text.indexOf("@");
    expect(pickMention(draft(text), { start, end: text.length, literal: text.slice(start) }, generic, directory)).toBe(null);
  }
});

test("nested blockquote prefixes preserve wrapped addresses, adjacent addresses, and UTF-8 spans", () => {
  const directory = {
    ...roster, agents: [...roster.agents,
      { id: "ag-underscore", handle: "agent_", name: "Underscore" },
      { id: "ag-double-underscore", handle: "agent__", name: "Double Underscore" },
    ],
  };
  for (const prefix of [">", ">>", "> >", "  > >>", ">\t>", "🌿 Earlier\r\n   >   >"]) {
    for (const [left, right] of [["", ""], ["_", "_"], ["__", "__"], ["*", "*"], ["(*", "*)"], ["<", ">"]]) {
      for (const suffix of ["", ',"second"@coder', ';_" second"@agent_']) {
        const text = `${prefix}${left}"local"@agent${right}${suffix}\n🌿 Say "@Alice Moreno"`;
        const mention = "@Alice Moreno";
        const start = new TextEncoder().encode(text.slice(0, text.indexOf(mention))).length;
        expect(send(text, directory)).toEqual({
          ok: true, body: { text, mentions: [alice], spans: [{ start, end: start + mention.length, index: 0 }] },
        });
      }
    }
  }
});

test("an angle bracket outside a blockquote prefix does not change mention or chooser boundaries", () => {
  for (const text of [
    'See <tag>" "@Alice Moreno"',
    'See <tag>_" "@Alice Moreno"',
    '> See <tag>" "@Alice Moreno"',
    '    >" "@Alice Moreno"',
    '\\>" "@Alice Moreno"',
    '>"@Alice Moreno"',
  ]) expect(body(send(text)).mentions).toEqual([alice]);

  const source = draft('>"@Alice Moreno"');
  const [match] = scanMentionDraft(source, roster);
  const chosen = pickMention(source, match, alice, roster)!;
  expect(chosen.anchors[0]?.picked).toBe(true);
  expect(body(prepareMentionSend(chosen, roster)).mentions).toEqual([alice]);
  const text = ">@agent";
  const selection = { start: 1, end: text.length, literal: "@agent" };
  expect(send(text)).toEqual({ ok: true, body: { text } });
  expect(pickMention(draft(text), selection, generic, roster)).toBe(null);
  expect(prepareMentionSend({ text, anchors: [{ ...selection, mention: generic, picked: true }] }, roster))
    .toEqual({ ok: false, reason: "invalid-anchor" });
});

test("an email-like invalid host stays in the chooser even after its agent changes handle", () => {
  const initial = {
    ...roster, agents: roster.agents.map(a => a.id === coder.agentId ? { ...a, handle: "_coder" } : a),
  };
  const text = 'Use "local"@_coder';
  const source = captureTypedMentions(draft(text), initial, { finalize: true });
  const [match] = scanMentionDraft(source, roster, { finalize: true });
  expect(match.kind).toBe("ambiguous");
  const chosen = pickMention(source, match, coder, roster)!;
  expect(body(prepareMentionSend(chosen, roster)).mentions).toEqual([{ kind: "agent", agent_id: coder.agentId }]);
  expect(chosen.anchors[0]?.literal).toBe("@_coder");
});

test("an incomplete or implausible address suffix does not hide an ordinary mention", () => {
  for (const text of ['Say "ask @bo now"@', 'Say "ask @bo now"@.', 'Say "ask @bo now"\n"@']) {
    expect(body(send(text)).mentions).toEqual([{ kind: "user", id: "bo" }]);
  }
  expect(body(send(' an unmatched quote before "@coder')).mentions).toEqual([{ kind: "agent", agent_id: coder.agentId }]);
  expect(body(send('Say "ask @bo now\n"@coder')).mentions)
    .toEqual([{ kind: "user", id: "bo" }, { kind: "agent", agent_id: coder.agentId }]);
  expect(body(send('Say "route <"local"@agent>" then "@bo,"')).mentions).toEqual([{ kind: "user", id: "bo" }]);
});

test("a quote inside a selected label is opaque next to a quoted address", () => {
  const dev = { kind: "user", id: "dev" } as const;
  const directory = {
    ...roster, members: [...roster.members, { id: dev.id, displayName: 'Dev "Ops' }],
  };
  const chosen = pick(draft('🌿 @d then <"local"@agent> "@bo"'), 3, 5, dev, directory);
  expect(scanMentionDraft(chosen, directory).map(m => m.literal)).toEqual(['@Dev "Ops', "@bo"]);
  expect(body(prepareMentionSend(chosen, directory))).toEqual({
    text: '🌿 @Dev "Ops then <"local"@agent> "@bo"',
    mentions: [dev, { kind: "user", id: "bo" }],
    spans: [{ start: 5, end: 14, index: 0 }, { start: 37, end: 40, index: 1 }],
  });
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

test("B-G rejects edge @ labels; explicit picks fall back to a valid ID or handle", () => {
  const directory = {
    members: [{ id: "member", displayName: "@Support" }, { id: "other", displayName: "Support@" }],
    agents: [{ id: coder.agentId, handle: "_coder", name: "@Coder" }],
  };
  const source = draft("@s");
  expect(pick(source, 0, 2, { kind: "user", id: "member" }, directory).text).toBe("@member ");
  expect(pick(source, 0, 2, { kind: "user", id: "other" }, directory).text).toBe("@other ");
  expect(pick(source, 0, 2, coder, directory).text).toBe("@_coder ");
  for (const text of ["@@Support", "@Support@"]) {
    expect(prepareMentionSend({
      text, anchors: [{ start: 0, end: text.length, literal: text, mention: { kind: "user", id: "member" } }],
    }, directory)).toEqual({ ok: false, reason: "invalid-anchor" });
  }
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

test("Ask Agent counts against the total recipient cap unless that identity is already addressed", () => {
  const members = Array.from({ length: MAX_MENTION_RECIPIENTS }, (_, i) => ({ id: `p${i}` }));
  const directory = { ...roster, members };
  const full = members.map(m => `@${m.id}`).join(" ");
  const oneShort = full.slice(0, full.lastIndexOf(" "));
  expect(prepareMentionSend(draft(full), directory, { askAgent: {} }))
    .toEqual({ ok: false, reason: "too-many-recipients" });
  expect(prepareMentionSend(draft(full), directory, { askAgent: { agentId: coder.agentId } }))
    .toEqual({ ok: false, reason: "too-many-recipients" });
  const named = body(prepareMentionSend(draft(`${oneShort} @coder`), directory, { askAgent: { agentId: coder.agentId } }));
  expect([named.mentions?.length, named.invoke_agent, named.agent_id]).toEqual([MAX_MENTION_RECIPIENTS, true, coder.agentId]);
  const duplicate = body(prepareMentionSend(draft(`${oneShort} @agent`), directory, { askAgent: {} }));
  expect([duplicate.mentions?.length, duplicate.invoke_agent, duplicate.agent_id]).toEqual([MAX_MENTION_RECIPIENTS, true, undefined]);
  expect(body(prepareMentionSend(draft("plain"), directory, { askAgent: {} })))
    .toEqual({ text: "plain", invoke_agent: true });
  expect(prepareMentionSend(draft("plain"), directory, { askAgent: { agentId: "missing" } }))
    .toEqual({ ok: false, reason: "stale-recipient" });
});

test("display-name limit counts Unicode scalars, not code units or wire bytes", () => {
  const name = "🧩".repeat(64);
  const tooLong = `${name}🧩`;
  const directory = { members: [{ id: "short", displayName: name }, { id: "long", displayName: tooLong }], agents: [] };
  expect(body(send(`@${name}`, directory)).spans).toEqual([{ start: 0, end: 257, index: 0 }]);
  expect(body(send(`@${tooLong}`, directory)).mentions).toBe(undefined);
  expect(pick(draft("@lo"), 0, 3, { kind: "user", id: "long" }, directory).text).toBe("@long ");
});
