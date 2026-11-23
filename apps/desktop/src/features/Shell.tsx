import { cleanup, keymap } from "nsh";
import { mainPlace, openSettings, view } from "../store";
import { Column, compactWidth, modalOpen, navigationPanel, PanelToggle, Surface } from "../ui";
import { ChatView } from "./ChatView";
import { LibraryHome } from "./library/LibraryView";
import { ManageSpace } from "./ManageSpace";
import { ScheduledTasksView } from "./ScheduledTasksView";
import { SearchView } from "./SearchView";
import { SettingsModal } from "./SettingsView";
import { Sidebar } from "./Sidebar";
import { SpaceView } from "./SpaceView";
import { TopSearch } from "./TopSearch";

/**
 * The signed-in app: the sidebar, then the main panel: the search box on
 * top (mod+k focuses it), under it the Chat, Library, Search, Scheduled
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
  return (
    <Surface variant="app" testID="workspace">
      <Sidebar hidden={!navigationPanel.open()} />
      <Column grow testID="main-panel">
        <Surface variant="topbar" testID="top-bar" gap={2}>
          <PanelToggle side="left" name="workspace navigation" caption={compactWidth() ? undefined : "Navigation"} expanded={navigationPanel.open()}
            shortcut="B" testID="toggle-navigation" onPress={navigationPanel.toggle} />
          <Column grow align="center">
            <TopSearch />
          </Column>
        </Surface>
        <If when={mainPlace() === "chat"}>
          <ChatView />
        </If>
        <If when={mainPlace() === "library"}>
          <LibraryHome />
        </If>
        <If when={mainPlace() === "search"}>
          <SearchView />
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
    </Surface>
  );
}
