import { expect, test } from "nsh/test";
import { FOLD_LINES, MAX_SHOWN_LINES, outputView } from "./output";

const lines = (n: number) => Array.from({ length: n }, (_, i) => `line ${i + 1}`).join("\n");

test("short output neither folds nor offers a toggle", () => {
  const v = outputView(lines(FOLD_LINES + 2), false);
  expect(v.foldable).toBe(false);
  expect(v.toggleLabel).toBe(null);
  expect(v.shown).toBe(lines(FOLD_LINES + 2));
});

test("long output folds with 'Show all N lines' and opens to every line", () => {
  expect(outputView(lines(24), false).toggleLabel).toBe("Show all 24 lines");
  const open = outputView(lines(24), true);
  expect(open.toggleLabel).toBe("Show less");
  expect(open.shown).toBe(lines(24));
  expect(open.notice).toBe(null);
  // One very long line folds too.
  expect(outputView("x".repeat(1300), false).foldable).toBe(true);
});

test("huge output is bounded once opened, and says that Copy has it all", () => {
  const text = lines(MAX_SHOWN_LINES + 500);
  expect(outputView(text, false).toggleLabel).toBe("Show first 2,000 lines");
  const v = outputView(text, true);
  expect(v.bounded).toBe(true);
  expect(v.shown.split("\n")).toHaveLength(MAX_SHOWN_LINES);
  expect(v.notice).toBe("Showing part of 2,500 lines. Copy includes all of it.");
  // Without a Copy control beside it, the notice doesn't point to one.
  expect(outputView(text, true, false).notice).toBe("Showing part of 2,500 lines.");
  // A single enormous line is cut by characters.
  expect(outputView("y".repeat(500_000), true).shown.length).toBe(200_000);
});
