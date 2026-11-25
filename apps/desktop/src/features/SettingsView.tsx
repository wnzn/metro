// Settings: a modal over whatever the user was doing (their Chat, a Space's
// room or its management, Search, Scheduled tasks). Closing it (Close,
// Escape, the backdrop) returns there as it was: the place, Space, room,
// draft and scroll are never touched, because the main panel stays mounted
// underneath. Its sections are its own navigation, side by side on a wide
// window (an icon rail while collapsed) and as a row of tabs on a narrow
// one, hidden there while collapsed; the app's sidebar only opens it.
// Each page owns its scroll area and loads its data when opened
// (src/settings.ts, src/routing.ts).

import type { JSX } from "nsh";
import { closeSettings, enableSettingsExtension, openSettings, settingsExtensionAvailable, settingsOpen, settingsSection } from "../store";
import type { SettingsExtension, SettingsSection } from "../store";
import { Column, compactWidth, Divider, IconButton, Label, Modal, ModalNav, ModalNavSection, ModalTab, ModalTabRow, PanelToggle, RailButton, Row, settingsPanel, Spacer, Surface } from "../ui";
import type { IconName } from "../ui";
import { AppearanceSettings } from "./AppearanceSettings";
import { ConnectionBanner } from "./Conversation";
import { MonitoringSettings } from "./MonitoringSettings";
import { ProfileSettings } from "./profile/ProfileSettings";
import { ProvidersSettings } from "./ProvidersSettings";
import { ModelChoicesSettings } from "./ModelChoicesSettings";
import { RunnersSettings } from "./RunnersSettings";

type Page = { section: SettingsSection; label: string; icon: IconName; hook?: string };
type ExtensionPage = { Page: () => JSX.Element; dot?: () => string | undefined };
const extensions: Partial<Record<SettingsExtension, ExtensionPage>> = {};
/** Register once at startup, when the implementation is present. No stub tabs or deep links. */
export function registerSettingsPage(section: SettingsExtension, page: ExtensionPage) {
  extensions[section] = page;
  enableSettingsExtension(section);
}

const GROUPS: readonly { id: string; label: string; pages: readonly Page[] }[] = [
  { id: "you", label: "You", pages: [
    { section: "profile", label: "Profile", icon: "person" },
    { section: "connections", label: "Model connections", icon: "providers", hook: "providers" },
    { section: "models", label: "Model choices", icon: "routing", hook: "routing" },
    { section: "runners", label: "Runners", icon: "runner" },
  ] },
  { id: "computer", label: "This computer", pages: [
    { section: "websearch", label: "Web search", icon: "search" },
    { section: "appearance", label: "Appearance", icon: "appearance" },
  ] },
  { id: "metro", label: "Metro", pages: [
    { section: "monitoring", label: "Monitoring", icon: "clock" },
    { section: "about", label: "About & updates", icon: "metro" },
  ] },
];
const isExtension = (section: SettingsSection): section is SettingsExtension => section === "websearch" || section === "about";
const visible = (pages: readonly Page[]) => pages.filter((p) => !isExtension(p.section) || settingsExtensionAvailable(p.section));
const pageDot = (page: Page) => isExtension(page.section) ? extensions[page.section]?.dot?.() : undefined;
const tabHook = (page: Page) => page.hook ?? page.section; // keep existing automation hooks

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
      {/* Collapsed, the sections stay as a rail of their icons. */}
      <If
        when={settingsPanel.open()}
        else={
          <Surface variant="rail" testID="settings-rail" accessibilityLabel="Settings sections">
            <Each of={GROUPS} key={(g) => g.id}>
              {(g, i) => (
                <>
                  <If when={i() > 0}><Divider /></If>
                  <Each of={visible(g().pages)} key={(p) => p.section}>
                    {(p) => (
                      <RailButton
                        testID={`rail-settings:${tabHook(p())}`}
                        label={p().label}
                        icon={p().icon}
                        dot={pageDot(p())}
                        dotTestID={`rail-settings:${tabHook(p())}:dot`}
                        selected={settingsSection() === p().section}
                        onPress={() => openSettings(p().section)}
                      />
                    )}
                  </Each>
                </>
              )}
            </Each>
          </Surface>
        }
      >
        <ModalNav testID="settings-nav" accessibilityLabel="Settings sections">
          <Row padX={3} padY={2}>
            <Label variant="heading" heading>Settings</Label>
          </Row>
          <Tabs compact={false} />
        </ModalNav>
      </If>
      <Column grow>
        <Row align="center" padX={3} padY={2}>
          <SectionsToggle />
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
        <SectionsToggle />
        <Spacer />
        <CloseButton />
      </Row>
      <If when={settingsPanel.open()}>
        <ModalTabRow testID="settings-nav">
          <Tabs compact />
        </ModalTabRow>
      </If>
      <SettingsPage />
    </Column>
  );
}

function SectionsToggle() {
  return <PanelToggle side="left" name="settings sections" expanded={settingsPanel.open()}
    testID="toggle-settings-sections" onPress={settingsPanel.toggle} />;
}

function Tabs(props: { compact: boolean }) {
  return (
    <Each of={GROUPS} key={(g) => g.id}>
      {(g, i) => (
        <>
          <ModalNavSection testID={`settings-group:${g().id}`} label={g().label} compact={props.compact} divided={i() > 0} />
          <Each of={visible(g().pages)} key={(p) => p.section}>
            {(p) => (
              <ModalTab
                testID={`nav-settings:${tabHook(p())}`}
                label={p().label}
                icon={p().icon}
                dot={pageDot(p())}
                compact={props.compact}
                selected={settingsSection() === p().section}
                onPress={() => openSettings(p().section)}
              />
            )}
          </Each>
        </>
      )}
    </Each>
  );
}

function CloseButton() {
  return <IconButton testID="settings-close" icon="close" accessibilityLabel="Close Settings" tooltip="Close (Esc)" onPress={closeSettings} />;
}

function SettingsPage() {
  const extension = (section: SettingsExtension) => settingsExtensionAvailable(section) ? extensions[section] : undefined;
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
      <If when={settingsSection() === "connections"}>
        <ProvidersSettings />
      </If>
      <If when={settingsSection() === "models"}>
        <ModelChoicesSettings />
      </If>
      <If when={settingsSection() === "monitoring"}>
        <MonitoringSettings />
      </If>
      <If when={settingsSection() === "appearance"}>
        <AppearanceSettings />
      </If>
      <If when={settingsSection() === "websearch" ? extension("websearch") : undefined}>
        {(page) => <Extension page={page()} />}
      </If>
      <If when={settingsSection() === "about" ? extension("about") : undefined}>
        {(page) => <Extension page={page()} />}
      </If>
    </Surface>
  );
}

function Extension(props: { page: ExtensionPage }) {
  const Page = props.page.Page;
  return <Page />;
}
