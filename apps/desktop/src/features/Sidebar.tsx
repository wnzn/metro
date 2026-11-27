import { announce, cleanup, Clipboard, signal } from "nsh";
import { onLocalWorkspace } from "../localWorkspace";
import {
  activeChatThreadId,
  activeRoomId,
  activeSpaceId,
  connectedTo,
  createRoom,
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
import { goPages, pages, pagesRoute } from "../pagesState";
import { dismissRunnerOffer, runnerOfferCount, watchInstalledRunner } from "../runnersState";
import { AccountMenu, Action, Banner, Column, Field, Label, MetroMark, NavGroup, NavItem, navigationPanel, RailBrand, RailButton, ResizeHandle, Row, ScrollArea, SectionHeader, SidebarBrand, Surface } from "../ui";
import type { AccountMenuItem, Tone } from "../ui";
import { loadSavedServers, signOut } from "../savedWorkspaces";
import { settingsPageDot } from "./SettingsView";
import { WorkspaceDialogs, workspaceItems, WorkspaceNotice } from "./WorkspaceSwitcher";
import { PersonAvatar } from "./profile/PersonAvatar";
import { profileName } from "./profile/profileState";
import { openSpaceSettings } from "./space/spaceSettingsState";
import { CreateSpaceForm } from "./CreateSpaceForm";
import { ChatThreadsNav } from "./chatThreads/ChatThreads";
import { DirectMessagesSection } from "./dms/DirectMessages";
import { activeDm, dmUnread, openMessages } from "./dms/state";

/**
 * The app's navigation: Chat, Library, Scheduled tasks, the Spaces and their
 * rooms, and Settings (a modal). Search is the box at the top of the main
 * panel. Who is signed in, where, and whether the connection is up, is the
 * account menu at the bottom.
 *
 * `mode`: the whole panel; collapsed to an icon `rail` (the same places,
 * each Space as its initial, the account at the bottom); or `hidden` (a
 * narrow window). The panel stays mounted while collapsed, so a half-typed
 * Space name is still there when it opens again.
 */
export function Sidebar(props: { mode: "full" | "rail" | "hidden" }) {
  watchInstalledRunner();
  return (
    <>
      <Surface variant="sidebar" testID="sidebar" hidden={props.mode !== "full"} width={navigationPanel.width()}>
        <SidebarBrand>
          <MetroMark />
        </SidebarBrand>

        <ScrollArea padX={2} padY={3} gap={1}>
          <NavItem
            testID="nav-chat"
            label="Chat"
            icon="chat"
            emphasis
            trailing={activeRunCount() ? `${activeRunCount()} running` : undefined}
            selected={mainPlace() === "chat" && !activeChatThreadId()}
            onPress={openChat}
          />
          <ChatThreadsNav />
          <NavItem testID="nav-library" label="Library" icon="library" selected={mainPlace() === "library"} onPress={() => openPlace("library")} />
          <NavItem testID="nav-pages" label="Pages" icon="file" selected={selectedPages()} onPress={() => goPages()} />
          <PinnedPages />
          <NavItem testID="nav-schedules" label="Scheduled tasks" icon="clock" selected={mainPlace() === "schedules"} onPress={() => openPlace("schedules")} />
          <DirectMessagesSection visible={props.mode === "full"} />
          <SpacesSection visible={props.mode === "full"} />
        </ScrollArea>

        <Column padX={2} padY={2} gap={1}>
          <NavItem testID="nav-settings" label="Settings" icon="settings" dot={settingsPageDot("about")}
            selected={settingsOpen()} onPress={() => openSettings()} />
        </Column>
        <AccountFooter />
      </Surface>
      <If when={props.mode === "full"}>
        <ResizeHandle panel={navigationPanel} edge="end" name="workspace navigation" collapsible testID="resize-navigation" />
      </If>
      <If when={props.mode === "rail"}>
        <SidebarRail />
      </If>
      {/* Once, whichever form the navigation has: the account menu in either opens them. */}
      <WorkspaceDialogs />
    </>
  );
}

/** The collapsed navigation: the same places as icons, each with its name as tooltip and accessible name. */
function SidebarRail() {
  const runs = () => activeRunCount();
  return (
    <Surface variant="rail" testID="sidebar-rail" accessibilityLabel="Workspace navigation">
      <RailBrand>
        <MetroMark symbolOnly size="sm" />
      </RailBrand>
      <RailButton testID="rail-chat" label={runs() ? `Chat, ${runs()} running` : "Chat"} icon="chat" selected={mainPlace() === "chat"} onPress={openChat} />
      <RailButton testID="rail-library" label="Library" icon="library" selected={mainPlace() === "library"} onPress={() => openPlace("library")} />
      <RailButton testID="rail-pages" label="Pages" icon="file" selected={mainPlace() === "pages"} onPress={() => goPages()} />
      <RailButton testID="rail-schedules" label="Scheduled tasks" icon="clock" selected={mainPlace() === "schedules"} onPress={() => openPlace("schedules")} />
      <RailButton testID="rail-messages" label="Messages" icon="chat" selected={mainPlace() === "dm"}
        dot={dmUnread() ? `${dmUnread()} unread` : undefined}
        onPress={() => { if (mainPlace() !== "dm" || activeDm()) openMessages(); }} />
      <ScrollArea gap={1} align="center" padY={2}>
        <Each of={spaces()} key={(space) => space.id}>
          {(space) => (
            <RailButton
              testID={`rail-space:${space().id}`}
              label={`Space ${space().name}`}
              initial={spaceInitial(space().name)}
              seed={space().id}
              selected={space().id === activeSpaceId() && mainPlace() === "space"}
              onPress={() => void selectSpace(space().id)}
              menu={[
                { id: `rail-space-settings:${space().id}`, label: "Space settings", onPress: () => openSpaceSettings(space().id) },
                { id: `rail-space-pages:${space().id}`, label: "Pages", onPress: () => goPages({ kind: "home", spaceId: space().id }) },
              ]}
            />
          )}
        </Each>
        <RailButton
          testID="rail-new-space"
          label="New Space"
          icon="plus"
          onPress={() => {
            addingSpace.set(true);
            navigationPanel.toggle();
          }}
        />
      </ScrollArea>
      <RailButton testID="rail-settings" label="Settings" icon="settings" dot={settingsPageDot("about")} dotTestID="settings-update-dot"
        selected={settingsOpen()} onPress={() => openSettings()} />
      <AccountFooter compact />
    </Surface>
  );
}

/** A Space's tile in the rail: the first letter or digit of its name. */
export function spaceInitial(name: string): string {
  const ch = [...name.trim()].find((c) => /[\p{L}\p{N}]/u.test(c));
  return (ch ?? "#").toLocaleUpperCase();
}

/** The new-Space form is open (the rail's New Space opens the panel on it). */
const addingSpace = signal(false);

const activeRunCount = () => runs().filter(isActiveRun).length;
/** An agent run is going on in this Space (from the runs the app already loaded). */
const spaceRunning = (sid: string) => runs().some((r) => r.spaceId === sid && isActiveRun(r));

function SpacesSection(props: { visible: boolean }) {
  const adding = addingSpace;
  const name = signal("");
  const spaceExpanded = (sid: string) => (mainPlace() === "space" && activeSpaceId() === sid)
    || (mainPlace() === "pages" && pagesRoute().spaceId === sid);
  return (
    <Column gap={1} padY={2}>
      {/* The same 12-point inset as the rows' icons; + shows on hover and focus. */}
      <SectionHeader
        label="Spaces"
        action={{
          icon: adding() ? "close" : "plus",
          label: adding() ? "Cancel new Space" : "New Space",
          tooltip: adding() ? "Cancel" : "New Space",
          testID: "new-space-toggle",
          shown: adding() || spaces().length === 0,
          onPress: () => adding.set(!adding()),
        }}
      />
      <If when={!creatingSpace() && runnerOfferCount()}>
        {(count) => (
          <Banner testID="runner-offer" tone="neutral" variant="inline" title="Use this computer's runner?"
            message={`${count()} of your Spaces ${count() === 1 ? "has" : "have"} no runner. Attaching lets their agents list and read files in a folder on this computer.`}>
            <Row gap={1} wrap>
              <Action compact testID="runner-offer-choose" label="Choose Spaces…" variant="ghost"
                onPress={() => { if (!creatingSpace() && runnerOfferCount()) openSettings("runners"); }} />
              <Action compact testID="runner-offer-dismiss" label="Not now" variant="ghost"
                onPress={() => { if (!creatingSpace() && runnerOfferCount()) dismissRunnerOffer(); }} />
            </Row>
          </Banner>
        )}
      </If>
      <If when={adding()}>
        <Column padX={2} padY={1} gap={2}>
          <CreateSpaceForm name={name} setName={name.set} visible={props.visible}
            onCreated={() => { name.set(""); adding.set(false); }} />
        </Column>
      </If>
      <Each of={spaces()} key={(space) => space.id}>
        {(space) => (
          <Column gap={1}>
            <NavItem
              testID={`space:${space().id}`}
              label={space().name}
              tile={{ initial: spaceInitial(space().name), seed: space().id }}
              expanded={spaceExpanded(space().id)}
              dot={spaceRunning(space().id) ? "the agent is working here" : undefined}
              tooltip={space().role === "owner" ? `${space().name} · you own this Space` : undefined}
              emphasis={space().id === activeSpaceId()}
              selected={false}
              onPress={() => void selectSpace(space().id)}
              menu={{
                testID: `space-menu:${space().id}`,
                accessibilityLabel: `More for ${space().name}`,
                items: [
                  { id: `space-settings:${space().id}`, label: "Space settings", onPress: () => openSpaceSettings(space().id) },
                  { id: `space-agents:${space().id}`, label: "Agents", onPress: () => openSpaceSettings(space().id, "agents") },
                  { id: `space-prefs:${space().id}`, label: "Your preferences", onPress: () => openSpaceSettings(space().id, "preferences") },
                  { id: `space-menu-pages:${space().id}`, label: "Pages", onPress: () => goPages({ kind: "home", spaceId: space().id }) },
                ],
              }}
            />
            <If when={spaceExpanded(space().id)}>
              <If when={space().id === activeSpaceId()}><RoomTree /></If>
              <NavGroup>
                <NavItem testID={`space-pages:${space().id}`} label="Pages" icon="file" depth={1}
                  selected={selectedPages(space().id)}
                  onPress={() => goPages({ kind: "home", spaceId: space().id })} />
                <If when={mainPlace() === "pages" && pagesRoute().spaceId === space().id}>
                  <Each of={pages().filter(page => page.space_id === space().id)} key={page => page.id}>
                    {page => <NavItem testID={`space-page:${page().id}`} label={[page().icon, page().title].filter(Boolean).join(" ")}
                      icon="file" depth={2} selected={false}
                      onPress={() => goPages({ kind: "page", id: page().id, spaceId: space().id })} />}
                  </Each>
                </If>
              </NavGroup>
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

function selectedPages(spaceId?: string) {
  const route = pagesRoute();
  return mainPlace() === "pages" && route.spaceId === spaceId
    && (route.kind !== "page" || !pages().some(p => p.id === route.id && p.pinned));
}

function selectedPage(id: string) {
  const route = pagesRoute();
  return mainPlace() === "pages" && route.kind === "page" && route.id === id;
}

function PinnedPages() {
  const pinned = () => {
    const names = new Map(spaces().map(s => [s.id, s.name]));
    return pages().filter(p => p.pinned).sort((a, b) =>
      Number(!!a.space_id) - Number(!!b.space_id)
      || (names.get(a.space_id ?? "") ?? "").localeCompare(names.get(b.space_id ?? "") ?? "")
      || (a.position ?? Number.MAX_SAFE_INTEGER) - (b.position ?? Number.MAX_SAFE_INTEGER)
      || a.title.localeCompare(b.title));
  };
  return <If when={pinned().length}>
    <NavGroup testID="pinned-pages">
      <Each of={pinned()} key={p => p.id}>{p =>
        <NavItem depth={1} icon="file" label={[p().icon, p().title].filter(Boolean).join(" ")}
          testID={`pinned-page:${p().id}`} selected={selectedPage(p().id)}
          onPress={() => goPages({ kind: "page", id: p().id, spaceId: p().space_id ?? undefined })} />
      }</Each>
    </NavGroup>
  </If>;
}

type TreeRow = { room: Room; depth: number };

/** Rooms in server order, each thread right under the room it was delegated from. */
export function roomTree(list: readonly Room[]): TreeRow[] {
  list = list.filter(room => room.anchorEventId === undefined);
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
    <NavGroup testID="room-list">
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
    </NavGroup>
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

/** The connection as the account line shows it: a dot, words only when not online. */
export function connectionStatus(phase: "online" | "retrying" | string): { tone: Tone; label: string; text?: string } {
  if (phase === "online") return { tone: "success", label: "Connection: Online" };
  if (phase === "retrying") return { tone: "warning", label: "Connection: Reconnecting", text: "Reconnecting" };
  return { tone: "neutral", label: "Connection: Connecting", text: "Connecting" };
}

/**
 * Who is signed in (picture, profile name), and where, once: "On this
 * device" for the app-hosted workspace, else the server's host, with the
 * connection's dot beside it. The menu copies the server URL or the user's
 * stable ID (what Space owners add), and signs out. `compact`: only the
 * picture, for the rail.
 */
function AccountFooter(props: { compact?: boolean }) {
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
  const connection = () => connectionStatus(status().phase);
  void loadSavedServers();
  // Item ids are the testIDs: account-menu opens them. The Workspaces group
  // switches straight to this computer's workspace or a saved server. Sign
  // out ends this session and forgets its server's token in memory.
  const items = (): readonly AccountMenuItem[] => [
    ...workspaceItems(),
    ...(settingsPageDot("about") ? [{ id: "account-update", label: "Update available", onPress: () => openSettings("about") }] : []),
    { id: "copy-server-url", label: "Copy server URL", icon: "copy", onPress: () => copy(connectedTo(), "Server URL copied") },
    { id: "copy-id", label: "Copy ID", icon: "copy", onPress: () => copy(me(), "ID copied") },
    { id: "disconnect", label: "Sign out", icon: "logout", destructive: true, onPress: () => signOut() },
  ];
  const where = () => (server().local ? "on this device" : `connected to ${server().title}`);
  // The accessible name ends with "<where>. Account menu": screen readers and
  // the native packaged-app checks (UI Automation, release.yml) rely on it, so
  // the connection goes before the place, never after it.
  return (
    <Column padX={props.compact ? 0 : 2} padY={2} gap={1} testID={props.compact ? "rail-account" : "active-server"}>
      <AccountMenu
        testID={props.compact ? "rail-account-menu" : "account-menu"}
        compact={props.compact}
        accessibilityLabel={`${name()}, ${connection().label}, ${where()}. Account menu`}
        avatar={<PersonAvatar userId={me()} name={name()} size={props.compact ? "md" : "sm"} />}
        name={name()}
        detail={server().title}
        detailIcon={server().local ? "device" : "server"}
        status={{ ...connection(), testID: props.compact ? "rail-connection-state" : "connection-state" }}
        note={me() ?? undefined}
        items={items()}
      />
      <If when={!props.compact}>
        <WorkspaceNotice />
      </If>
      <If when={props.compact ? null : copied()}>
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
