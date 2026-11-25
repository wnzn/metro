import { mount } from "nsh";
import { ConnectScreen } from "./src/features/ConnectScreen";
import { Shell } from "./src/features/Shell";
import { status } from "./src/store";
import { startWorkspace } from "./src/startupWorkspace";
import { Surface } from "./src/ui";
import { desktopUpdates } from "./src/updates";

function App() {
  void startWorkspace();
  const signedIn = () => {
    const phase = status().phase;
    return phase === "online" || phase === "retrying";
  };
  return (
    <Surface variant="panel" onLaidOut={desktopUpdates.start}>
      <If when={signedIn()} else={<ConnectScreen />}>
        <Shell />
      </If>
    </Surface>
  );
}

mount(App);
