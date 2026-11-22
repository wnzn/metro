// How much of a step's output is drawn. Folded output shows a few lines;
// opened, it shows every line up to a bound, so a huge result never stalls
// the transcript. Copy always takes the whole text. Domain logic only.

/** Lines a folded output shows. */
export const FOLD_LINES = 6;
/** Opened output draws at most this many lines … */
export const MAX_SHOWN_LINES = 2000;
/** … and this many characters. */
export const MAX_SHOWN_CHARS = 200_000;

export type OutputView = {
  /** The text to draw. */
  shown: string;
  /** Lines in the whole output. */
  lineCount: number;
  /** Long enough to fold. */
  foldable: boolean;
  /** Opened, but cut at the bound: only `shown` is drawn, Copy still has it all. */
  bounded: boolean;
  /** The fold control's label, or null when there's nothing to fold. */
  toggleLabel: string | null;
  /** Why the opened view stops early, or null. */
  notice: string | null;
};

const plural = (n: number, one: string, many: string) => `${n.toLocaleString("en-US")} ${n === 1 ? one : many}`;

/** `copyable`: a Copy control sits beside it, so the notice can point to it. */
export function outputView(text: string, open: boolean, copyable = true): OutputView {
  const lines = text.split("\n");
  const lineCount = lines.length;
  const foldable = lineCount > FOLD_LINES + 2 || text.length > 1200;
  let shown = text;
  let bounded = false;
  if (open && (lineCount > MAX_SHOWN_LINES || text.length > MAX_SHOWN_CHARS)) {
    shown = lines.slice(0, MAX_SHOWN_LINES).join("\n").slice(0, MAX_SHOWN_CHARS);
    bounded = true;
  }
  const toggleLabel = !foldable ? null : open ? "Show less" : lineCount > MAX_SHOWN_LINES ? `Show first ${plural(MAX_SHOWN_LINES, "line", "lines")}` : `Show all ${plural(lineCount, "line", "lines")}`;
  const notice = bounded ? `Showing part of ${plural(lineCount, "line", "lines")}.${copyable ? " Copy includes all of it." : ""}` : null;
  return { shown, lineCount, foldable, bounded, toggleLabel, notice };
}
