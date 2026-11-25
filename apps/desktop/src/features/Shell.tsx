import { cleanup, effect, keymap, untracked } from "nsh";
import { mainPlace, me, openSettings, spaces, view } from "../store";
import { refreshPages } from "../pagesState";
import { CenteredToolbar, Column, compactWidth, modalOpen, navigationPanel, PanelToggle, Surface } from "../ui";
import { ChatView } from "./ChatView";
import { LibraryHome } from "./library/LibraryView";
import { ManageSpace } from "./ManageSpace";
import { PagesView } from "./pages/PagesView";
import { ScheduledTasksView } from "./ScheduledTasksView";
import { SearchView } from "./SearchView";
import { SettingsModal } from "./SettingsView";
import { SpaceSettingsModal } from "./space/SpaceSettings";
import { Sidebar } from "./Sidebar";
import { RoomHeaderActions, RoomHeaderTitle, SpaceView } from "./SpaceView";
import { TopSearch } from "./TopSearch";

/**
 * The signed-in app: the sidebar (or its icon rail while collapsed; nothing
 * on a narrow window), then the main panel: one header row (the sidebar's
 * toggle, a room's breadcrumb and actions, the search box, which mod+k
 * focuses), under it the Chat, Library, Search, Scheduled
 * tasks, or a Space's room or its management. Settings opens as a modal over
 * all of it and leaves the main panel mounted, so closing it returns to the
 * same place, draft and scroll.
 */
export function Shell() {
  try {
    // Removed with the Shell: signing out and in again never stacks it.
    cleanup(keymap.bind("mod+,", () => openSettings()));
    cleanup(keymap.bind("mod+b", () => { if (!modalOpen()) navigationPanel.toggle(); }, { when: "always" }));
  } catch {
    /* a platform that keeps this keystroke: the sidebar's Settings still opens it */
  }
  let lastPagesScope = "";
  effect(() => {
    const scope = JSON.stringify([me(), mainPlace() === "pages", ...spaces().map(s => s.id).sort()]);
    if (scope !== lastPagesScope) {
      lastPagesScope = scope;
      if (me()) untracked(() => void refreshPages());
    }
  });
  // A Space's room puts its breadcrumb and actions in the same header row.
  const inRoom = () => mainPlace() === "space" && view() !== "manage";
  return (
    <Surface variant="app" testID="workspace">
      <Sidebar mode={navigationPanel.open() ? "full" : compactWidth() ? "hidden" : "rail"} />
      <Column grow testID="main-panel">
        <CenteredToolbar
          testID="top-bar"
          leading={
            <PanelToggle side="left" name="workspace navigation" expanded={navigationPanel.open()}
              shortcut="B" testID="toggle-navigation" onPress={navigationPanel.toggle} />
          }
          title={inRoom() ? <RoomHeaderTitle /> : undefined}
          trailing={inRoom() ? <RoomHeaderActions /> : undefined}
        >
          <TopSearch />
        </CenteredToolbar>
        <If when={mainPlace() === "chat"}>
          <ChatView />
        </If>
        <If when={mainPlace() === "library"}>
          <LibraryHome />
        </If>
        <If when={mainPlace() === "search"}>
          <SearchView />
        </If>
        <If when={mainPlace() === "pages"}>
          <PagesView />
        </If>
        <If when={mainPlace() === "schedules"}>
          <ScheduledTasksView />
        </If>
        <If when={mainPlace() === "space"}>
          <If when={view() === "manage"} else={<SpaceView />}>
            <ManageSpace />
          </If>
        </If>
      </Column>
      <SettingsModal />
      <SpaceSettingsModal />
    </Surface>
  );
}
