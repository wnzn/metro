import { signal } from "nsh";
import type { ViewRef } from "nsh";
import { mainPlace, onTransientClear, startupReadGuard } from "../../store";
import type { RoomEvent } from "../../store";
import type { ForwardSource } from "../../api/dms";

/** Exact source for POST /v1/forward, plus the local preview. Never send preview data to the server. */
export const forwardSelection = signal<{
  source: ForwardSource; event: RoomEvent; current: () => boolean; opener?: ViewRef;
} | null>(null);

export function openForward(source: ForwardSource, event: RoomEvent, opener?: ViewRef, sourceVisible = () => true) {
  if ((event.kind !== "message" && event.kind !== "agent") || event.id !== source.event_id) return;
  const route = mainPlace();
  const valid = startupReadGuard(source.kind === "space" ? source.space_id : undefined);
  forwardSelection.set({ source, event, opener, current: () => valid() && mainPlace() === route && sourceVisible() });
}

export function closeForward() {
  const selection = forwardSelection();
  if (!selection) return;
  forwardSelection.set(null);
  // Wait for the host to take the modal off the focus stack.
  requestAnimationFrame(() => {
    if (selection.current() && !forwardSelection()) selection.opener?.focus();
  });
}

onTransientClear(() => forwardSelection.set(null));
