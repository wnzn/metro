import { mount } from "nsh";
import { ConnectScreen } from "./src/features/ConnectScreen";
import { Shell } from "./src/features/Shell";
import { restorePreferences, status } from "./src/store";

function App() {
  void restorePreferences();
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
