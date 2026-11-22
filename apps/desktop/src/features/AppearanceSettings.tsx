// Settings › Appearance: Dark (the default), Light or System. The choice is
// kept on this device (no secrets) and applies to every screen at once,
// because every screen draws with the same semantic tokens.
//
// The preview draws one of each kind of element with the real primitives, so
// it shows exactly what the rest of the app looks like in the current scheme.
// Nothing in it acts; its controls only change the preview's own state.

import { signal } from "nsh";
import {
  Action,
  ActionBar,
  APPEARANCE_OPTIONS,
  appearancePreference,
  AuthorName,
  AuthorTag,
  Avatar,
  Badge,
  Banner,
  Chip,
  Column,
  Field,
  Label,
  MessageFrame,
  MessageHeader,
  PageHeader,
  resolvedAppearance,
  Row,
  ScrollArea,
  SegmentedControl,
  setAppearancePreference,
  SettingRow,
  SettingsGroup,
  Surface,
  Timestamp,
  Toggle,
} from "../ui";

export function AppearanceSettings() {
  const shown = () => `${resolvedAppearance() === "dark" ? "Dark" : "Light"}, from your system`;
  const chip = signal<"all" | "mine">("all");
  const toggle = signal(true);
  const field = signal("");
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
          <If when={appearancePreference() === "system"}>
            <SettingRow label="Showing now" description="Changes when your operating system does.">
              <Label color="muted" testID="appearance-shown">
                {shown()}
              </Label>
            </SettingRow>
          </If>
        </SettingsGroup>

        <SettingsGroup title="Preview" description="One of each kind of element, in the current scheme. Nothing here does anything.">
          <Column pad={4} gap={4} testID="appearance-preview">
            <Column gap={2}>
              <Label variant="strong">Buttons</Label>
              <Row gap={2} wrap align="center">
                <Action label="Primary" variant="primary" onPress={() => {}} accessibilityLabel="Preview of a primary button" />
                <Action label="Secondary" onPress={() => {}} accessibilityLabel="Preview of a secondary button" />
                <Action label="Ghost" variant="ghost" onPress={() => {}} accessibilityLabel="Preview of a quiet button" />
                <Action label="Remove" variant="destructive" onPress={() => {}} accessibilityLabel="Preview of a destructive button" />
                <Action label="Delete" variant="danger" onPress={() => {}} accessibilityLabel="Preview of a confirm-delete button" />
                <Action label="Disabled" variant="primary" disabled onPress={() => {}} accessibilityLabel="Preview of a disabled button" />
                <Action label="Compact" compact onPress={() => {}} accessibilityLabel="Preview of a compact button" />
              </Row>
            </Column>

            <Column gap={2}>
              <Label variant="strong">Badges and chips</Label>
              <Row gap={2} wrap align="center">
                <Badge tone="agent" label="agent" />
                <Badge tone="runner" label="runner" />
                <Badge tone="success" label="online" />
                <Badge tone="warning" label="waiting" />
                <Badge tone="danger" label="failed" />
                <Badge tone="system" label="system" />
                <Badge tone="neutral" label="neutral" />
                <Badge tone="neutral" variant="outline" label="outline" />
              </Row>
              <Row gap={2} wrap align="center">
                <Chip label="All" selected={chip() === "all"} onPress={() => chip.set("all")} accessibilityLabel="Preview of a chip: All" />
                <Chip label="Mine" selected={chip() === "mine"} onPress={() => chip.set("mine")} accessibilityLabel="Preview of a chip: Mine" />
              </Row>
            </Column>

            <Column gap={2}>
              <Label variant="strong">Fields</Label>
              <Row gap={3} align="center">
                <Field value={field()} onChange={(v) => field.set(v)} placeholder="A text field" grow accessibilityLabel="Preview of a text field" />
                <Toggle label="Preview of a switch" checked={toggle()} onChange={(v) => toggle.set(v)} />
              </Row>
            </Column>

            <Column gap={2}>
              <Label variant="strong">Messages</Label>
              <Surface variant="card" padY={2} testID="appearance-sample-message">
                <MessageFrame gutter={<Avatar initials="LN" tone="runner" />}>
                  <MessageHeader>
                    <AuthorName name="Leo Navarro" />
                    <AuthorTag label="you" />
                    <Timestamp text="09:41" full="Monday 09:41" />
                  </MessageHeader>
                  <Label>The new layout is ready for review.</Label>
                </MessageFrame>
                <MessageFrame gutter={<Avatar initials="A" tone="agent" />}>
                  <MessageHeader>
                    <AuthorName name="Agent" agent />
                    <Timestamp text="09:42" full="Monday 09:42" />
                  </MessageHeader>
                  <Label>I checked both themes. Nothing to fix.</Label>
                </MessageFrame>
              </Surface>
            </Column>

            <Column gap={2}>
              <Label variant="strong">Feedback</Label>
              <Banner variant="inline" tone="success" title="Saved" message="Your changes are in effect." />
              <Banner variant="inline" tone="warning" message="The runner is offline. Jobs wait until it's back." />
              <Banner variant="inline" tone="danger" title="Couldn't connect" message="Check the address and try again." />
            </Column>

            <Column gap={2}>
              <Label variant="strong">Surfaces</Label>
              <Row gap={3} wrap>
                <Surface variant="card" pad={3} gap={1}>
                  <Label variant="heading">Card</Label>
                  <Label variant="caption" color="muted">
                    Flat, with a border.
                  </Label>
                </Surface>
                <Surface variant="popover" pad={3} gap={1}>
                  <Label variant="heading">Menu</Label>
                  <Label variant="caption" color="muted">
                    One step above, with a shadow.
                  </Label>
                </Surface>
              </Row>
              <Label variant="caption" color="muted">
                Body text uses the foreground color; quieter text like this uses the muted one.
              </Label>
              <ActionBar>
                <Label variant="meta">Updated 09:42</Label>
              </ActionBar>
            </Column>
          </Column>
        </SettingsGroup>
      </Surface>
    </ScrollArea>
  );
}
