import { effect, lifecycle, untracked } from "nsh";

export function intervalFor(value: unknown): number {
  switch (value) {
    case "30s": return 30_000;
    case "1m": return 60_000;
    case "5m": return 300_000;
    case "15m": return 900_000;
    case "1h": return 3_600_000;
    default: return 0;
  }
}

/** An owned interval. Remove it while the view is covered, unmounted or the
 * window is inactive, and check again at the actual callback boundary. */
export function visiblePolling(enabled: () => boolean, period: () => number, poll: () => void, immediate = false) {
  const visible = () => lifecycle() === "active" && enabled();
  effect(() => {
    if (!visible()) return;
    const ms = period();
    if (!ms) return;
    const tick = () => { if (untracked(visible)) untracked(poll); };
    if (immediate) tick();
    const timer = setInterval(tick, ms);
    return () => clearInterval(timer);
  });
}
