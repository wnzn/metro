import { mount } from "nsh";
import { ConnectScreen } from "./src/features/ConnectScreen";
import { Shell } from "./src/features/Shell";
import { pickAttachments } from "./src/features/attachments/attachmentSend";
import { startWorkspace } from "./src/startupWorkspace";
import { spaceGone, status } from "./src/store";
import { Action, Row, Surface } from "./src/ui";

// Keep production navigation, membership refresh and attachment handling.
// Only the refresh trigger and the held native picker are test controls.
function StartupAccessProbe() {
  void startWorkspace();
  let finishPick: (() => void) | undefined;
  return (
    <Surface variant="panel">
      <If when={status().phase === "online" || status().phase === "retrying"} else={<ConnectScreen />}>
        <Row gap={2}>
          <Action testID="probe-refresh" label="Probe membership" onPress={() => void spaceGone("acme", "Acme left")} />
          <Action testID="probe-hold-pick" label="Hold file picker" onPress={() => void pickAttachments(() => new Promise(resolve => {
            finishPick = () => {
              const file = new Blob(["personal draft"], { type: "text/plain" });
              resolve([{ name: "personal.txt", size: file.size, status: "read", file }]);
            };
          }))} />
          <Action testID="probe-finish-pick" label="Finish file picker" onPress={() => finishPick?.()} />
        </Row>
        <Shell />
      </If>
    </Surface>
  );
}
mount(StartupAccessProbe);
