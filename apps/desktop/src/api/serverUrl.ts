// Validates the server URL the user types before any token is sent to it.
//
// Rules:
// - only http: and https: URLs, without embedded credentials, query or fragment;
// - plain http: only for loopback hosts (localhost, 127.0.0.0/8, ::1), so a
//   bearer token never crosses a network in clear text;
// - the result is the origin plus an optional base path, without a trailing slash.

export const DEFAULT_SERVER_URL = "http://127.0.0.1:8787";

export type ServerUrlResult = { ok: true; baseUrl: string; loopback: boolean } | { ok: false; error: string };

export function isLoopbackHost(hostname: string): boolean {
  const host = hostname.toLowerCase().replace(/^\[|\]$/g, "");
  if (host === "localhost" || host.endsWith(".localhost")) return true;
  if (host === "::1" || host === "0:0:0:0:0:0:0:1") return true;
  const v4 = /^(\d{1,3})\.(\d{1,3})\.(\d{1,3})\.(\d{1,3})$/.exec(host);
  if (v4) {
    const octets = v4.slice(1).map(Number);
    return octets.every((n) => n <= 255) && octets[0] === 127;
  }
  // IPv4-mapped IPv6 loopback, e.g. ::ffff:127.0.0.1 or ::ffff:7f00:1
  const mapped = /^::ffff:(.+)$/.exec(host);
  if (mapped) {
    if (mapped[1].includes(".")) return isLoopbackHost(mapped[1]);
    return /^7f[0-9a-f]{2}:[0-9a-f]{1,4}$/.test(mapped[1]);
  }
  return false;
}

export function parseServerUrl(input: string): ServerUrlResult {
  const raw = input.trim();
  if (!raw) return { ok: false, error: "Enter the server URL, for example " + DEFAULT_SERVER_URL + "." };
  if (!/^[a-z][a-z0-9+.-]*:\/\//i.test(raw)) {
    return { ok: false, error: "Include the scheme: http://127.0.0.1:8787 for a local server, https://… for a remote one." };
  }
  let url: URL;
  try {
    url = new URL(raw);
  } catch {
    return { ok: false, error: `"${raw}" is not a valid URL.` };
  }
  const scheme = url.protocol.toLowerCase();
  if (scheme !== "http:" && scheme !== "https:") {
    return { ok: false, error: `Unsupported scheme ${url.protocol} — use http:// (local only) or https://.` };
  }
  if (url.username || url.password) {
    return { ok: false, error: "Remove the user name and password from the URL; the access token is entered separately." };
  }
  if (url.search || url.hash) {
    return { ok: false, error: "The server URL must not contain a query (?…) or fragment (#…)." };
  }
  if (!url.hostname) return { ok: false, error: "The server URL has no host." };
  const loopback = isLoopbackHost(url.hostname);
  if (scheme === "http:" && !loopback) {
    return {
      ok: false,
      error:
        `Refusing to send your access token over plain HTTP to ${url.host}. ` +
        "Use https:// for remote servers (plain http:// is only allowed for localhost / 127.0.0.1 / ::1).",
    };
  }
  const path = url.pathname.replace(/\/+$/, "");
  return { ok: true, baseUrl: `${scheme}//${url.host}${path}`, loopback };
}
