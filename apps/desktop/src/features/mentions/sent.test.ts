import { expect, test } from "nsh/test";
import type { RoomEvent } from "../../api/client";
import { copyMentionText, readMentionSpans, sentMentions } from "./sent";

test("wire spans use UTF-8 boundaries and original recipient positions", () => {
  const text = "😀 @Bób.";
  const indexes = new Map([[2, 0]]);
  const good = [{ start: 5, end: 10, index: 2 }];
  expect(readMentionSpans(good, text, indexes)).toEqual([{ start: 5, end: 10, index: 0 }]);
  for (const span of [
    { start: 1, end: 10, index: 2 }, // inside the emoji
    { start: 5, end: 8, index: 2 }, // inside ó
    { start: 5, end: 10, index: 0 }, // unrecognized wire recipient
    { start: 5, end: 10, index: "2" },
  ]) expect(readMentionSpans([span], text, indexes)).toEqual([]);
  expect(readMentionSpans([...good, ...good], text, indexes)).toEqual([]);
});

test("legacy rendering binds only exact stored identities, not nearby text or attributed agents", () => {
  const e: RoomEvent = {
    id: 1, kind: "message", actor: "Sender",
    text: '@Bob, @bobby, @coder. ` @bob ` and <"a"@bob> then @agent and @carol.',
    mentions: [{ kind: "user", id: "bob" }, { kind: "agent", agentId: "ag-1", legacyHandle: "coder" }, { kind: "agent", addressedBy: "classifier" }],
  };
  const pieces = sentMentions(e);
  expect(pieces.map(p => p.literal)).toEqual(["@Bob", "@coder"]);
  expect(copyMentionText(e.text, pieces, p => p.index === 0 ? "@Bob Tan" : "@Refactor Coach"))
    .toBe('@Bob Tan, @bobby, @Refactor Coach. ` @bob ` and <"a"@bob> then @agent and @carol.');
  expect(sentMentions({ ...e, spans: [] })).toEqual([]);
  expect(sentMentions({ ...e, mentions: [{ kind: "agent", agentId: "ag-1" }] })).toEqual([]);
  expect(sentMentions({ ...e, mentions: [] })).toEqual([]);
});
