// The private preview directory policy against a small in-memory file system
// (the headless engine has no real one). The real thing was checked under
// NSH's Linux host with umask 077 and 022; see the commit's notes.

import { expect, test } from "nsh/test";
import { ensurePrivateDir, unixModes } from "./privateDir";
import type { PrivateDirFs } from "./privateDir";

/** A file system of one directory: made with `umask`, or already there with `mode`, links and contents as given. */
function fakeFs(opts: { umask: number; existing?: number; link?: boolean; notEmpty?: boolean; failMkdir?: boolean }) {
  let mode: number | null = opts.existing ?? null;
  const calls: string[] = [];
  const fs: PrivateDirFs = {
    async mkdir() {
      calls.push("mkdir");
      if (opts.failMkdir) throw Object.assign(new Error("denied"), { code: "EACCES" });
      if (mode === null) mode = 0o777 & ~opts.umask;
    },
    async lstat() {
      calls.push("lstat");
      if (mode === null) throw Object.assign(new Error("missing"), { code: "ENOENT" });
      const m = mode;
      return { mode: 0o040000 | m, isDirectory: () => !opts.link, isSymbolicLink: () => !!opts.link };
    },
    async rmdir() {
      calls.push("rmdir");
      if (opts.notEmpty) throw Object.assign(new Error("not empty"), { code: "ENOTEMPTY" });
      mode = null;
    },
  };
  return { fs, calls, mode: () => mode };
}

test("under the launcher's umask 077 a fresh directory is 0700 and accepted", async () => {
  const f = fakeFs({ umask: 0o077 });
  expect(await ensurePrivateDir("/d/quick-preview", f.fs, "linux")).toBeNull();
  expect(f.mode()).toBe(0o700);
  expect(f.calls).not.toContain("rmdir");
});

test("under umask 022 a fresh directory is 0755: refused, never written to (macOS too)", async () => {
  for (const platform of ["linux", "macos"]) {
    const f = fakeFs({ umask: 0o022 });
    const why = await ensurePrivateDir("/d/quick-preview", f.fs, platform);
    expect(why).toBe("the preview folder is open to other users (mode 755)");
  }
  // Group-readable only is refused too: no group or other bit at all.
  expect(await ensurePrivateDir("/d/x", fakeFs({ umask: 0o037 }).fs, "linux")).toBe("the preview folder is open to other users (mode 740)");
});

test("an older, open directory is remade when empty (and accepted under 077), kept and refused when not", async () => {
  const empty = fakeFs({ umask: 0o077, existing: 0o755 });
  expect(await ensurePrivateDir("/d/quick-preview", empty.fs, "linux")).toBeNull();
  expect(empty.calls).toContain("rmdir");
  expect(empty.mode()).toBe(0o700);
  const full = fakeFs({ umask: 0o077, existing: 0o755, notEmpty: true });
  expect(await ensurePrivateDir("/d/quick-preview", full.fs, "linux")).toBe("the preview folder is open to other users (mode 755)");
});

test("a link, or a directory that can't be made or checked, is refused", async () => {
  expect(await ensurePrivateDir("/d/q", fakeFs({ umask: 0o077, existing: 0o700, link: true }).fs, "linux")).toBe("the preview folder isn't a plain folder");
  expect(await ensurePrivateDir("/d/q", fakeFs({ umask: 0o077, failMkdir: true }).fs, "linux")).toBe("the preview folder couldn't be checked (EACCES)");
});

test("Windows has no Unix modes to check: a plain directory is accepted; a link still isn't", async () => {
  expect(unixModes("windows")).toBe(false);
  expect(await ensurePrivateDir("C:/d/q", fakeFs({ umask: 0o022 }).fs, "windows")).toBeNull();
  expect(await ensurePrivateDir("C:/d/q", fakeFs({ umask: 0o022, existing: 0o777, link: true }).fs, "windows")).toBe("the preview folder isn't a plain folder");
});
