import { Clipboard, signal } from "nsh";
import { onLocalWorkspace } from "../localWorkspace";
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
  mainPlace,
  me,
  openChat,
  openPlace,
  openSettings,
  rooms,
  runs,
  selectRoom,
  selectSpace,
  settingsOpen,
  spaces,
  status,
  view,
} from "../store";
import type { Room } from "../store";
import { Action, Column, Field, Icon, IconButton, Label, NavItem, Row, ScrollArea, Spacer, StatusDot, Surface } from "../ui";
import type { Tone } from "../ui";

/**
 * The app's navigation: Chat, Scheduled tasks, the Spaces and their rooms,
 * and Settings (a modal). Search is the box at the top of the main panel.
 * The server this session is connected to, and who is signed in, are at the
 * bottom.
 */
export function Sidebar() {
  return (
    <Surface variant="sidebar" testID="sidebar">
      <Row align="center" gap={2} padX={4} padY={3}>
        <Label variant="heading" color="sidebar">
          Metro
        </Label>
        <Spacer />
        <ConnectionDot />
      </Row>

      <ScrollArea padX={2} gap={1}>
        <NavItem
          testID="nav-chat"
          label="Chat"
          icon="chat"
          emphasis
          trailing={activeRunCount() ? `${activeRunCount()} running` : undefined}
          selected={mainPlace() === "chat"}
          onPress={openChat}
        />
        <NavItem testID="nav-schedules" label="Scheduled tasks" icon="clock" selected={mainPlace() === "schedules"} onPress={() => openPlace("schedules")} />
        <SpacesSection />
      </ScrollArea>

      <Column padX={2} padY={2} gap={1}>
        <NavItem testID="nav-settings" label="Settings" icon="settings" selected={settingsOpen()} onPress={() => openSettings()} />
      </Column>
      <ServerFooter />
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
          Spaces
        </Label>
        <Spacer />
        <IconButton
          testID="new-space-toggle"
          icon={adding() ? "close" : "plus"}
          accessibilityLabel={adding() ? "Cancel new Space" : "New Space"}
          tooltip={adding() ? "Cancel" : "New Space"}
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
              icon={space().id === activeSpaceId() && mainPlace() === "space" ? "chevron-down" : "chevron-right"}
              trailing={space().role === "owner" ? "owner" : undefined}
              emphasis={space().id === activeSpaceId()}
              selected={false}
              onPress={() => void selectSpace(space().id)}
            />
            <If when={space().id === activeSpaceId() && mainPlace() === "space"}>
              <RoomTree />
            </If>
          </Column>
        )}
      </Each>
      <If when={spaces().length === 0 && !adding()}>
        <Column padX={2} padY={1}>
          <Label variant="caption" color="muted">
            No Spaces yet. Create one, or ask a Space owner to add you.
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
            icon={row().depth > 1 ? "thread" : "hash"}
            depth={row().depth}
            selected={row().room.id === activeRoomId() && view() === "room" && mainPlace() === "space"}
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

/**
 * Where this session is connected. "On this device" only for the workspace
 * this app itself hosts (`hosted`: src/localWorkspace.ts `onLocalWorkspace`),
 * never from the URL: a loopback address may be a port forward to another
 * computer. Every other session shows the host and URL it connected to.
 */
export function serverPlace(url: string | null, hosted: boolean): { local: boolean; title: string; detail: string } {
  if (!url) return { local: false, title: "Not connected", detail: "" };
  if (hosted) return { local: true, title: "On this device", detail: url };
  try {
    return { local: false, title: new URL(url).host, detail: url };
  } catch {
    return { local: false, title: url, detail: url };
  }
}

/** The server this session talks to, who is signed in, copy ID and disconnect. */
function ServerFooter() {
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
  const server = () => serverPlace(connectedTo(), onLocalWorkspace());
  return (
    <Column padX={3} padY={3} gap={2} testID="server-footer">
      <Row align="center" gap={2} accessibilityLabel={`Connected to ${server().title}${server().local ? `, ${server().detail}` : ""}`} testID="active-server">
        <Icon name={server().local ? "device" : "server"} color="sidebar" />
        <Column grow>
          <Label variant="strong" color="sidebar" lines={1} testID="server-title">
            {server().title}
          </Label>
          <Label variant="caption" color="muted" lines={1} testID="server-origin">
            {server().detail}
          </Label>
        </Column>
      </Row>
      <Row align="center" gap={1}>
        <Column grow>
          <Label variant="mono" color="sidebar" lines={1} testID="my-id">
            {me() ?? ""}
          </Label>
        </Column>
        <IconButton
          testID="copy-id"
          icon={copied() ? "check" : "copy"}
          accessibilityLabel={copied() ? "Your ID was copied" : "Copy your ID, to share with a Space owner"}
          tooltip={copied() ? "Copied" : "Copy ID"}
          onPress={copy}
        />
        <IconButton testID="disconnect" icon="logout" accessibilityLabel="Disconnect" tooltip="Disconnect" onPress={() => disconnect()} />
      </Row>
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
