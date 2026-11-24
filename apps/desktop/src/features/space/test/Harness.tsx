// Test-only entry: the real connect screen and Shell, plus the Space
// settings modal and a button that opens it for the active Space, so this
// lane's pages are tested before the sidebar's Space menu routes them.

import { mount } from "nsh";
import { activeSpaceId, restorePreferences, status } from "../../../store";
import { Action, Row, Surface } from "../../../ui";
import { ConnectScreen } from "../../ConnectScreen";
import { Shell } from "../../Shell";
import { openSpaceSettings } from "../spaceSettingsState";
import { SpaceSettingsModal } from "../SpaceSettings";

function Harness() {
  void restorePreferences();
  const signedIn = () => status().phase === "online" || status().phase === "retrying";
  return (
    <If when={signedIn()} else={<ConnectScreen />}>
      <Surface variant="app">
        <Surface variant="panel" grow>
          <Row gap={1}>
            <Action testID="h-space-settings" label="Space settings" onPress={() => activeSpaceId() && openSpaceSettings(activeSpaceId()!)} />
          </Row>
          <Shell />
        </Surface>
      </Surface>
      <SpaceSettingsModal />
    </If>
  );
}

mount(Harness);
