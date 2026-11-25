import { effect, mount, signal, untracked } from "nsh";
import { BASE, TOKENS } from "../../../test/fakeServer";
import { dismissRunnerOffer, installedRunnerChoice, runnerOfferCount, watchInstalledRunner } from "../../runnersState";
import { connect, me } from "../../store";
import { Action, Column, Label } from "../../ui";
import { RunnersSettings } from "../RunnersSettings";

function Consumer() {
  watchInstalledRunner();
  return null;
}

function FolderConsumer() {
  watchInstalledRunner(true);
  return null;
}

function WatchFixture() {
  const active = signal(true);
  const settings = signal(false);
  const visibleCount = signal(0);
  let shown = false;
  effect(() => {
    const visible = runnerOfferCount() > 0;
    if (visible && !shown) visibleCount.set(untracked(visibleCount) + 1);
    shown = visible;
  });
  return (
    <Column grow testID="watch-fixture">
      <If when={active()}>
        <Consumer />
        <Consumer />
      </If>
      <Label>{`Offer appearances: ${visibleCount()}`}</Label>
      <Label>{`Create choice: ${installedRunnerChoice()?.id ?? "none"}`}</Label>
      <If when={runnerOfferCount()}>
        <Action testID="dismiss-fixture-offer" label="Not now" onPress={dismissRunnerOffer} />
      </If>
      <Action testID="toggle-watchers" label="Toggle watches" onPress={() => active.set(!active())} />
      <Action testID="toggle-runner-settings" label="Toggle runner settings" onPress={() => settings.set(!settings())} />
      <If when={settings()}>
        <FolderConsumer />
        <RunnersSettings />
      </If>
    </Column>
  );
}

function App() {
  return <If when={me()} else={<Action testID="fixture-connect" label="Connect fixture" onPress={() => void connect(BASE, TOKENS.alice!)} />}>
    <WatchFixture />
  </If>;
}

mount(App);
