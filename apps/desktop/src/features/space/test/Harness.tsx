// Test-only entry: the real connect screen and Shell (which holds the Space
// settings modal), plus a button that opens the settings for the active
// Space, so the pages are tested without going through a menu.

import { mount } from "nsh";
import { activeSpaceId, restorePreferences, status } from "../../../store";
import { Action, Row, Surface } from "../../../ui";
import { ConnectScreen } from "../../ConnectScreen";
import { Shell } from "../../Shell";
import { openSpaceSettings } from "../spaceSettingsState";

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
    </If>
  );
}

mount(Harness);
