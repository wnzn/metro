// The one policy for directories that hold a library file's bytes on disk
// (the viewer's and quick previews' private copies, made only where the
// platform's player can't play a `blob:` URL, as GTK's can't).
//
// On Unix (Linux, macOS and the other Unix hosts) a directory is used only
// when it is a real directory, not a link, with no group or other permission
// bits at all (mode & 0o077 === 0). NSH's files API can't set modes, so this
// rests on the process's umask: the shipped launcher sets 0o077 before the UI
// starts, so a directory it creates is 0700 and its files 0600. A directory
// that isn't private (an older one, a run under umask 022) is removed when
// it's empty and made again, then checked again; if it still isn't private,
// nothing is written and the caller says why. On Windows, Unix mode bits mean
// nothing (files sit under the user's own profile), so only the directory
// itself is checked. Where there's no data directory (the web) there's no
// file fallback at all.

import { lstat, mkdir, rmdir } from "fs/promises";
import proc from "process";

export type PrivateDirFs = {
  mkdir: (path: string, options: { recursive: boolean }) => Promise<void>;
  lstat: (path: string) => Promise<{ mode: number; isDirectory(): boolean; isSymbolicLink(): boolean }>;
  rmdir: (path: string) => Promise<void>;
};

const nativeFs: PrivateDirFs = { mkdir, lstat, rmdir };

/** Why the directory can't hold private bytes; null when it can. */
export type PrivateDirRefusal = string | null;

const UNIX = new Set(["linux", "macos", "freebsd", "openbsd", "netbsd", "android", "ios"]);

/** Whether this platform's mode bits decide who can read a file. */
export const unixModes = (platform: string) => UNIX.has(platform);

async function inspect(dir: string, fs: PrivateDirFs, unix: boolean): Promise<PrivateDirRefusal> {
  const st = await fs.lstat(dir);
  if (st.isSymbolicLink() || !st.isDirectory()) return "the preview folder isn't a plain folder";
  if (unix && (st.mode & 0o077) !== 0) return `the preview folder is open to other users (mode ${(st.mode & 0o777).toString(8)})`;
  return null;
}

/**
 * Makes `dir` if needed and confirms it may hold private bytes, before any
 * byte is written. Resolves to null when it may, else to why not (the caller
 * writes nothing and says so).
 */
export async function ensurePrivateDir(dir: string, fs: PrivateDirFs = nativeFs, platform: string = proc.platform): Promise<PrivateDirRefusal> {
  const unix = unixModes(platform);
  try {
    await fs.mkdir(dir, { recursive: true });
    const first = await inspect(dir, fs, unix);
    if (first === null || !first.startsWith("the preview folder is open")) return first;
    // Not private (made before the launcher's umask, or a run without it): only an empty one is remade.
    try {
      await fs.rmdir(dir);
    } catch {
      return first;
    }
    await fs.mkdir(dir, { recursive: true });
    return await inspect(dir, fs, unix);
  } catch (error) {
    return `the preview folder couldn't be checked (${String((error as { code?: string })?.code ?? (error as Error)?.message ?? error)})`;
  }
}
