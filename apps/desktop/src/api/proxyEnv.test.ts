import { expect, test } from "nsh/test";
import vectors from "../../../../crates/url-policy/testdata/host_vectors.json";
import { parseProxyRoute, reachedDirectly, ureqNoProxyMatches } from "./proxyEnv";
import { parseServerUrl, plainHttpWarning } from "./serverUrl";

type Doc = {
  no_proxy: { no_proxy: string; host: string; match: boolean }[];
  loopback_uri_hosts: string[];
  not_exempted_by_default: string[];
  launcher_no_proxy_entries: string[];
};
const doc = vectors as unknown as Doc;
const LAUNCHER = doc.launcher_no_proxy_entries.join(",");

test("matches NO_PROXY like ureq 3 (shared cases with metro-url-policy)", () => {
  expect(doc.no_proxy.length > 10).toBe(true);
  for (const c of doc.no_proxy) {
    if (ureqNoProxyMatches(c.no_proxy, c.host) !== c.match) throw new Error(`${JSON.stringify(c)}`);
  }
});

test("a Unicode look-alike NO_PROXY entry does not skip consent for a Tailscale host", () => {
  const r = parseProxyRoute({ METRO_PROXY_ROUTE: "proxied", METRO_PROXY_DIRECT_FOR: "\u212abox.ts.net" });
  const url = parseServerUrl("http://kbox.ts.net:8787", r);
  expect(url.ok && url.network === "tailscale" && !url.direct && url.needsConsent).toBe(true);
});

test("behind a proxy only exact listed hosts go direct: loopback, and what the person listed", () => {
  const r = parseProxyRoute({ METRO_PROXY_ROUTE: "proxied", METRO_PROXY_DIRECT_FOR: "corp.example,box.tail1234.ts.net," + LAUNCHER });
  for (const h of doc.loopback_uri_hosts) expect(reachedDirectly(h, r)).toBe(true);
  expect(reachedDirectly("box.tail1234.ts.net", r)).toBe(true);
  for (const h of doc.not_exempted_by_default.filter((x) => x !== "box.tail1234.ts.net")) {
    if (reachedDirectly(h, r)) throw new Error(`${h} would skip the proxy`);
  }
});

test("the route: direct means everything direct; proxied and unknown only NO_PROXY matches; anything else is unknown", () => {
  expect(reachedDirectly("100.64.0.1", parseProxyRoute({ METRO_PROXY_ROUTE: "direct" }))).toBe(true);
  for (const env of [
    { METRO_PROXY_ROUTE: "proxied" },
    { METRO_PROXY_ROUTE: "unknown" },
    {},
    { METRO_PROXY_ROUTE: "DIRECT" },
    { METRO_PROXY_ROUTE: "direct-ish" },
  ]) {
    const r = parseProxyRoute(env);
    expect(r.route === "direct").toBe(false);
    expect(reachedDirectly("100.64.0.1", r)).toBe(false);
    expect(reachedDirectly("127.0.0.1", r)).toBe(false);
    expect(reachedDirectly("127.0.0.1", { ...r, directFor: LAUNCHER })).toBe(true);
  }
});

test("Tailscale and loopback skip the question only when reached directly; unknown asks", () => {
  const proxied = parseProxyRoute({ METRO_PROXY_ROUTE: "proxied", METRO_PROXY_DIRECT_FOR: LAUNCHER });
  const unknown = parseProxyRoute({});
  const direct = parseProxyRoute({ METRO_PROXY_ROUTE: "direct", METRO_PROXY_DIRECT_FOR: LAUNCHER });
  for (const url of ["http://100.101.102.103:8787", "http://metro.tail1234.ts.net", "http://[fd7a:115c:a1e0::1]:8787"]) {
    for (const r of [proxied, unknown]) {
      const a = parseServerUrl(url, r);
      expect(a.ok && !a.direct && a.needsConsent).toBe(true);
    }
    const b = parseServerUrl(url, direct);
    expect(b.ok && b.direct && !b.needsConsent).toBe(true);
  }
  // Loopback: direct through the launcher's exact entries even behind a proxy; unknown without them asks.
  const lp = parseServerUrl("http://127.0.0.1:8787", proxied);
  expect(lp.ok && lp.direct && !lp.needsConsent).toBe(true);
  const lu = parseServerUrl("http://127.0.0.1:8787", unknown);
  expect(lu.ok && lu.needsConsent).toBe(true);
  // HTTPS never asks.
  const tls = parseServerUrl("https://metro.example.com", unknown);
  expect(tls.ok && !tls.needsConsent).toBe(true);
  expect(plainHttpWarning("http://100.101.102.103:8787", proxied)).toContain("goes through a proxy");
  expect(plainHttpWarning("http://192.168.1.5:8787", proxied)).toContain("between you and 192.168.1.5:8787");
});
