import { expect, test } from "nsh/test";
import vectors from "../../../../crates/url-policy/testdata/host_vectors.json";
import { classifyHost, isLoopbackHost, parseServerUrl, plainHttpWarning } from "./serverUrl";
import type { ProxyRoute } from "./proxyEnv";

/** As the launcher passes it when no proxy applies (proxyEnv.test.ts covers the others). */
const DIRECT: ProxyRoute = { route: "direct", directFor: "" };

test("plain http works for every host; only hosts outside loopback and Tailscale need consent", () => {
  for (const quiet of [
    "http://127.0.0.1:8787", "http://localhost:8787/", "http://[::1]:8787", "http://127.10.0.3", "https://metro.example.com",
    "http://100.64.0.1", "http://100.127.255.254:8787", "http://[fd7a:115c:a1e0::1]:8787", "http://metro.tail1234.ts.net",
    "https://192.168.1.5", "https://metro.tail1234.ts.net",
  ]) {
    const r = parseServerUrl(quiet, DIRECT);
    expect(r.ok).toBe(true);
    if (r.ok) expect(r.needsConsent).toBe(false);
  }
  for (const asks of [
    "http://192.168.1.5:8787", "http://metro.example.com", "http://10.0.0.1", "http://127.0.0.1.evil.com", "http://[::ffff:c0a8:1]",
    "http://100.63.255.255", "http://100.128.0.1", "http://[fd7a:115c:a1e1::1]", "http://ts.net", "http://evil-ts.net", "http://metro.ts.net.evil.com",
  ]) {
    const r = parseServerUrl(asks, DIRECT);
    expect(r.ok).toBe(true);
    if (r.ok) expect(r.needsConsent).toBe(true);
  }
});

test("normalizes and rejects malformed URLs", () => {
  expect(parseServerUrl(" http://127.0.0.1:8787/ ", DIRECT)).toEqual({ ok: true, baseUrl: "http://127.0.0.1:8787", loopback: true, network: "loopback", direct: true, needsConsent: false });
  expect(parseServerUrl("https://metro.example.com/api/", DIRECT)).toEqual({ ok: true, baseUrl: "https://metro.example.com/api", loopback: false, network: "other", direct: true, needsConsent: false });
  expect(parseServerUrl("http://100.101.102.103:8787/", DIRECT)).toEqual({ ok: true, baseUrl: "http://100.101.102.103:8787", loopback: false, network: "tailscale", direct: true, needsConsent: false });
  expect(parseServerUrl("http://192.168.1.5:8787", DIRECT)).toEqual({ ok: true, baseUrl: "http://192.168.1.5:8787", loopback: false, network: "other", direct: true, needsConsent: true });
  expect(parseServerUrl("").ok).toBe(false);
  expect(parseServerUrl("127.0.0.1:8787").ok).toBe(false);
  expect(parseServerUrl("ftp://127.0.0.1").ok).toBe(false);
  expect(parseServerUrl("https://user:pw@metro.example.com").ok).toBe(false);
  expect(parseServerUrl("http://user:pw@192.168.1.5").ok).toBe(false);
  expect(parseServerUrl("http://metro.tail1234.ts.net/?x=1").ok).toBe(false);
  expect(parseServerUrl("https://metro.example.com/?x=1").ok).toBe(false);
  expect(parseServerUrl("http://192.168.1.5/#t").ok).toBe(false);
});

test("recognizes loopback forms", () => {
  expect(isLoopbackHost("LOCALHOST")).toBe(true);
  expect(isLoopbackHost("[::1]")).toBe(true);
  expect(isLoopbackHost("::ffff:127.0.0.1")).toBe(true);
  expect(isLoopbackHost("::ffff:7f00:1")).toBe(true);
  expect(isLoopbackHost("0:0:0:0:0:0:0:1")).toBe(true);
  expect(isLoopbackHost("128.0.0.1")).toBe(false);
  expect(isLoopbackHost("localhost.example.com")).toBe(false);
});

test("classifies Tailscale ranges and names exactly", () => {
  expect(classifyHost("100.64.0.0")).toBe("tailscale");
  expect(classifyHost("100.127.255.255")).toBe("tailscale");
  expect(classifyHost("100.63.255.255")).toBe("other");
  expect(classifyHost("100.128.0.0")).toBe("other");
  expect(classifyHost("[fd7a:115c:a1e0::53]")).toBe("tailscale");
  expect(classifyHost("FD7A:115C:A1E0:AB12:4843:CD96:6258:B240")).toBe("tailscale");
  expect(classifyHost("fd7a:115c:a1e1::1")).toBe("other");
  expect(classifyHost("fd7a:115c::1")).toBe("other");
  expect(classifyHost("::ffff:100.100.100.100")).toBe("tailscale");
  expect(classifyHost("::ffff:6464:6464")).toBe("tailscale");
  expect(classifyHost("Host.Tailnet-Name.TS.NET")).toBe("tailscale");
  expect(classifyHost("host.tailnet.ts.net.")).toBe("tailscale");
  expect(classifyHost("a.ts.net")).toBe("tailscale");
  expect(classifyHost("a..ts.net")).toBe("other");
  expect(classifyHost("100.64.0.1.")).toBe("other");
  expect(classifyHost("localhost.")).toBe("other");
  expect(classifyHost("foo.localhost")).toBe("other");
  expect(classifyHost("ts.net")).toBe("other");
  expect(classifyHost(".ts.net")).toBe("other");
  expect(classifyHost("evilts.net")).toBe("other");
  expect(classifyHost("host.ts.net.example.com")).toBe("other");
  expect(classifyHost("192.168.1.5")).toBe("other");
  expect(classifyHost("1:2:3:4:5:6:7:8:9")).toBe("other");
  expect(classifyHost("fd7a:115c:a1e0::1::2")).toBe("other");
  expect(classifyHost("")).toBe("other");
});

test("the warning names the host and the risk, plainly", () => {
  expect(plainHttpWarning("http://192.168.1.5:8787/metro")).toBe(
    "This connection isn't encrypted. Anyone on the network between you and 192.168.1.5:8787 could read your sign-in token. Use it on networks you trust, such as Tailscale or a VPN.",
  );
});

test("matches the shared host vectors (crates/url-policy/testdata/host_vectors.json)", () => {
  const hosts = (vectors as { hosts: { host: string; class: string }[] }).hosts;
  expect(hosts.length > 40).toBe(true);
  for (const v of hosts) {
    const got = classifyHost(v.host);
    if (got !== v.class) throw new Error(`${JSON.stringify(v.host)}: ${got}, expected ${v.class}`);
  }
});

test("a URL is classified by the host the app will connect to", () => {
  // WHATWG URL parsing turns 100.64.0.1. into the IP literal, so the
  // request goes straight to that address; localhost. stays a name.
  const ts = parseServerUrl("http://100.64.0.1.:8787", DIRECT);
  expect(ts.ok && ts.baseUrl === "http://100.64.0.1:8787" && ts.network === "tailscale").toBe(true);
  const name = parseServerUrl("http://localhost.:8787");
  expect(name.ok && name.network === "other" && name.needsConsent).toBe(true);
  const sub = parseServerUrl("http://foo.localhost:8787");
  expect(sub.ok && sub.needsConsent).toBe(true);
});
