import { cleanup, effect, signal, untracked } from "nsh";
import { ApiError } from "../../api/client";
import type { CompactionPrefs } from "../../api/client";
import { apiClient, me, readHostedAccess, sessionGuard } from "../../store";
import { Action, Column, Label, PercentField, Row, SettingRow, SettingsGroup, Toggle } from "../../ui";

export function CompactionSettings() {
  const saved = signal<CompactionPrefs | null>(null);
  const auto = signal(true);
  const percent = signal("70");
  const problem = signal<string | null>(null);
  const confirmation = signal(false);
  const busy = signal(false);
  const unsupported = signal(false);
  const retry = signal(0);
  let alive = true;
  cleanup(() => { alive = false; });
  const accept = (prefs: CompactionPrefs) => {
    saved.set(prefs);
    auto.set(prefs.auto);
    percent.set(String(prefs.at_percent));
  };
  effect(() => {
    const account = me();
    retry();
    untracked(() => {
      const client = apiClient();
      const current = sessionGuard();
      const valid = () => alive && current() && me() === account;
      saved.set(null);
      busy.set(false);
      problem.set(null);
      confirmation.set(false);
      unsupported.set(false);
      if (!account || !client) return;
      void readHostedAccess(client, () => client.compactionPrefs()).then(
        prefs => { if (valid()) accept(prefs); },
        error => {
          if (!valid()) return;
          if (error instanceof ApiError && (error.status === 404 || error.status === 405)) unsupported.set(true);
          else problem.set("Couldn't load summary settings.");
        },
      );
    });
  });
  const validPercent = () => /^\d{2}$/.test(percent()) && Number(percent()) >= 40 && Number(percent()) <= 90;
  const dirty = () => auto() !== saved()?.auto || percent() !== String(saved()?.at_percent);
  const save = async () => {
    const client = apiClient();
    if (!client || !saved() || !dirty() || !validPercent() || busy()) return;
    const sameSession = sessionGuard();
    const account = me();
    const valid = () => alive && sameSession() && me() === account;
    busy.set(true);
    problem.set(null);
    confirmation.set(false);
    try {
      const prefs = await readHostedAccess(client, () => client.compactionPrefs({ auto: auto(), at_percent: Number(percent()) }));
      if (valid()) {
        accept(prefs);
        confirmation.set(true);
      }
    } catch {
      if (valid()) problem.set("Couldn't save summary settings. Try again.");
    } finally {
      if (valid()) busy.set(false);
    }
  };
  return (
    <If when={!!me() && !unsupported()}>
      <SettingsGroup title="Conversation summaries" testID="compaction-settings"
        description="Keep earlier messages in context for agent runs you start. Saved messages stay visible in Chat and Spaces.">
        <If when={saved()} else={
          <SettingRow label={problem() ?? "Loading summary settings…"}>
            <If when={problem()}><Action label="Try again" onPress={() => retry.set(retry() + 1)} /></If>
          </SettingRow>
        }>
          <SettingRow label="Summarize automatically"
            description="On by default. Summarizing can make one extra call to the connection answering your run. If shared, its owner can see the request.">
            <Toggle label="Summarize automatically" testID="compaction-auto" checked={auto()} disabled={busy()}
              onChange={value => { confirmation.set(false); auto.set(value); }} />
          </SettingRow>
          <SettingRow stacked label="Summarize at" description="40–90% of the model’s context budget (70% by default). Summaries can also run when a conversation has too many messages.">
            <PercentField accessibilityLabel="Automatic summary threshold" testID="compaction-percent" value={percent()}
              disabled={busy()} onChange={value => { confirmation.set(false); percent.set(value); }}
              error={validPercent() ? null : "Enter a whole number from 40 to 90."} />
          </SettingRow>
          <Column padX={4} padY={3} gap={2}>
            <If when={problem()}>{message => <Label color="danger" variant="caption">{message()}</Label>}</If>
            <If when={confirmation()}><Label color="muted" variant="caption">Summary settings saved.</Label></If>
            <Row><Action testID="compaction-save" label="Save summary settings" busyLabel="Saving…" busy={busy()}
              disabled={!dirty() || !validPercent()} onPress={() => void save()} /></Row>
          </Column>
        </If>
      </SettingsGroup>
    </If>
  );
}
