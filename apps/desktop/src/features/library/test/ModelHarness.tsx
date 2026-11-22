// Test-only entry: the real connect screen, then the media model choice
// (MediaModelSettings, which Settings › Model providers mounts) above the
// real Library page, so the choice and its effect on Index are tested in this
// lane without depending on where Settings places it.

import { mount } from "nsh";
import { restorePreferences, status } from "../../../store";
import { Column, ScrollArea, Surface } from "../../../ui";
import { ConnectScreen } from "../../ConnectScreen";
import { LibraryHome, MediaModelSettings } from "../LibraryView";

function Harness() {
  void restorePreferences();
  const signedIn = () => status().phase === "online" || status().phase === "retrying";
  return (
    <If when={signedIn()} else={<ConnectScreen />}>
      <Surface variant="app">
        <Column grow>
          <ScrollArea pad={4}>
            <MediaModelSettings />
          </ScrollArea>
          <LibraryHome />
        </Column>
      </Surface>
    </If>
  );
}

mount(Harness);
