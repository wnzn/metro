// Settings: a modal over whatever the user was doing (their Chat, a Space's
// room or its management, Search, Scheduled tasks). Closing it (Close,
// Escape, the backdrop) returns there as it was: the place, Space, room,
// draft and scroll are never touched, because the main panel stays mounted
// underneath. Its sections are its own navigation, side by side on a wide
// window and as a row of tabs on a narrow one; the app's sidebar only opens it.
// Each page owns its scroll area and loads its data when opened
// (src/settings.ts, src/routing.ts).

import { closeSettings, openSettings, settingsOpen, settingsSection } from "../store";
import type { SettingsSection } from "../store";
import { Column, compactWidth, IconButton, Label, Modal, ModalNav, ModalTab, ModalTabRow, Row, Spacer, Surface } from "../ui";
import type { IconName } from "../ui";
import { AppearanceSettings } from "./AppearanceSettings";
import { ConnectionBanner } from "./Conversation";
import { ProfileSettings } from "./profile/ProfileSettings";
import { ProvidersSettings } from "./ProvidersSettings";
import { RoutingSettings } from "./RoutingSettings";
import { RunnersSettings } from "./RunnersSettings";

export const SETTINGS_PAGES: readonly { section: SettingsSection; label: string; icon: IconName }[] = [
  { section: "profile", label: "Profile", icon: "person" },
  { section: "runners", label: "Runners", icon: "runner" },
  { section: "providers", label: "Model providers", icon: "providers" },
  { section: "routing", label: "Model routing", icon: "routing" },
  { section: "appearance", label: "Appearance", icon: "appearance" },
];

/** The Settings modal. Mounted once by the Shell; shown while Settings is open. */
export function SettingsModal() {
  return (
    <Modal open={settingsOpen()} title="Settings" onClose={closeSettings} testID="settings-dialog">
      {/* Unmounted while closed: a typed key or a half-filled form never outlives the modal. */}
      <If when={settingsOpen()}>
        <If when={compactWidth()} else={<WideSettings />}>
          <CompactSettings />
        </If>
      </If>
    </Modal>
  );
}

function WideSettings() {
  return (
    <Row grow testID="settings">
      <ModalNav testID="settings-nav" accessibilityLabel="Settings sections">
        <Row padX={2} padY={2}>
          <Label variant="heading" heading>
            Settings
          </Label>
        </Row>
        <Tabs compact={false} />
      </ModalNav>
      <Column grow>
        <Row align="center" padX={3} padY={2}>
          <Spacer />
          <CloseButton />
        </Row>
        <SettingsPage />
      </Column>
    </Row>
  );
}

function CompactSettings() {
  return (
    <Column grow testID="settings">
      <Row align="center" padX={4} padY={2} gap={2}>
        <Label variant="heading" heading>
          Settings
        </Label>
        <Spacer />
        <CloseButton />
      </Row>
      <ModalTabRow testID="settings-nav">
        <Tabs compact />
      </ModalTabRow>
      <SettingsPage />
    </Column>
  );
}

function Tabs(props: { compact: boolean }) {
  return (
    <Each of={SETTINGS_PAGES} key={(p) => p.section}>
      {(p) => (
        <ModalTab
          testID={`nav-settings:${p().section}`}
          label={p().label}
          icon={p().icon}
          compact={props.compact}
          selected={settingsSection() === p().section}
          onPress={() => openSettings(p().section)}
        />
      )}
    </Each>
  );
}

function CloseButton() {
  return <IconButton testID="settings-close" icon="close" accessibilityLabel="Close Settings" tooltip="Close (Esc)" onPress={closeSettings} />;
}

function SettingsPage() {
  return (
    <Surface variant="panel" grow testID="settings-page">
      <ConnectionBanner />
      {/* Profile (./profile, its own lane) loads when shown and resets at sign-out. */}
      <If when={settingsSection() === "profile"}>
        <ProfileSettings />
      </If>
      <If when={settingsSection() === "runners"}>
        <RunnersSettings />
      </If>
      <If when={settingsSection() === "providers"}>
        <ProvidersSettings />
      </If>
      <If when={settingsSection() === "routing"}>
        <RoutingSettings />
      </If>
      <If when={settingsSection() === "appearance"}>
        <AppearanceSettings />
      </If>
    </Surface>
  );
}
