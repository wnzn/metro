// Test fixture only: exercise optional Settings routes without either feature bundle.
import { mount, signal } from "nsh";
import { openSettings } from "../store";
import { Action, PageHeader, PageLayout, Surface } from "../ui";
import { registerSettingsPage, SettingsModal } from "./SettingsView";

const update = signal(false);
function AboutFixture() {
  return <PageLayout testID="about-fixture"><PageHeader title="Fixture About" /></PageLayout>;
}
registerSettingsPage("about", { Page: AboutFixture, dot: () => update() ? "Update available" : undefined });

function Fixture() {
  return (
    <Surface variant="app">
      <Action testID="uninstalled-route" label="Uninstalled route" onPress={() => openSettings("websearch")} />
      <Action testID="installed-route" label="Installed route" onPress={() => { update.set(true); openSettings("about"); }} />
      <Action testID="legacy-providers-route" label="Legacy providers route" onPress={() => openSettings("providers")} />
      <Action testID="legacy-routing-route" label="Legacy routing route" onPress={() => openSettings("routing")} />
      <SettingsModal />
    </Surface>
  );
}
mount(Fixture);
