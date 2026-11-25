import { mount, signal, Storage } from "nsh";
import { readBridgeEnv } from "../src/device/env";
import { AboutSettings } from "../src/features/AboutSettings";
import { Action, ActionBar, Column, Label } from "../src/ui";
import { createDesktopUpdates } from "../src/updates";

function Harness() {
  const parked = signal(false);
  const settled = signal(false);
  let finish: (succeed: boolean) => void = () => {};
  const env = readBridgeEnv();
  const updates = createDesktopUpdates(env.present ? env.endpoint : null, {
    async getItem(key) {
      const value = await Storage.getItem(key);
      // Hold the read in app JS, not in the native module stub queue: the
      // updater bridge must be able to answer independently of storage.
      return new Promise((resolve, reject) => {
        finish = succeed => {
          parked.set(false);
          settled.set(true);
          if (succeed) resolve(value);
          else reject(new Error("Fixture storage unavailable"));
        };
        parked.set(true);
      });
    },
    setItem: (key, value) => Storage.setItem(key, value),
  });
  return <Column grow>
    <ActionBar>
      <Action label="Finish storage read" testID="h-finish-storage" disabled={!parked()} onPress={() => finish(true)} />
      <Action label="Fail storage read" testID="h-fail-storage" disabled={!parked()} onPress={() => finish(false)} />
      <Action label="Poll updater" testID="h-poll-updater" onPress={() => void updates.refresh()} />
    </ActionBar>
    <If when={parked()}><Label testID="h-storage-held">Storage read held</Label></If>
    <If when={settled()}><Label testID="h-storage-settled">Storage replied</Label></If>
    <AboutSettings updates={updates} />
  </Column>;
}

mount(Harness);
