// Whether NSH's fetch reaches a host directly or through a proxy.
//
// A plain-HTTP server counts as local (loopback) or encrypted (Tailscale)
// only while the bytes really go there directly; through a proxy they cross
// whatever network the proxy is on. So the app skips the plain-HTTP question
// for such a host only when this says the request won't use a proxy.
//
// NSH 0.6.1's fetch is ureq 3 (3.4.2): it takes one proxy from ALL_PROXY,
// HTTPS_PROXY or HTTP_PROXY (or their lowercase forms) and skips it for hosts
// matched by NO_PROXY (or no_proxy, when NO_PROXY is unset), matched against
// the URI's host as written (IPv6 in brackets): `*`, an exact host, `.suffix`
// or `*suffix`, `prefix.` or `prefix*`. No CIDR ranges. On Windows it may also
// use the system proxy, which the app can't see, so there only a NO_PROXY
// match counts as direct. The packaged launcher puts loopback and the
// Tailscale ranges into the UI's NO_PROXY (metro_url_policy::
// direct_no_proxy_entries), so normally every such host matches.
//
// The proxy variables' values are only ever tested for being set: never
// shown, stored or logged (they can hold a user name and password).

import proc from "process";

export const PROXY_VARS = ["ALL_PROXY", "all_proxy", "HTTPS_PROXY", "https_proxy", "HTTP_PROXY", "http_proxy"] as const;

export type ProxyEnv = Readonly<{
  /** Some proxy variable is set. */
  proxied: boolean;
  /** The NO_PROXY value ureq reads. */
  noProxy: string;
  /** Windows: a system proxy the app can't see may apply. */
  windows: boolean;
}>;

/** Pure, for tests. */
export function parseProxyEnv(env: Record<string, string | undefined>, platform: string): ProxyEnv {
  const proxied = PROXY_VARS.some((k) => typeof env[k] === "string" && env[k] !== "");
  const noProxy = env.NO_PROXY !== undefined ? env.NO_PROXY : env.no_proxy ?? "";
  return { proxied, noProxy, windows: platform === "windows" };
}

/** ureq 3's NO_PROXY matching (see metro_url_policy::ureq_no_proxy_matches). */
export function ureqNoProxyMatches(noProxy: string, uriHost: string): boolean {
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
export function reachedDirectly(uriHost: string, env: ProxyEnv): boolean {
  if (ureqNoProxyMatches(env.noProxy, uriHost)) return true;
  return !env.proxied && !env.windows;
}

/** This process's proxy settings, read once (the launcher sets them before the UI starts). */
let current: ProxyEnv | null = null;
export function proxyEnv(): ProxyEnv {
  if (current) return current;
  let env: Record<string, string> = {};
  let platform = "";
  try {
    env = proc.env ?? {};
    platform = proc.platform ?? "";
  } catch {
    env = {};
  }
  current = parseProxyEnv(env, platform);
  return current;
}
