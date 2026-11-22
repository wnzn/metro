import { expect, test } from "nsh/test";
import { parseConnection, parseStatus, scrub } from "./bridge";
import { parseBridgeEnv } from "./env";

const TOKEN = "0123456789abcdef".repeat(4);

test("only the launcher's exact loopback URL and 64-hex bearer count as a bridge", () => {
  const ok = parseBridgeEnv({ METRO_DESKTOP_BRIDGE_URL: "http://127.0.0.1:41234", METRO_DESKTOP_BRIDGE_TOKEN: TOKEN });
  expect(ok.present).toBe(true);
  if (ok.present) {
    expect(ok.endpoint.url).toBe("http://127.0.0.1:41234");
    expect(ok.endpoint.authorization()).toBe(`Bearer ${TOKEN}`);
    // The bearer is not a plain field of what callers hold.
    expect(JSON.stringify(ok)).not.toContain(TOKEN);
  }
  expect(parseBridgeEnv({})).toEqual({ present: false, reason: "absent" });
  const bad: Record<string, string>[] = [
    { METRO_DESKTOP_BRIDGE_URL: "http://127.0.0.1:41234/", METRO_DESKTOP_BRIDGE_TOKEN: TOKEN },
    { METRO_DESKTOP_BRIDGE_URL: "https://127.0.0.1:41234", METRO_DESKTOP_BRIDGE_TOKEN: TOKEN },
    { METRO_DESKTOP_BRIDGE_URL: "http://localhost:41234", METRO_DESKTOP_BRIDGE_TOKEN: TOKEN },
    { METRO_DESKTOP_BRIDGE_URL: "http://[::1]:41234", METRO_DESKTOP_BRIDGE_TOKEN: TOKEN },
    { METRO_DESKTOP_BRIDGE_URL: "http://127.0.0.1.evil.example:41234", METRO_DESKTOP_BRIDGE_TOKEN: TOKEN },
    { METRO_DESKTOP_BRIDGE_URL: "http://user@127.0.0.1:41234", METRO_DESKTOP_BRIDGE_TOKEN: TOKEN },
    { METRO_DESKTOP_BRIDGE_URL: "http://127.0.0.1:99999", METRO_DESKTOP_BRIDGE_TOKEN: TOKEN },
    { METRO_DESKTOP_BRIDGE_URL: "http://127.0.0.1:0", METRO_DESKTOP_BRIDGE_TOKEN: TOKEN },
    { METRO_DESKTOP_BRIDGE_URL: "http://127.0.0.1:41234", METRO_DESKTOP_BRIDGE_TOKEN: TOKEN.toUpperCase() },
    { METRO_DESKTOP_BRIDGE_URL: "http://127.0.0.1:41234", METRO_DESKTOP_BRIDGE_TOKEN: TOKEN.slice(1) },
    { METRO_DESKTOP_BRIDGE_URL: "http://127.0.0.1:41234" },
    { METRO_DESKTOP_BRIDGE_TOKEN: TOKEN },
  ];
  for (const env of bad) expect(parseBridgeEnv(env)).toEqual({ present: false, reason: "malformed" });
});

test("text from the bridge never shows a bearer or key shape", () => {
  expect(scrub(`bad ${TOKEN} here`)).toBe("bad [hidden] here");
  expect(scrub("key sk-ant-api03-abcdefgh rejected")).toBe("key [hidden] rejected");
  expect(scrub("Authorization: Bearer xyz")).toBe("Authorization: Bearer [hidden]");
  expect(scrub("metro_pc_abcdef123 revoked")).toBe("[hidden] revoked");
  expect(scrub("The endpoint answered 401.")).toBe("The endpoint answered 401.");
});

test("status and connections parse the contract's snake_case", () => {
  const s = parseStatus({
    bridge: 1,
    version: "0.2.0",
    instance: "primary",
    kinds: ["device-openai-oauth"],
    session: { server_url: "http://127.0.0.1:8787", account_id: "bob" },
    connections: [
      { connection_id: "pc1", kind: "device-openai-oauth", base_url: null, models: ["m"], credential: "signed_in", worker: "online", detail: null, login: { state: "waiting", auth_url: "http://127.0.0.1:5/auth/start?k=x", error: null } },
    ],
    local: "running",
  });
  expect(s.session).toEqual({ serverUrl: "http://127.0.0.1:8787", accountId: "bob" });
  expect(s.connections[0]!.login).toEqual({ state: "waiting", authUrl: "http://127.0.0.1:5/auth/start?k=x", error: null });
  expect(s.local).toBe("running");
  expect(() => parseStatus({ bridge: 2 })).toThrow();
  expect(() => parseConnection({ kind: "x" })).toThrow();
  expect(parseConnection({ connection_id: "a", kind: "k", detail: `leak ${TOKEN}` }).detail).toBe("leak [hidden]");
});
