import { mount, signal } from "nsh";
import { AboutSettings } from "../src/features/AboutSettings";
import { connect, disconnect, me } from "../src/store";
import { desktopUpdates, updateAvailable } from "../src/updates";
import { Action, ActionBar, Column, Label, Surface } from "../src/ui";
import { BASE, TOKENS } from "./fakeServer";

const OTHER = "http://127.0.0.1:8788";

function Harness() {
  const show = signal(false);
  const rendered = signal(false);
  return <Surface variant="panel">
    <Column grow>
      <ActionBar>
        <Action label="Toggle About" testID="h-about" onPress={() => show.set(!show())} />
        <Action label="Render app surface" testID="h-render" onPress={() => rendered.set(true)} />
        <Action label="Poll updater" testID="h-poll-updater" onPress={() => void desktopUpdates.refresh()} />
        <Action label="Alice" testID="h-alice" onPress={() => void connect(BASE, TOKENS.alice)} />
        <Action label="Bob" testID="h-bob" onPress={() => void connect(BASE, TOKENS.bob)} />
        <Action label="Other server" testID="h-other-server" onPress={() => void connect(OTHER, TOKENS.alice)} />
        <Action label="Sign out" testID="h-sign-out" onPress={() => disconnect()} />
      </ActionBar>
      <Label>{me() ?? "Signed out"}</Label>
      <If when={rendered()}><Surface variant="card" pad={4} onLaidOut={desktopUpdates.start}><Label>App rendered</Label></Surface></If>
      <If when={updateAvailable()}><Label testID="h-update-available">Quiet update indicator</Label></If>
      <If when={show()}><AboutSettings /></If>
    </Column>
  </Surface>;
}

mount(Harness);
