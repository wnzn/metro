// The app's typeface, from fonts already on the computer (nothing is
// downloaded or bundled).
//
// theme/theme.json names Inter for every platform. NSH 0.6.1 lays text out
// in the FIRST family of the list when it is installed and otherwise in the
// system's default face; the later names only supply characters the first
// lacks (verified on Linux/GTK). So the preferred face must come first:
// - Windows: Segoe UI Variable (Windows 11), set here at startup;
// - elsewhere: Inter where installed, else the system's default UI face.
// Importing this module (any import of "../ui" does) applies it before the
// first frame.

import { platform, setTheme } from "nsh";
import type { Theme } from "nsh";
import theme from "../../theme/theme.json";

export const WINDOWS_FONT = "'Segoe UI Variable Text', 'Segoe UI', sans-serif";

/** The theme of the build, with the Windows face when running on Windows. */
export function platformTheme(os: string): Theme | null {
  if (os !== "windows") return null;
  const base = theme as Theme;
  return { cssVars: { ...base.cssVars, theme: { ...(base.cssVars.theme ?? {}), "font-sans": WINDOWS_FONT } } };
}

const windows = platformTheme(platform.os);
if (windows) {
  try {
    setTheme(windows);
  } catch {
    /* a theme NSH refuses: keep the built one (Inter, else the system face) */
  }
}
