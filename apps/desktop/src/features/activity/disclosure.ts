// Which activity details the user opened. Held outside the rows, so a rerender,
// a new poll or the virtualized list recycling a row never resets a toggle,
// and keyed by connection, user and place, so another Space, room or the Chat
// with the same event ids never inherits an open row.

import { computed, signal } from "nsh";
import { activeRoomId, activeSpaceId, connectedTo, mainPlace, me } from "../../store";

const MAX_OPEN = 500;

const open = signal<ReadonlySet<string>>(new Set());

/** The current place's scope: Chat or Space + room, for this server and user. */
export const disclosureScope = computed(() => {
  const where = mainPlace() === "chat" ? "chat" : `space\u0001${activeSpaceId() ?? ""}\u0001${activeRoomId() ?? ""}`;
  return `${connectedTo() ?? ""}\u0002${me() ?? ""}\u0002${where}`;
});

/** `item` is an event id or an activity group's stable key. */
export function disclosureKey(scope: string, item: number | string, part = "details"): string {
  return `${scope}\u0003${item}\u0003${part}`;
}

export function isOpen(key: string): boolean {
  return open().has(key);
}

export function setOpen(key: string, value: boolean): void {
  const current = open();
  if (current.has(key) === value) return;
  const next = new Set(current);
  if (value) {
    next.add(key);
    // Bounded: forget the oldest opened rows first.
    while (next.size > MAX_OPEN) next.delete(next.values().next().value as string);
  } else next.delete(key);
  open.set(next);
}

export function toggle(key: string): void {
  setOpen(key, !isOpen(key));
}

/** Forget every toggle (tests; sign-out is covered by the scope's user and server). */
export function resetDisclosures(): void {
  open.set(new Set());
}
