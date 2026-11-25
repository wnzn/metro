// The user's appearance preference: Dark (the default), Light, or System.
//
// The preference is a single non-secret string kept in the app's local
// Storage under APPEARANCE_KEY, so it survives restarts. The colors
// themselves live only in theme/theme.json; this module only picks which of
// its two appearances NSH shows.
//
// Importing this module (any import of "../ui" does) shows the app dark
// before the first frame, then applies the saved preference as soon as
// Storage answers. A Light user sees at most that one dark frame; nothing
// ever paints in a hardcoded light default.

import { colorScheme, setColorScheme, signal, Storage } from "nsh";

export type AppearancePreference = "dark" | "light" | "system";

export const APPEARANCE_KEY = "metro.appearance";
export const DEFAULT_APPEARANCE: AppearancePreference = "dark";

export const APPEARANCE_OPTIONS: readonly { value: AppearancePreference; label: string; description: string }[] = [
  { value: "dark", label: "Dark", description: "The default." },
  { value: "light", label: "Light", description: "Light backgrounds, dark text." },
  { value: "system", label: "System", description: "Follows your operating system." },
];

export function isAppearancePreference(value: unknown): value is AppearancePreference {
  return value === "dark" || value === "light" || value === "system";
}

const preference = signal<AppearancePreference>(DEFAULT_APPEARANCE);
let chosenThisSession = false;
let restoring: Promise<void> | undefined;

/** The preference in effect (reactive). */
export const appearancePreference = () => preference();

/** The appearance actually shown, after System is resolved (reactive). */
export const resolvedAppearance = () => colorScheme();

/** Shows `value` at once and remembers it on this device. */
let writes = Promise.resolve(true);
export function setAppearancePreference(value: AppearancePreference): Promise<boolean> {
  if (!isAppearancePreference(value)) return Promise.resolve(false);
  chosenThisSession = true;
  preference.set(value);
  setColorScheme(value);
  writes = writes.then(() => Storage.setItem(APPEARANCE_KEY, value)).then(() => true, () => false);
  return writes;
}

/**
 * Applies the saved preference. It's idempotent and safe to call from
 * App.tsx, and this module already calls it on import. A choice the user
 * makes before Storage answers wins over the saved value.
 */
export function restoreAppearance(): Promise<void> {
  restoring ??= (async () => {
    let saved: string | null = null;
    try {
      saved = await Storage.getItem(APPEARANCE_KEY);
    } catch {
      /* storage unavailable: keep the default */
    }
    if (chosenThisSession || !isAppearancePreference(saved)) return;
    preference.set(saved);
    setColorScheme(saved);
  })();
  return restoring;
}

// Before mount(): dark from the first frame, whatever the system says.
setColorScheme(DEFAULT_APPEARANCE);
void restoreAppearance();
