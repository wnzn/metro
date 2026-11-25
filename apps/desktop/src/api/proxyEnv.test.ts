import { expect, test } from "nsh/test";
import vectors from "../../../../crates/url-policy/testdata/host_vectors.json";
import { parseProxyEnv, reachedDirectly, ureqNoProxyMatches } from "./proxyEnv";
import { parseServerUrl, plainHttpWarning } from "./serverUrl";

type Doc = {
  no_proxy: { no_proxy: string; host: string; match: boolean }[];
  direct_uri_hosts: string[];
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

test("the launcher's NO_PROXY reaches every canonical loopback and Tailscale host directly, even with a proxy set", () => {
  const env = parseProxyEnv({ HTTP_PROXY: "http://proxy.invalid:3128", NO_PROXY: "corp.example," + LAUNCHER }, "linux");
  for (const h of doc.direct_uri_hosts) expect(reachedDirectly(h, env)).toBe(true);
  for (const h of ["192.168.1.5", "metro.example.com", "[::ffff:6464:6464]"]) expect(reachedDirectly(h, env)).toBe(false);
});

test("proxy variables: any of the six counts; NO_PROXY wins over no_proxy; Windows needs a NO_PROXY match", () => {
  for (const k of ["ALL_PROXY", "all_proxy", "HTTPS_PROXY", "https_proxy", "HTTP_PROXY", "http_proxy"]) {
    expect(parseProxyEnv({ [k]: "http://p:1" }, "linux").proxied).toBe(true);
  }
  expect(parseProxyEnv({ HTTP_PROXY: "" }, "linux").proxied).toBe(false);
  expect(parseProxyEnv({ NO_PROXY: "", no_proxy: "a" }, "linux").noProxy).toBe("");
  expect(parseProxyEnv({ no_proxy: "a" }, "linux").noProxy).toBe("a");
  expect(reachedDirectly("100.64.0.1", parseProxyEnv({}, "linux"))).toBe(true);
  expect(reachedDirectly("100.64.0.1", parseProxyEnv({}, "windows"))).toBe(false);
  expect(reachedDirectly("100.64.0.1", parseProxyEnv({ NO_PROXY: LAUNCHER }, "windows"))).toBe(true);
});

test("Tailscale and loopback skip the question only when reached directly", () => {
  const proxied = parseProxyEnv({ HTTPS_PROXY: "http://proxy.invalid:3128" }, "linux");
  const launched = parseProxyEnv({ HTTPS_PROXY: "http://proxy.invalid:3128", NO_PROXY: LAUNCHER }, "linux");
  for (const url of ["http://100.101.102.103:8787", "http://metro.tail1234.ts.net", "http://[fd7a:115c:a1e0::1]:8787", "http://127.0.0.1:8787"]) {
    const a = parseServerUrl(url, proxied);
    expect(a.ok && !a.direct && a.needsConsent).toBe(true);
    const b = parseServerUrl(url, launched);
    expect(b.ok && b.direct && !b.needsConsent).toBe(true);
  }
  // An IPv4-mapped Tailscale address isn't in the launcher's list: asked about.
  const mapped = parseServerUrl("http://[::ffff:6464:6464]:8787", launched);
  expect(mapped.ok && mapped.network === "tailscale" && mapped.needsConsent).toBe(true);
  // HTTPS never asks, proxy or not.
  const tls = parseServerUrl("https://metro.example.com", proxied);
  expect(tls.ok && !tls.needsConsent).toBe(true);
  expect(plainHttpWarning("http://100.101.102.103:8787", proxied)).toContain("goes through a proxy");
  expect(plainHttpWarning("http://192.168.1.5:8787", proxied)).toContain("between you and 192.168.1.5:8787");
});
