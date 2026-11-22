// Where the desktop bridge is, if this UI was started by the packaged Metro
// launcher (contract v1). The launcher sets exactly two variables on the UI
// process only; NSH exposes them because nsh.json lists them under
// capabilities.process.env (NSH 0.6.1 shows no other variables to an app).
//
// Strict on purpose: anything but `http://127.0.0.1:<port>` and a 64-hex
// bearer counts as "no bridge" (`nsh run`, `nsh test`, a hand-started UI),
// so the bearer is never sent anywhere but IPv4 loopback.
//
// The bearer lives only in the closure this module returns. It is never
// exported as a value, put in a signal, written to storage or logged.

import proc from "process";

const URL_RE = /^http:\/\/127\.0\.0\.1:([0-9]{1,5})$/;
const TOKEN_RE = /^[0-9a-f]{64}$/;

export type BridgeEndpoint = {
  /** `http://127.0.0.1:<port>`, no trailing slash. */
  readonly url: string;
  /** The `Authorization` header value. A function so the bearer never sits in a plain field. */
  readonly authorization: () => string;
};

export type BridgeEnv =
  | { present: true; endpoint: BridgeEndpoint }
  /** `absent`: neither variable is set. `malformed`: set, but not what the launcher sets. */
  | { present: false; reason: "absent" | "malformed" };

/** Parses the two variables; pure, for tests. */
export function parseBridgeEnv(env: Record<string, string | undefined>): BridgeEnv {
  const url = env.METRO_DESKTOP_BRIDGE_URL;
  const token = env.METRO_DESKTOP_BRIDGE_TOKEN;
  if (url === undefined && token === undefined) return { present: false, reason: "absent" };
  const m = typeof url === "string" ? URL_RE.exec(url) : null;
  const port = m ? Number(m[1]) : 0;
  if (!m || port < 1 || port > 65535 || typeof token !== "string" || !TOKEN_RE.test(token)) {
    return { present: false, reason: "malformed" };
  }
  const header = `Bearer ${token}`;
  return { present: true, endpoint: { url: url as string, authorization: () => header } };
}

/** Reads the variables once, at startup. NSH returns a new env object per read; nothing is cached but the parsed result. */
export function readBridgeEnv(): BridgeEnv {
  let env: Record<string, string> = {};
  try {
    env = proc.env ?? {};
  } catch {
    env = {};
  }
  return parseBridgeEnv({
    METRO_DESKTOP_BRIDGE_URL: env.METRO_DESKTOP_BRIDGE_URL,
    METRO_DESKTOP_BRIDGE_TOKEN: env.METRO_DESKTOP_BRIDGE_TOKEN,
  });
}
