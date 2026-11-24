import { signal, Storage, untracked } from "nsh";

/** A panel's width range, in points: where it starts, and how far a resize may take it. */
export type PanelSize = { initial: number; min: number; max: number };

/** A side panel: whether it is open, and (resizable ones) its width. */
export type LayoutPanel = ReturnType<typeof panel>;

/** Device-local layout only. Store each panel separately so toggling one
 * never changes another. An early click (or resize) wins over an
 * outstanding read. The width is stored apart from the collapsed state, so
 * collapsing and reopening a panel keeps the width the user chose. */
function panel(name: string, size: PanelSize = { initial: 0, min: 0, max: 0 }) {
  const key = `metro.layout.${name}`;
  const widthKey = `${key}.width`;
  const open = signal(true);
  const width = signal(size.initial);
  let edited = false;
  let resized = false;
  let writes = Promise.resolve();
  const write = (k: string, v: string) => {
    writes = writes.then(() => Storage.setItem(k, v)).catch(() => {});
  };
  const clamp = (w: number) => Math.round(Math.min(size.max, Math.max(size.min, w)));
  void Storage.getItem(key).then(value => {
    if (!edited && value === "collapsed") open.set(false);
  }).catch(() => {});
  if (size.initial > 0) {
    void Storage.getItem(widthKey).then(value => {
      const saved = Number(value);
      if (!resized && value && Number.isFinite(saved)) width.set(clamp(saved));
    }).catch(() => {});
  }
  const setOpen = (next: boolean) => {
    edited = true;
    open.set(next);
    write(key, next ? "expanded" : "collapsed");
  };
  return {
    open,
    /** The width it is drawn at (points), within `size`. */
    width,
    size,
    toggle() {
      setOpen(!untracked(open));
    },
    collapse() {
      if (untracked(open)) setOpen(false);
    },
    /** Sets the width, clamped to its range; `persist: false` while a drag is still going on. */
    setWidth(w: number, persist = true) {
      resized = true;
      const next = clamp(w);
      width.set(next);
      if (persist) write(widthKey, String(next));
    },
    /** Back to its default width. */
    resetWidth() {
      resized = true;
      width.set(size.initial);
      write(widthKey, String(size.initial));
    },
  };
}

export const navigationPanel = panel("navigation", { initial: 256, min: 200, max: 400 });
export const runsPanel = panel("runs", { initial: 340, min: 260, max: 520 });
export const settingsPanel = panel("settings-sections", { initial: 228, min: 180, max: 320 });
