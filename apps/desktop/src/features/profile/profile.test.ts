// Pure pieces behind profiles and the transcript's times: wire parsing, the
// picture check, initials, local time formatting in the real NSH runtime, and
// the transcript's day lines and continuation rows.

import { expect, test } from "nsh/test";
import { toCreatedAt } from "../../api/client";
import type { RoomEvent } from "../../api/client";
import { parseProfile, sniffAvatar } from "../../api/profile";
import { groupTranscript } from "../activity/group";
import { clockFor, dayLabel, fullStamp, shortTime, stampOf } from "../chatTime";
import { transcriptRows } from "../transcriptRows";
import { initialsOf, personTone } from "./identity";
import { BOB_PNG } from "./test/fixtures";

test("created_at: RFC 3339 strings only; null, numbers, epoch 0 and junk are 'unknown', never now", () => {
  expect(toCreatedAt("2026-11-22T03:04:05.123Z")).toBe("2026-11-22T03:04:05.123Z");
  expect(toCreatedAt("2026-11-22T11:04:05+08:00")).toBe("2026-11-22T03:04:05.000Z");
  expect(toCreatedAt(null)).toBe(undefined);
  expect(toCreatedAt(undefined)).toBe(undefined);
  expect(toCreatedAt(1795358797256)).toBe(undefined);
  expect(toCreatedAt("1970-01-01T00:00:00Z")).toBe(undefined);
  expect(toCreatedAt("yesterday")).toBe(undefined);
  expect(toCreatedAt("2026-11-22 03:04:05")).toBe(undefined);
  expect(toCreatedAt("2026-13-45T99:99:99Z")).toBe(undefined);
});

test("profiles parse as documented; a null bio is empty; a malformed answer is refused", () => {
  expect(parseProfile({ id: "bob", display_name: "Bob Tan", bio: null, avatar_version: "v3" })).toEqual({ id: "bob", displayName: "Bob Tan", bio: "", avatarVersion: "v3" });
  expect(parseProfile({ id: "a", display_name: "A", bio: "hi", avatar_version: null })!.avatarVersion).toBe(null);
  expect(parseProfile({ id: "a", display_name: "A", bio: "hi", avatar_version: "" })!.avatarVersion).toBe(null);
  expect(parseProfile({ display_name: "no id" })).toBe(null);
  expect(parseProfile({ id: "a", display_name: 7 })).toBe(null);
  expect(parseProfile([])).toBe(null);
});

test("a picture is checked by its bytes: PNG and JPEG only", () => {
  expect(sniffAvatar(BOB_PNG)).toBe("image/png");
  expect(sniffAvatar(new Uint8Array([0xff, 0xd8, 0xff, 0xe0, 0, 0x10]))).toBe("image/jpeg");
  expect(sniffAvatar(new TextEncoder().encode("<svg xmlns='http://www.w3.org/2000/svg'/>"))).toBe(null);
  expect(sniffAvatar(new TextEncoder().encode("GIF89a"))).toBe(null);
  expect(sniffAvatar(new Uint8Array([]))).toBe(null);
});

test("initials and tones come from names and stable ids", () => {
  expect(initialsOf("Ada Lovelace")).toBe("AL");
  expect(initialsOf("bob")).toBe("B");
  expect(initialsOf("  maría  josé garcía ")).toBe("MG");
  expect(initialsOf("ops-bot")).toBe("OB");
  expect(initialsOf("")).toBe("?");
  expect(initialsOf("🙂")).toBe("?");
  expect(personTone("bob")).toBe(personTone("bob"));
  expect(personTone("bob")).not.toBe("agent");
  expect(personTone("bob")).not.toBe("danger");
});

const utc12 = clockFor({ timeZone: "UTC", hourCycle: "h12", languages: ["en-US"] });
const utc23 = clockFor({ timeZone: "Etc/UTC", hourCycle: "h23", languages: ["en-US"] });

test("times format in the named zone with the user's hour cycle (real NSH Intl)", () => {
  const ms = Date.parse("2026-11-22T15:04:05Z");
  expect(utc12.zone).toBe("UTC");
  expect(utc12.utcFallback).toBe(false);
  expect(shortTime(ms, utc12)).toBe("3:04 PM");
  expect(shortTime(ms, utc23)).toBe("15:04");
  expect(dayLabel(ms, utc12)).toBe("Sunday, November 22, 2026");
  expect(fullStamp(ms, utc12)).toBe("Sunday, November 22, 2026, 3:04:05 PM (UTC)");
  // A zone this runtime can't format falls back to UTC, and says so.
  const unknown = clockFor({ timeZone: "Mars/Olympus_Mons", hourCycle: "h23" });
  expect(unknown.zone).toBe("UTC");
  expect(unknown.utcFallback).toBe(true);
  expect(shortTime(ms, unknown)).toBe("15:04 UTC");
  expect(stampOf(undefined, utc12)).toBe(null);
  expect(stampOf("2026-11-22T15:04:05.000Z", utc12)!.day).toBe("2026-11-22");
});

const ev = (id: number, over: Partial<RoomEvent>): RoomEvent => ({ id, kind: "message", actor: "x", text: `t${id}`, ...over });

test("a date line starts each day; same-author messages within minutes continue; undated rows get no time", () => {
  const events: RoomEvent[] = [
    ev(1, { actor: "bob" }), // legacy: no id, no time
    ev(2, { actor: "Bob Tan", actorId: "bob", createdAt: "2026-11-21T23:50:00.000Z" }),
    ev(3, { actor: "Alice", actorId: "alice", createdAt: "2026-11-22T01:02:03.000Z" }),
    ev(4, { actor: "Alice", actorId: "alice", createdAt: "2026-11-22T01:03:00.000Z" }),
    ev(5, { actor: "Alice", actorId: "alice", createdAt: "2026-11-22T01:20:00.000Z" }),
    ev(6, { kind: "agent", actor: "agent", createdAt: "2026-11-22T01:21:00.000Z" }),
    ev(7, { kind: "error", actor: "system", createdAt: "2026-11-22T01:21:30.000Z" }),
    ev(8, { kind: "error", actor: "system", createdAt: "2026-11-22T01:21:40.000Z" }),
  ];
  const rows = transcriptRows(groupTranscript(events), events, utc12);
  const shape = rows.map((r) => (r.type === "day" ? `day ${r.label}` : r.type === "event" ? `${r.event.id}${r.continued ? "+" : ""} ${r.stamp?.short ?? "-"}` : "group"));
  expect(shape).toEqual([
    "1 -",
    "day Saturday, November 21, 2026",
    "2 11:50 PM",
    "day Sunday, November 22, 2026",
    "3 1:02 AM",
    "4+ 1:03 AM",
    "5 1:20 AM", // more than 5 minutes later: a new header
    "6 1:21 AM",
    "7 1:21 AM",
    "8 1:21 AM", // errors never fold into the previous one
  ]);
  // Keys are unique and stable.
  expect(new Set(rows.map((r) => r.key)).size).toBe(rows.length);
  expect(transcriptRows(groupTranscript(events), events, utc12).map((r) => r.key)).toEqual(rows.map((r) => r.key));
});
