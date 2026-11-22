// A test-only entry (theme.test.tsx opens it): the real AppearanceSettings
// inside the same surfaces the app uses, so the tests can read the colors
// NSH actually shows. Not part of the app bundle.

import { mount } from "nsh";
import { AppearanceSettings } from "../features/AppearanceSettings";
import { Action, Column, Label, NavItem, Surface } from "./index";

function Fixture() {
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
          <Action testID="fx-primary" label="Primary" variant="primary" onPress={() => {}} />
        </Column>
        <AppearanceSettings />
      </Surface>
    </Surface>
  );
}

mount(Fixture);
