// Result highlights: only the query's own words are marked, never invented.

import { expect, test } from "nsh/test";
import { markTerms } from "./searchState";

const marked = (text: string, query: string) => markTerms(text, query).filter((p) => p.mark).map((p) => p.text);

test("marks the query's words wherever they appear, case-insensitively, and keeps the whole text", () => {
  const parts = markTerms("The Rollback plan: revert, then roll back again.", "rollback plan");
  expect(parts.map((p) => p.text).join("")).toBe("The Rollback plan: revert, then roll back again.");
  expect(marked("The Rollback plan: revert, then roll back again.", "rollback plan")).toEqual(["Rollback", "plan"]);
});

test("short and filler words mark nothing; a match found only by meaning stays unmarked", () => {
  expect(marked("What powered the expedition after sunset?", "what is the")).toEqual([]);
  expect(marked("Portable solar cells charged the batteries.", "What powered the expedition after sunset?")).toEqual([]);
  expect(markTerms("", "anything")).toEqual([{ text: "", mark: false }]);
});

test("regex characters in a query are taken literally", () => {
  expect(marked("Costs (est.) rose", "(est.)")).toEqual(["est"]);
  expect(marked("a+b=c", "a+b")).toEqual([]);
});
