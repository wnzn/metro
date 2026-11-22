import { keymap } from "nsh";
import { mainPlace, openSettings, view } from "../store";
import { Column, Surface } from "../ui";
import { ChatView } from "./ChatView";
import { ManageSpace } from "./ManageSpace";
import { ScheduledTasksView } from "./ScheduledTasksView";
import { SearchView } from "./SearchView";
import { SettingsModal } from "./SettingsView";
import { Sidebar } from "./Sidebar";
import { SpaceView } from "./SpaceView";
import { TopSearch } from "./TopSearch";

/**
 * The signed-in app: the sidebar, then the main panel: the search box on
 * top, under it the Chat, Search, Scheduled tasks, or a Space's room or its
 * management. Settings opens as a modal over all of it and leaves the main
 * panel mounted, so closing it returns to the same place, draft and scroll.
 */
export function Shell() {
  try {
    keymap.bind("mod+,", () => openSettings());
  } catch {
    /* a platform that keeps this keystroke: the sidebar's Settings still opens it */
  }
  return (
    <Surface variant="app" testID="workspace">
      <Sidebar />
      <Column grow testID="main-panel">
        <Surface variant="topbar" testID="top-bar">
          <TopSearch />
        </Surface>
        <If when={mainPlace() === "chat"}>
          <ChatView />
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
