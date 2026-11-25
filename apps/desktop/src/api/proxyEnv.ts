// Whether NSH's fetch reaches a host directly or through a proxy.
//
// A plain-HTTP server counts as local (loopback) or encrypted (Tailscale)
// only while the bytes really go there directly; through a proxy they cross
// whatever network the proxy is on. So the app skips the plain-HTTP question
// for such a host only when this says the request won't use a proxy.
//
// The app never sees the proxy settings themselves (their URLs can hold a
// user name and password). The packaged launcher passes two non-secret
// values instead (crates/desktop/src/main.rs):
// - METRO_PROXY_ROUTE: `direct` (no proxy applies), `proxied` (a proxy
//   variable is set) or `unknown` (Windows without one: a system proxy may
//   apply);
// - METRO_PROXY_DIRECT_FOR: the UI's NO_PROXY, whose matches always go
//   direct. NSH 0.6.1's fetch is ureq 3, which matches NO_PROXY against the
//   URI's host as written (IPv6 in brackets): `*`, an exact host, `.suffix`
//   or `*suffix`, `prefix.` or `prefix*`. The launcher adds only this
//   machine's exact hosts, so a Tailscale server behind a proxy is direct
//   only when the person lists it in their own NO_PROXY.
// Anything missing or unrecognised (no launcher: `nsh run`, a hand-started
// UI) is `unknown`: only a NO_PROXY match counts as direct then.

import proc from "process";

export type ProxyRoute = Readonly<{
  route: "direct" | "proxied" | "unknown";
  /** NO_PROXY entries (ureq syntax) that always go direct. */
  directFor: string;
}>;

/** Pure, for tests. */
export function parseProxyRoute(env: Record<string, string | undefined>): ProxyRoute {
  const r = env.METRO_PROXY_ROUTE;
  const route = r === "direct" || r === "proxied" || r === "unknown" ? r : "unknown";
  return { route, directFor: typeof env.METRO_PROXY_DIRECT_FOR === "string" ? env.METRO_PROXY_DIRECT_FOR : "" };
}

/** ureq 3's NO_PROXY matching (see metro_url_policy::ureq_no_proxy_matches). */
export function ureqNoProxyMatches(noProxy: string, uriHost: string): boolean {
  if (noProxy === "") return false;
  const host = uriHost.toLowerCase();
  return noProxy.split(",").some((entry) => {
    const e = entry.toLowerCase();
    if (e === "*") return true;
    if (e.startsWith("*")) return host.endsWith(e.slice(1));
    if (e.startsWith(".")) return host.endsWith(e);
    if (e.endsWith("*")) return host.startsWith(e.slice(0, -1));
    if (e.endsWith(".")) return host.startsWith(e);
    return host === e;
  });
}

/** Whether a request to `uriHost` (as in a URL: IPv6 in brackets) goes there without a proxy. */
export function reachedDirectly(uriHost: string, r: ProxyRoute): boolean {
  return r.route === "direct" || ureqNoProxyMatches(r.directFor, uriHost);
}

/** This process's route, read once (the launcher sets it before the UI starts). */
let current: ProxyRoute | null = null;
export function proxyRoute(): ProxyRoute {
  if (current) return current;
  let env: Record<string, string> = {};
  try {
    env = proc.env ?? {};
  } catch {
    env = {};
  }
  current = parseProxyRoute(env);
  return current;
}
