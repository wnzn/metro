// Settings › Appearance: Dark (the default), Light or System. The choice is
// kept on this device (no secrets) and applies to every screen at once,
// because every screen draws with the same semantic tokens.

import {
  Action,
  APPEARANCE_OPTIONS,
  appearancePreference,
  Badge,
  Column,
  Label,
  PageHeader,
  resolvedAppearance,
  Row,
  ScrollArea,
  SegmentedControl,
  setAppearancePreference,
  SettingRow,
  SettingsGroup,
  Surface,
} from "../ui";

export function AppearanceSettings() {
  const shown = () => {
    const scheme = resolvedAppearance() === "dark" ? "Dark" : "Light";
    return appearancePreference() === "system" ? `${scheme}, from your system` : scheme;
  };
  return (
    <ScrollArea pad={6} testID="appearance-settings">
      <Surface variant="content" gap={6}>
        <PageHeader title="Appearance" subtitle="How Metro looks on this device. Saved locally." />

        <SettingsGroup title="Theme">
          <SettingRow label="Color scheme" description="Dark is the default. System follows your operating system.">
            <SegmentedControl
              testID="appearance"
              accessibilityLabel="Color scheme"
              options={APPEARANCE_OPTIONS}
              value={appearancePreference()}
              onChange={setAppearancePreference}
            />
          </SettingRow>
          <SettingRow label="Showing now">
            <Label color="muted" testID="appearance-shown">
              {shown()}
            </Label>
          </SettingRow>
        </SettingsGroup>

        <SettingsGroup title="Preview" description="The colors every screen uses, in the current scheme.">
          <Column pad={4} gap={3} testID="appearance-preview">
            <Row gap={2} wrap align="center">
              <Badge tone="agent" label="agent" />
              <Badge tone="runner" label="runner" />
              <Badge tone="success" label="online" />
              <Badge tone="warning" label="waiting" />
              <Badge tone="danger" label="failed" />
            </Row>
            <Row gap={2} align="center">
              <Action label="Primary" variant="primary" onPress={() => {}} accessibilityLabel="Preview of a primary button" />
              <Action label="Secondary" onPress={() => {}} accessibilityLabel="Preview of a secondary button" />
              <Action label="Ghost" variant="ghost" onPress={() => {}} accessibilityLabel="Preview of a quiet button" />
            </Row>
            <Label variant="caption" color="muted">
              Body text uses the foreground color; quieter text like this uses the muted one.
            </Label>
          </Column>
        </SettingsGroup>
      </Surface>
    </ScrollArea>
  );
}
