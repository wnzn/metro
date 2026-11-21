import { expect, test } from "nsh/test";
import { isLoopbackHost, parseServerUrl } from "./serverUrl";

test("allows plain http only for loopback hosts", () => {
  for (const ok of ["http://127.0.0.1:8787", "http://localhost:8787/", "http://[::1]:8787", "http://127.10.0.3", "https://metro.example.com"]) {
    expect(parseServerUrl(ok).ok).toBe(true);
  }
  for (const bad of ["http://192.168.1.5:8787", "http://metro.example.com", "http://10.0.0.1", "http://127.0.0.1.evil.com", "http://[::ffff:c0a8:1]"]) {
    const r = parseServerUrl(bad);
    expect(r.ok).toBe(false);
    if (!r.ok) expect(r.error).toContain("plain HTTP");
  }
});

test("normalizes and rejects malformed URLs", () => {
  expect(parseServerUrl(" http://127.0.0.1:8787/ ")).toEqual({ ok: true, baseUrl: "http://127.0.0.1:8787", loopback: true });
  expect(parseServerUrl("https://metro.example.com/api/")).toEqual({ ok: true, baseUrl: "https://metro.example.com/api", loopback: false });
  expect(parseServerUrl("").ok).toBe(false);
  expect(parseServerUrl("127.0.0.1:8787").ok).toBe(false);
  expect(parseServerUrl("ftp://127.0.0.1").ok).toBe(false);
  expect(parseServerUrl("https://user:pw@metro.example.com").ok).toBe(false);
  expect(parseServerUrl("https://metro.example.com/?x=1").ok).toBe(false);
});

test("recognizes loopback forms", () => {
  expect(isLoopbackHost("LOCALHOST")).toBe(true);
  expect(isLoopbackHost("[::1]")).toBe(true);
  expect(isLoopbackHost("::ffff:127.0.0.1")).toBe(true);
  expect(isLoopbackHost("128.0.0.1")).toBe(false);
  expect(isLoopbackHost("localhost.example.com")).toBe(false);
});
