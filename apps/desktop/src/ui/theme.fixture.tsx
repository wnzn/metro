// A test-only entry (theme.test.tsx opens it): the real AppearanceSettings
// inside the same surfaces the app uses, plus one of each shared primitive,
// so the tests can read the colors NSH actually shows and drive the new
// primitives. Not part of the app bundle.

import { mount, signal } from "nsh";
import { AppearanceSettings } from "../features/AppearanceSettings";
import { Action, ActionsMenu, Banner, Column, Disclosure, Label, NavItem, Picker, Row, Surface } from "./index";

function Fixture() {
  const last = signal("none");
  const open = signal(false);
  const picked = signal<string | null>(null);
  return (
    <Surface variant="app" testID="fx-app">
      <Surface variant="sidebar" pad={2} gap={1} testID="fx-sidebar">
        <NavItem testID="fx-nav" label="Appearance" prefix="" selected onPress={() => {}} />
        <NavItem label="Runner" prefix="" selected={false} onPress={() => {}} />
      </Surface>
      <Surface variant="panel" testID="fx-panel">
        <Column pad={4} gap={2}>
          <Surface variant="card" pad={3} testID="fx-card">
            <Label testID="fx-card-text">Fixture card</Label>
            <Label testID="fx-card-muted" color="muted" variant="caption">
              Fixture quiet card
            </Label>
          </Surface>
          <Label testID="fx-body">Fixture body</Label>
          <Label testID="fx-muted" color="muted">
            Fixture quiet page
          </Label>
          <Row gap={2} align="center">
            <Action testID="fx-primary" label="Primary" variant="primary" onPress={() => {}} />
            <Action testID="fx-disabled" label="Fixture off" variant="primary" disabled onPress={() => {}} />
            <Action testID="fx-destructive" label="Fixture remove" variant="destructive" onPress={() => last.set("remove")} />
            <Action testID="fx-danger" label="Fixture delete" variant="danger" onPress={() => {}} />
            <Action testID="fx-edge" label="Fixture edge" variant="ghost" compact edgeAligned onPress={() => {}} />
            <ActionsMenu
              testID="fx-menu"
              label="Fixture more"
              items={[
                { id: "fx-menu-rename", label: "Fixture rename", onPress: () => last.set("rename") },
                { id: "fx-menu-off", label: "Fixture unavailable", onPress: () => last.set("off"), disabled: true },
                { id: "fx-menu-delete", label: "Fixture delete item", onPress: () => last.set("delete"), destructive: true },
              ]}
            />
          </Row>
          <Label testID="fx-last">{`Last: ${last()}`}</Label>
          <Banner testID="fx-banner" variant="inline" tone="danger" title="Fixture failed" message="Fixture banner message" />
          <Disclosure testID="fx-disclosure" title="Fixture details" summary="2 items" open={open()} onToggle={(v) => open.set(v)}>
            <Label>Fixture hidden content</Label>
          </Disclosure>
          <Picker
            testID="fx-picker"
            accessibilityLabel="Fixture picker"
            items={[
              { value: "a", label: "Fixture alpha", detail: "Fixture alpha detail" },
              { value: "b", label: "Fixture beta" },
            ]}
            value={picked()}
            onChange={(v) => picked.set(v)}
            placeholder="Fixture choose"
          />
        </Column>
        <AppearanceSettings />
      </Surface>
    </Surface>
  );
}

mount(Fixture);
