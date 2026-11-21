import { place, view } from "../store";
import { Surface } from "../ui";
import { ChatView } from "./ChatView";
import { ManageSpace } from "./ManageSpace";
import { Sidebar } from "./Sidebar";
import { SpaceView } from "./SpaceView";

/** The signed-in app: sidebar plus the Chat, a Space's room, or its management. */
export function Shell() {
  return (
    <Surface variant="app" testID="workspace">
      <Sidebar />
      <If when={place() === "space"} else={<ChatView />}>
        <If when={view() === "manage"} else={<SpaceView />}>
          <ManageSpace />
        </If>
      </If>
    </Surface>
  );
}
