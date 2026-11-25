// Development-only gallery for checking both palettes against real controls.
import { mount, signal } from "nsh";
import { Action, ActionBar, AuthorGlyph, AuthorName, AuthorTag, Avatar, Badge, Banner, Chip, Column, Field, Label, MessageFrame, MessageHeader, PageLayout, Row, SettingsGroup, Surface, Timestamp, Toggle } from "./index";

function Gallery() {
  const chip = signal("all");
  const toggle = signal(true);
  const field = signal("");
  return (
    <PageLayout pad={6} gap={6} testID="appearance-preview">
      <SettingsGroup title="Buttons">
        <Row gap={2} wrap pad={4}>
          <Action label="Primary" variant="primary" onPress={() => {}} accessibilityLabel="Preview of a primary button" />
          <Action label="Secondary" onPress={() => {}} accessibilityLabel="Preview of a secondary button" />
          <Action label="Ghost" variant="ghost" onPress={() => {}} accessibilityLabel="Preview of a quiet button" />
          <Action label="Remove" variant="destructive" onPress={() => {}} accessibilityLabel="Preview of a destructive button" />
          <Action label="Delete" variant="danger" onPress={() => {}} accessibilityLabel="Preview of a confirm-delete button" />
          <Action label="Disabled" variant="primary" disabled onPress={() => {}} accessibilityLabel="Preview of a disabled button" />
          <Action label="Compact" compact onPress={() => {}} accessibilityLabel="Preview of a compact button" />
        </Row>
      </SettingsGroup>
      <SettingsGroup title="Badges and chips">
        <Column pad={4} gap={2}>
          <Row gap={2} wrap align="center">
            <Badge tone="agent" label="Agent" />
            <Badge tone="runner" label="Runner" />
            <Badge tone="success" label="Online" />
            <Badge tone="warning" label="Waiting" />
            <Badge tone="danger" label="Failed" />
            <Badge tone="system" label="System" />
            <Badge tone="neutral" label="Neutral" />
            <Badge tone="neutral" variant="outline" label="Outline" />
          </Row>
          <Row gap={2} wrap align="center">
            <Chip label="All" selected={chip() === "all"} onPress={() => chip.set("all")} accessibilityLabel="Preview of a chip: All" />
            <Chip label="Mine" selected={chip() === "mine"} onPress={() => chip.set("mine")} accessibilityLabel="Preview of a chip: Mine" />
          </Row>
        </Column>
      </SettingsGroup>
      <SettingsGroup title="Fields">
        <Row pad={4} gap={3} align="center">
          <Field value={field()} onChange={field.set} placeholder="A text field" grow accessibilityLabel="Preview of a text field" />
          <Toggle label="Preview of a switch" checked={toggle()} onChange={toggle.set} />
        </Row>
      </SettingsGroup>
      <SettingsGroup title="Messages" testID="appearance-sample-message">
        <MessageFrame gutter={<Avatar initials="LN" tone="runner" />}>
          <MessageHeader><AuthorName name="Leo Navarro" /><AuthorTag label="you" /><Timestamp text="09:41" full="Monday 09:41" /></MessageHeader>
          <Label>The new layout is ready for review.</Label>
        </MessageFrame>
        <MessageFrame gutter={<AuthorGlyph kind="agent" tone="agent" />}>
          <MessageHeader><AuthorName name="Agent" agent /><Timestamp text="09:42" full="Monday 09:42" /></MessageHeader>
          <Label>I checked both themes. Nothing to fix.</Label>
        </MessageFrame>
      </SettingsGroup>
      <SettingsGroup title="Feedback">
        <Column pad={4} gap={2}>
          <Banner variant="inline" tone="success" title="Saved" message="Your changes are in effect." />
          <Banner variant="inline" tone="warning" message="The runner is offline. Jobs wait until it's back." />
          <Banner variant="inline" tone="danger" title="Couldn't connect" message="Check the address and try again." />
          <Label color="danger">An error next to a field uses the danger ink.</Label>
        </Column>
      </SettingsGroup>
      <SettingsGroup title="Surfaces">
        <Column pad={4} gap={2}>
          <Row gap={3} wrap>
            <Surface variant="card" pad={3} gap={1}><Label variant="heading">Card</Label><Label variant="caption" color="muted">Flat, with a border.</Label></Surface>
            <Surface variant="popover" pad={3} gap={1}><Label variant="heading">Menu</Label><Label variant="caption" color="muted">One step above, rounder, with a shadow.</Label></Surface>
          </Row>
          <ActionBar><Label variant="meta">Updated 09:42</Label></ActionBar>
        </Column>
      </SettingsGroup>
    </PageLayout>
  );
}

mount(Gallery);
