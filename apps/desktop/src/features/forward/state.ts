import { signal } from "nsh";
import type { ViewRef } from "nsh";
import { mainPlace, onTransientClear, sessionGuard } from "../../store";
import type { RoomEvent } from "../../store";

/** Exact source for POST /v1/forward, plus the local preview. Never send preview data to the server. */
export type ForwardSource =
  | { kind: "space"; space_id: string; room_id: string; event_id: number }
  | { kind: "chat"; event_id: number }
  | { kind: "dm"; dm_id: string; event_id: number };
export const forwardSelection = signal<{
  source: ForwardSource; event: RoomEvent; current: () => boolean; opener?: ViewRef;
} | null>(null);

export function openForward(source: ForwardSource, event: RoomEvent, opener?: ViewRef, sourceVisible = () => true) {
  if ((event.kind !== "message" && event.kind !== "agent") || event.id !== source.event_id) return;
  const route = mainPlace();
  const valid = sessionGuard(source.kind === "space" ? source.space_id : undefined);
  forwardSelection.set({ source, event, opener, current: () => valid() && mainPlace() === route && sourceVisible() });
}

export function closeForward() {
  const selection = forwardSelection();
  forwardSelection.set(null);
  if (selection?.current()) queueMicrotask(() => selection.opener?.focus());
}

onTransientClear(() => forwardSelection.set(null));
