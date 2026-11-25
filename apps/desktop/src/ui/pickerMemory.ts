import { signal } from "nsh";
import type { Storage } from "nsh";

export type PickerKind = "default" | "composer" | "routing" | "space-prefs";

/** A local UI preference, never keyed by a display name, credential or Space. */
export function pickerScopeKey(server: string | null, user: string | null, kind: PickerKind): string | null {
  return server && user ? `metro.modelPicker:${JSON.stringify([server, user, kind])}` : null;
}

type PickerStorage = Pick<typeof Storage, "getItem" | "setItem" | "removeItem">;
export type PickerGroupMemory = { collapsed: () => boolean; setCollapsed: (next: boolean) => void };

/** Separate keys keep a late read or a failed write for one provider from
 * changing another. NSH Storage runs its operations in order; mounted
 * pickers for the same scope also share these live values. */
export function createPickerMemory(storage?: PickerStorage) {
  const groups = new Map<string, PickerGroupMemory>();
  return (scope: string | null, provider: string) => {
    const key = `${scope ?? ""}:${JSON.stringify(provider)}`;
    const existing = groups.get(key);
    if (existing) return existing;

    const collapsed = signal(false);
    let edited = false;
    const entry = {
      collapsed: () => collapsed(),
      setCollapsed(next: boolean) {
        edited = true;
        collapsed.set(next);
        if (scope && storage) {
          void (next ? storage.setItem(key, "collapsed") : storage.removeItem(key)).catch(() => {});
        }
      },
    };
    groups.set(key, entry);
    if (scope && storage) {
      void storage.getItem(key).then((saved) => {
        if (!edited) collapsed.set(saved === "collapsed");
      }).catch(() => {});
    }
    return entry;
  };
}
