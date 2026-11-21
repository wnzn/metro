import { Clipboard, signal } from "nsh";
import {
  activeRoomId,
  activeSpaceId,
  connectedTo,
  createRoom,
  createSpace,
  creatingRoom,
  creatingSpace,
  disconnect,
  isActiveRun,
  me,
  openChat,
  place,
  rooms,
  runs,
  selectRoom,
  selectSpace,
  spaces,
  status,
  view,
} from "../store";
import type { Room } from "../store";
import { Action, Column, Field, Label, NavItem, Row, ScrollArea, Spacer, StatusDot, Surface } from "../ui";
import type { Tone } from "../ui";

export function Sidebar() {
  return (
    <Surface variant="sidebar" testID="sidebar">
      <Column pad={3} gap={1}>
        <Row align="center" gap={2}>
          <Label variant="heading" color="sidebar">
            Metro
          </Label>
          <Spacer />
          <ConnectionDot />
        </Row>
        <Label variant="caption" color="muted" lines={1} testID="server-origin">
          {connectedTo() ?? ""}
        </Label>
      </Column>

      <ScrollArea padX={2} gap={1}>
        <NavItem
          testID="nav-chat"
          label="Chat"
          prefix="✦"
          emphasis
          trailing={activeRunCount() ? `${activeRunCount()} running` : undefined}
          selected={place() === "chat"}
          onPress={openChat}
        />
        <SpacesSection />
      </ScrollArea>

      <Identity />
    </Surface>
  );
}

const activeRunCount = () => runs().filter(isActiveRun).length;

function SpacesSection() {
  const adding = signal(false);
  const name = signal("");
  const submit = async () => {
    if (await createSpace(name())) {
      name.set("");
      adding.set(false);
    }
  };
  return (
    <Column gap={1} padY={2}>
      <Row align="center" padX={2}>
        <Label variant="caption" color="muted">
          SPACES
        </Label>
        <Spacer />
        <Action
          testID="new-space-toggle"
          label={adding() ? "Cancel" : "+ Space"}
          accessibilityLabel={adding() ? "Cancel new Space" : "New Space"}
          variant="ghost"
          onPress={() => adding.set(!adding())}
        />
      </Row>
      <If when={adding()}>
        <Column padX={2} padY={1} gap={2}>
          <Field testID="new-space-name" accessibilityLabel="New Space name" value={name()} onChange={name.set} placeholder="Space name" autoFocus maxLength={80} onSubmit={() => void submit()} />
          <Action testID="create-space" label="Create Space" busyLabel="Creating…" busy={creatingSpace()} variant="primary" onPress={() => void submit()} />
        </Column>
      </If>
      <Each of={spaces()} key={(space) => space.id}>
        {(space) => (
          <Column gap={1}>
            <NavItem
              testID={`space:${space().id}`}
              label={space().name}
              prefix={space().id === activeSpaceId() && place() === "space" ? "▾" : "▸"}
              trailing={space().role === "owner" ? "owner" : undefined}
              emphasis={space().id === activeSpaceId()}
              selected={false}
              onPress={() => void selectSpace(space().id)}
            />
            <If when={space().id === activeSpaceId() && place() === "space"}>
              <RoomTree />
            </If>
          </Column>
        )}
      </Each>
      <If when={spaces().length === 0 && !adding()}>
        <Column padX={2} padY={1}>
          <Label variant="caption" color="muted">
            No Spaces yet. Create one, or ask a Space owner to add your ID.
          </Label>
        </Column>
      </If>
    </Column>
  );
}

type TreeRow = { room: Room; depth: number };

/** Rooms in server order, each thread right under the room it was delegated from. */
export function roomTree(list: readonly Room[]): TreeRow[] {
  const ids = new Set(list.map((r) => r.id));
  const children = new Map<string, Room[]>();
  const roots: Room[] = [];
  for (const room of list) {
    if (room.parentRoomId && ids.has(room.parentRoomId) && room.parentRoomId !== room.id) {
      children.set(room.parentRoomId, [...(children.get(room.parentRoomId) ?? []), room]);
    } else roots.push(room);
  }
  const out: TreeRow[] = [];
  const seen = new Set<string>();
  const walk = (room: Room, depth: number) => {
    if (seen.has(room.id)) return; // guards against cycles
    seen.add(room.id);
    out.push({ room, depth });
    for (const child of children.get(room.id) ?? []) walk(child, depth + 1);
  };
  for (const root of roots) walk(root, 1);
  for (const room of list) if (!seen.has(room.id)) walk(room, 1); // members of a cycle
  return out;
}

function RoomTree() {
  const adding = signal(false);
  const name = signal("");
  const submit = async () => {
    if (await createRoom(name())) {
      name.set("");
      adding.set(false);
    }
  };
  return (
    <Column gap={1} testID="room-list">
      <Each of={roomTree(rooms())} key={(row) => row.room.id}>
        {(row) => (
          <NavItem
            testID={`room:${row().room.id}`}
            label={row().room.name}
            prefix={row().depth > 1 ? "↳" : "#"}
            depth={row().depth}
            selected={row().room.id === activeRoomId() && view() === "room" && place() === "space"}
            onPress={() => selectRoom(row().room.id)}
          />
        )}
      </Each>
      <If
        when={adding()}
        else={
          <Row padX={2}>
            <Action testID="new-room-toggle" label="+ Room" accessibilityLabel="New room" variant="ghost" onPress={() => adding.set(true)} />
          </Row>
        }
      >
        <Column padX={2} padY={1} gap={2}>
          <Field testID="new-room-name" accessibilityLabel="New room name" value={name()} onChange={name.set} placeholder="Room name" autoFocus maxLength={80} onSubmit={() => void submit()} />
          <Row gap={2}>
            <Action testID="create-room" label="Create room" busyLabel="Creating…" busy={creatingRoom()} variant="primary" onPress={() => void submit()} />
            <Action label="Cancel" variant="ghost" onPress={() => adding.set(false)} />
          </Row>
        </Column>
      </If>
    </Column>
  );
}

function Identity() {
  const copied = signal(false);
  const copy = () => {
    const id = me();
    if (!id) return;
    Clipboard.writeText(id)
      .then(() => {
        copied.set(true);
        setTimeout(() => copied.set(false), 1500);
      })
      .catch(() => {});
  };
  return (
    <Column pad={3} gap={2}>
      <Column gap={1}>
        <Label variant="caption" color="muted">
          Your user ID (share it to be added to a Space)
        </Label>
        <Row align="center" gap={2}>
          <Column grow>
            <Label variant="mono" lines={1} testID="my-id">
              {me() ?? ""}
            </Label>
          </Column>
          <Action testID="copy-id" label={copied() ? "Copied" : "Copy ID"} variant="ghost" onPress={copy} />
        </Row>
      </Column>
      <Action testID="disconnect" label="Disconnect" variant="ghost" onPress={() => disconnect()} />
    </Column>
  );
}

function ConnectionDot() {
  const tone = (): Tone => {
    const s = status();
    return s.phase === "online" ? "success" : s.phase === "retrying" ? "warning" : "neutral";
  };
  const label = () => {
    const s = status();
    return s.phase === "online" ? "Online" : s.phase === "retrying" ? "Reconnecting" : "Connecting";
  };
  return (
    <Row align="center" gap={1} accessibilityLabel={`Connection: ${label()}`} testID="connection-state">
      <StatusDot tone={tone()} />
      <Label variant="caption" color="muted">
        {label()}
      </Label>
    </Row>
  );
}
