import { cleanup, mount } from "nsh";
import { AboutSettings } from "./src/features/AboutSettings";
import { ConnectScreen } from "./src/features/ConnectScreen";
import { registerSettingsPage } from "./src/features/SettingsView";
import { Shell } from "./src/features/Shell";
import { WebSearchSettings } from "./src/features/WebSearchSettings";
import { me, openSettings, settingsAnchor, status } from "./src/store";
import { startWorkspace } from "./src/startupWorkspace";
import { Surface } from "./src/ui";
import { desktopUpdates, updateAvailable } from "./src/updates";
import { registerWebSearchSettingsAction } from "./src/webSearch";

registerSettingsPage("about", {
  Page: () => <AboutSettings focusPrivacy={settingsAnchor() === "privacy"} />,
  dot: () => updateAvailable() ? "Update available" : undefined,
});
registerSettingsPage("websearch", {
  Page: () => <WebSearchSettings
    onOpenAbout={() => openSettings("about")}
    onOpenPrivacy={() => openSettings("about-privacy")} />,
});

function SignedInWorkspace() {
  cleanup(registerWebSearchSettingsAction(() => { if (me()) openSettings("websearch"); }));
  return <Shell />;
}

function App() {
  void startWorkspace();
  const signedIn = () => {
    const phase = status().phase;
    return phase === "online" || phase === "retrying";
  };
  return (
    <Surface variant="panel" onLaidOut={desktopUpdates.start}>
      <If when={signedIn()} else={<ConnectScreen />}>
        <SignedInWorkspace />
      </If>
    </Surface>
  );
}

mount(App);
