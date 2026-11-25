// Validates the server URL the user types before any token is sent to it.
//
// Rules:
// - only http: and https: URLs, without embedded credentials, query or fragment;
// - plain http: works for every host, but the host decides how:
//   - loopback (localhost, 127.0.0.0/8, ::1) and Tailscale (100.64.0.0/10,
//     fd7a:115c:a1e0::/48, names ending in .ts.net) are used without asking:
//     loopback never leaves this computer, and Tailscale encrypts the traffic
//     between devices itself;
//   - any other host needs the person's informed, one-time confirmation for
//     that exact server address first (`needsConsent`), because a bearer
//     token would cross the network in clear text. Nothing is ever blocked.
// - the result is the origin plus an optional base path, without a trailing slash.

export const DEFAULT_SERVER_URL = "http://127.0.0.1:8787";

/** Where a host is, as far as an unencrypted connection is concerned. */
export type HostClass = "loopback" | "tailscale" | "other";

export type ServerUrlResult =
  | {
      ok: true;
      baseUrl: string;
      loopback: boolean;
      /** The host's class (see `classifyHost`). */
      network: HostClass;
      /** Plain http to a host that is neither loopback nor Tailscale: ask once before sending a token. */
      needsConsent: boolean;
    }
  | { ok: false; error: string };

function bareHost(hostname: string): string {
  const host = hostname.toLowerCase();
  return host.startsWith("[") && host.endsWith("]") ? host.slice(1, -1) : host;
}

const LABEL = /^[a-z0-9](?:[a-z0-9-]*[a-z0-9])?$/;

/** `<label>[.<label>...].ts.net` (lowercase input). */
function isTsNetName(name: string): boolean {
  if (!name.endsWith(".ts.net")) return false;
  const labels = name.slice(0, -".ts.net".length);
  return labels !== "" && labels.split(".").every((l) => LABEL.test(l));
}

function ipv4(host: string): number[] | null {
  const m = /^(\d{1,3})\.(\d{1,3})\.(\d{1,3})\.(\d{1,3})$/.exec(host);
  if (!m) return null;
  const octets = m.slice(1).map(Number);
  return octets.every((n) => n <= 255) ? octets : null;
}

/** The eight 16-bit groups of an IPv6 address (IPv4 tail allowed), or null. */
function ipv6(host: string): number[] | null {
  if (!host.includes(":") || !/^[0-9a-f:.]+$/.test(host)) return null;
  let text = host;
  const tail: number[] = [];
  const cut = text.lastIndexOf(":");
  const last = text.slice(cut + 1);
  if (last.includes(".")) {
    const v4 = ipv4(last);
    if (!v4) return null;
    tail.push((v4[0] << 8) | v4[1], (v4[2] << 8) | v4[3]);
    text = text.slice(0, cut);
    if (text.endsWith(":")) text += ":";
  }
  const halves = text.split("::");
  if (halves.length > 2) return null;
  const parse = (s: string) => (s === "" ? [] : s.split(":"));
  const head = parse(halves[0]);
  const rest = halves.length === 2 ? parse(halves[1]) : [];
  if ([...head, ...rest].some((g) => !/^[0-9a-f]{1,4}$/.test(g))) return null;
  const hex = (g: string) => parseInt(g, 16);
  const width = 8 - tail.length;
  let groups: number[];
  if (halves.length === 2) {
    const fill = width - head.length - rest.length;
    if (fill < 1) return null;
    groups = [...head.map(hex), ...new Array<number>(fill).fill(0), ...rest.map(hex)];
  } else {
    if (head.length !== width) return null;
    groups = head.map(hex);
  }
  return [...groups, ...tail];
}

/** The IPv4 address embedded as ::ffff:a.b.c.d, or null. */
function mappedV4(groups: number[]): number[] | null {
  if (groups.slice(0, 5).some((g) => g !== 0) || groups[5] !== 0xffff) return null;
  return [groups[6] >> 8, groups[6] & 0xff, groups[7] >> 8, groups[7] & 0xff];
}

function classifyV4(o: number[]): HostClass {
  if (o[0] === 127) return "loopback";
  if (o[0] === 100 && o[1] >= 64 && o[1] <= 127) return "tailscale"; // 100.64.0.0/10 (CGNAT, Tailscale's range)
  return "other";
}

/**
 * Loopback, Tailscale or other. Takes a URL hostname (IPv6 with or without
 * brackets). Anything that doesn't parse as one of the known forms is "other".
 * The host is taken exactly as given: a trailing dot (`100.64.0.1.`,
 * `localhost.`) makes it a name for the resolver, so "other"; only a
 * MagicDNS name may end in a dot. Matches the metro-url-policy crate, checked
 * by the shared vectors in crates/url-policy/testdata/host_vectors.json.
 */
export function classifyHost(hostname: string): HostClass {
  const host = bareHost(hostname);
  if (!host) return "other";
  if (host === "localhost") return "loopback";
  if (isTsNetName(host.endsWith(".") ? host.slice(0, -1) : host)) return "tailscale";
  const v4 = ipv4(host);
  if (v4) return classifyV4(v4);
  const v6 = ipv6(host);
  if (v6) {
    if (v6.slice(0, 7).every((g) => g === 0) && v6[7] === 1) return "loopback"; // ::1
    const mapped = mappedV4(v6);
    if (mapped) return classifyV4(mapped);
    if (v6[0] === 0xfd7a && v6[1] === 0x115c && v6[2] === 0xa1e0) return "tailscale"; // fd7a:115c:a1e0::/48
  }
  return "other";
}

export function isLoopbackHost(hostname: string): boolean {
  return classifyHost(hostname) === "loopback";
}

/** The host part of a server address, for messages. */
export function serverHost(baseUrl: string): string {
  try {
    return new URL(baseUrl).host;
  } catch {
    return baseUrl;
  }
}

/** The one-time warning shown before a token goes over plain HTTP to a host that isn't loopback or Tailscale. */
export function plainHttpWarning(baseUrl: string): string {
  return (
    `This connection isn't encrypted. Anyone on the network between you and ${serverHost(baseUrl)} could read your sign-in token. ` +
    "Use it on networks you trust, such as Tailscale or a VPN."
  );
}

export function parseServerUrl(input: string): ServerUrlResult {
  const raw = input.trim();
  if (!raw) return { ok: false, error: "Enter the server URL, for example " + DEFAULT_SERVER_URL + "." };
  if (!/^[a-z][a-z0-9+.-]*:\/\//i.test(raw)) {
    return { ok: false, error: "Include the scheme: http://… or https://…, for example " + DEFAULT_SERVER_URL + "." };
  }
  let url: URL;
  try {
    url = new URL(raw);
  } catch {
    return { ok: false, error: `"${raw}" is not a valid URL.` };
  }
  const scheme = url.protocol.toLowerCase();
  if (scheme !== "http:" && scheme !== "https:") {
    return { ok: false, error: `Unsupported scheme ${url.protocol} — use http:// or https://.` };
  }
  if (url.username || url.password) {
    return { ok: false, error: "Remove the user name and password from the URL; the access token is entered separately." };
  }
  if (url.search || url.hash) {
    return { ok: false, error: "The server URL must not contain a query (?…) or fragment (#…)." };
  }
  if (!url.hostname) return { ok: false, error: "The server URL has no host." };
  const network = classifyHost(url.hostname);
  const path = url.pathname.replace(/\/+$/, "");
  return {
    ok: true,
    baseUrl: `${scheme}//${url.host}${path}`,
    loopback: network === "loopback",
    network,
    needsConsent: scheme === "http:" && network === "other",
  };
}
