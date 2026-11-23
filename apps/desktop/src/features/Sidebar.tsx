import { announce, cleanup, Clipboard, signal } from "nsh";
import { onLocalWorkspace } from "../localWorkspace";
import {
  activeRoomId,
  activeSpaceId,
  connectedTo,
  createRoom,
  createSpace,
  creatingRoom,
  creatingSpace,
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
import { AccountMenu, Action, Column, Field, IconButton, Label, MetroMark, NavItem, Row, ScrollArea, Spacer, StatusDot, Surface } from "../ui";
import type { AccountMenuItem, Tone } from "../ui";
import { loadSavedServers, signOut } from "../savedWorkspaces";
import { WorkspaceDialogs, workspaceItems, WorkspaceNotice } from "./WorkspaceSwitcher";
import { PersonAvatar } from "./profile/PersonAvatar";
import { profileName } from "./profile/profileState";

/**
 * The app's navigation: Chat, Library, Scheduled tasks, the Spaces and their
 * rooms, and Settings (a modal). Search is the box at the top of the main
 * panel. Who is signed in, and where, is the account menu at the bottom.
 */
export function Sidebar(props: { hidden?: boolean }) {
  return (
    <Surface variant="sidebar" testID="sidebar" hidden={props.hidden}>
      <Row align="center" gap={2} padX={4} padY={3}>
        <MetroMark />
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
        <NavItem testID="nav-library" label="Library" icon="library" selected={mainPlace() === "library"} onPress={() => openPlace("library")} />
        <NavItem testID="nav-schedules" label="Scheduled tasks" icon="clock" selected={mainPlace() === "schedules"} onPress={() => openPlace("schedules")} />
        <SpacesSection />
      </ScrollArea>

      <Column padX={2} padY={2} gap={1}>
        <NavItem testID="nav-settings" label="Settings" icon="settings" selected={settingsOpen()} onPress={() => openSettings()} />
      </Column>
      <AccountFooter />
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
          // In line with the rooms: the plus where their # is.
          <NavItem testID="new-room-toggle" label="New room" icon="plus" depth={1} selected={false} onPress={() => adding.set(true)} />
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

/**
 * Who is signed in (picture, profile name), and where, once: "On this
 * device" for the app-hosted workspace, else the server's host. The menu
 * copies the server URL or the user's stable ID (what Space owners add),
 * and signs out.
 */
function AccountFooter() {
  const copied = signal<string | null>(null);
  let timer: ReturnType<typeof setTimeout> | undefined;
  cleanup(() => timer && clearTimeout(timer));
  const copy = (value: string | null, done: string) => {
    if (!value) return;
    Clipboard.writeText(value)
      .then(() => {
        copied.set(done);
        announce(done);
        if (timer) clearTimeout(timer);
        timer = setTimeout(() => copied.set(null), 1500);
      })
      .catch(() => {});
  };
  const server = () => serverPlace(connectedTo(), onLocalWorkspace());
  const name = () => profileName(me()) ?? me() ?? "";
  void loadSavedServers();
  // Item ids are the testIDs: account-menu opens them. The Workspaces group
  // switches straight to this computer's workspace or a saved server. Sign
  // out ends this session and forgets its server's token in memory.
  const items = (): readonly AccountMenuItem[] => [
    ...workspaceItems(),
    { id: "copy-server-url", label: "Copy server URL", icon: "copy", onPress: () => copy(connectedTo(), "Server URL copied") },
    { id: "copy-id", label: "Copy ID", icon: "copy", onPress: () => copy(me(), "ID copied") },
    { id: "disconnect", label: "Sign out", icon: "logout", destructive: true, onPress: () => signOut() },
  ];
  return (
    <Column padX={2} padY={2} gap={1} testID="active-server">
      <AccountMenu
        testID="account-menu"
        accessibilityLabel={`${name()}, ${server().local ? "on this device" : `connected to ${server().title}`}. Account menu`}
        avatar={<PersonAvatar userId={me()} name={name()} size="sm" />}
        name={name()}
        detail={server().title}
        detailIcon={server().local ? "device" : "server"}
        note={me() ?? undefined}
        items={items()}
      />
      <WorkspaceDialogs />
      <WorkspaceNotice />
      <If when={copied()}>
        {(message) => (
          <Row padX={2}>
            <Label variant="caption" color="success" testID="account-copied">
              {message()}
            </Label>
          </Row>
        )}
      </If>
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
