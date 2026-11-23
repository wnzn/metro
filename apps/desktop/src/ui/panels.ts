import { signal, Storage } from "nsh";

/** Device-local layout only. Store each panel separately so toggling one
 * never changes another. An early click wins over an outstanding read. */
function panel(name: string) {
  const key = `metro.layout.${name}`;
  const open = signal(true);
  let edited = false;
  let writes = Promise.resolve();
  void Storage.getItem(key).then(value => {
    if (!edited && value === "collapsed") open.set(false);
  }).catch(() => {});
  return {
    open,
    toggle() {
      edited = true;
      const next = !open();
      open.set(next);
      writes = writes.then(() => Storage.setItem(key, next ? "expanded" : "collapsed")).catch(() => {});
    },
  };
}

export const navigationPanel = panel("navigation");
export const runsPanel = panel("runs");
export const settingsPanel = panel("settings-sections");
