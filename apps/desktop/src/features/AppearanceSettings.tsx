// Appearance is saved on this computer, not to the signed-in account.
import { cleanup, signal } from "nsh";
import { Action, APPEARANCE_OPTIONS, appearancePreference, Badge, Banner, compactWidth, PageHeader, PageLayout, resolvedAppearance, SegmentedControl, setAppearancePreference, SettingRow, SettingsGroup } from "../ui";
import type { AppearancePreference } from "../ui";

export function AppearanceSettings() {
  const shown = () => `${resolvedAppearance() === "dark" ? "Dark" : "Light"}, from your system`;
  const state = signal<"saving" | "saved" | "failed" | null>(null);
  let mounted = true;
  cleanup(() => { mounted = false; });
  const save = async (value: AppearancePreference) => {
    if (state() === "saving") return;
    state.set("saving");
    const saved = await setAppearancePreference(value);
    if (mounted) state.set(saved ? "saved" : "failed");
  };
  return (
    <PageLayout testID="appearance-settings" pad={compactWidth() ? 4 : 6} gap={6} footer={state() === "saved" || state() === "failed" ? (
      <Banner variant="inline" testID="appearance-feedback" tone={state() === "saved" ? "success" : "danger"}
        message={state() === "saved" ? "Appearance saved on this computer." : "Appearance changed, but couldn't be saved for next time."}
        action={state() === "failed" ? <Action label="Try again" onPress={() => void save(appearancePreference())} /> : undefined} />
    ) : undefined}>
      <PageHeader title="Appearance" subtitle="Choose how Metro looks." scope={{ kind: "computer" }} />
      <SettingsGroup title="Theme">
        <SettingRow label="Color scheme" description="Dark is the default; System follows your operating system." stacked={compactWidth()}>
          <SegmentedControl testID="appearance" accessibilityLabel="Color scheme" options={APPEARANCE_OPTIONS}
            value={appearancePreference()} disabled={state() === "saving"} onChange={(v) => void save(v)} />
        </SettingRow>
        <If when={appearancePreference() === "system"}>
          <SettingRow label="Showing now" description="Changes when your operating system does.">
            <Badge tone="neutral" testID="appearance-shown" label={shown()} />
          </SettingRow>
        </If>
      </SettingsGroup>
    </PageLayout>
  );
}
