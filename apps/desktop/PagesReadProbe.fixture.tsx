import { Storage } from "nsh";
import "./App";

// Record only cancellation of read paths. The real app calls the original
// fetch with the test server's delayed responses and real AbortSignals.
const fetchOriginal = globalThis.fetch;
const counts = new Map<string, number>();
const starts = new Map<string, number>();
const observed = new WeakMap<AbortSignal, string>();
const abortOriginal = AbortController.prototype.abort;
AbortController.prototype.abort = function (reason?: unknown) {
  const key = observed.get(this.signal);
  if (key && !this.signal.aborted) {
    const count = (counts.get(key) ?? 0) + 1;
    counts.set(key, count);
    void Storage.setItem(`pages-abort:${key}`, String(count));
  }
  abortOriginal.call(this, reason);
};
globalThis.fetch = (input, init) => {
  if (typeof input === "string") {
    const path = new URL(input).pathname, method = init?.method ?? "GET";
    if (init?.signal && path.startsWith("/v1/pages/") && (method === "GET" || path.endsWith("/data"))) {
      const key = `${method}:${path}`;
      observed.set(init.signal, key);
      const count = (starts.get(key) ?? 0) + 1;
      starts.set(key, count);
      void Storage.setItem(`pages-read:${key}`, String(count));
    }
  }
  return fetchOriginal(input, init);
};
