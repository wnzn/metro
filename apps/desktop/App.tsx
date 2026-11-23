import { mount } from "nsh";
import { ConnectScreen } from "./src/features/ConnectScreen";
import { Shell } from "./src/features/Shell";
import { status } from "./src/store";
import { startWorkspace } from "./src/startupWorkspace";

function App() {
  void startWorkspace();
  const signedIn = () => {
    const phase = status().phase;
    return phase === "online" || phase === "retrying";
  };
  return (
    <If when={signedIn()} else={<ConnectScreen />}>
      <Shell />
    </If>
  );
}

mount(App);
