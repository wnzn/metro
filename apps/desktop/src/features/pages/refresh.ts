import { cleanup, effect, lifecycle, untracked } from "nsh";

/** Only for reads (including POST /data). Cancelling a read releases its
 * slot immediately, even if the transport later delivers its old result. */
export function pageReadLane<Key>() {
  type Read = { key: Key; controller: AbortController; done: Promise<void> };
  let active: Read | undefined;
  const cancel = () => {
    const previous = active;
    active = undefined;
    previous?.controller.abort();
  };
  cleanup(cancel);
  function run(key: Key, work: (signal: AbortSignal, current: () => boolean) => Promise<void>, reuse: (existing: Key) => boolean = k => k === key) {
    if (active && reuse(active.key)) return active.done;
    cancel();
    const controller = new AbortController();
    const read: Read = { key, controller, done: Promise.resolve() };
    const current = () => active === read && !controller.signal.aborted;
    active = read;
    read.done = Promise.resolve().then(() => {
      if (current()) return work(controller.signal, current);
    }).finally(() => { if (current()) active = undefined; });
    return read.done;
  }
  return { run, cancel };
}

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
