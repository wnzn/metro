// Test-only entry: the real connect screen and Shell, plus Settings › Profile
// on a button of its own, so this lane's page is tested before the Settings
// modal routes it.

import { mount, signal } from "nsh";
import { restorePreferences, status } from "../../../store";
import { Action, Row, Surface } from "../../../ui";
import { ConnectScreen } from "../../ConnectScreen";
import { Shell } from "../../Shell";
import { ProfileSettings } from "../ProfileSettings";

const showProfile = signal(false);

function Harness() {
  void restorePreferences();
  const signedIn = () => status().phase === "online" || status().phase === "retrying";
  return (
    <If when={signedIn()} else={<ConnectScreen />}>
      <Surface variant="app">
        <Surface variant="panel" grow>
          <Row gap={1}>
            <Action testID="h-profile" label={showProfile() ? "Close profile" : "Profile"} onPress={() => showProfile.set(!showProfile())} />
          </Row>
          <If when={showProfile()} else={<Shell />}>
            <ProfileSettings />
          </If>
        </Surface>
      </Surface>
    </If>
  );
}

mount(Harness);
