import { cleanup, mount, signal } from "nsh";
import { status } from "../../store";
import { Column, Label } from "../../ui";
import { registerWebSearchSettingsAction } from "../../webSearch";
import { ConnectScreen } from "../ConnectScreen";
import { Shell } from "../Shell";

function Fixture() {
  const opened = signal(false);
  cleanup(registerWebSearchSettingsAction(() => opened.set(true)));
  return (
    <If when={status().phase === "online"} else={<ConnectScreen />}>
      <Column grow>
        <If when={opened()}><Label>Opened Web search settings</Label></If>
        <Shell />
      </Column>
    </If>
  );
}
mount(Fixture);
