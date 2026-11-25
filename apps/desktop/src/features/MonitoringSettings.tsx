// Settings › Monitoring: recent events from one explicit source at a time —
// this computer's Metro helper, My Chat (the user's own personal Chat, from
// the server), one Space the user owns, or (for org admins) the connected
// server's instance. The page always says which source and, for a server
// source, which server (and, for My Chat, which account) the events came
// from. Sources are never mixed into one list (src/monitoring.ts).

import { connectedTo, me } from "../store";
import { settingsAccountScope } from "../settings";
import {
  loadMonitoring,
  MAX_MONITORING_EVENTS,
  monitoringResult,
  monitoringSource,
  monitoringSources,
  selectMonitoringSource,
} from "../monitoring";
import type { MonitoringEvent, MonitoringSource } from "../monitoring";
import { Action, Badge, Banner, Column, compactWidth, Divider, FactList, Label, PageHeader, PageLayout, Picker, Row, SettingRow, SettingsGroup } from "../ui";
import type { Fact, PageScope, PickerItem } from "../ui";

const GROUP: Record<MonitoringSource["kind"], string> = {
  device: "This computer",
  me: "Your account on this server",
  space: "Spaces you own",
  instance: "Organization",
};

function sourceItems(list: readonly MonitoringSource[]): PickerItem[] {
  return list.map((s) => ({
    value: s.key,
    label: s.kind === "space" ? `Space · ${s.label}` : s.label,
    group: GROUP[s.kind],
    detail:
      s.kind === "device"
        ? "Metro's helper on this computer"
        : s.kind === "me"
          ? "Your personal Chat's failures, from the server; only you"
          : s.kind === "space"
          ? "This Space only, from the server"
          : "The server itself (organization admins)",
  }));
}

function facts(s: MonitoringSource): Fact[] {
  if (s.kind === "device") return [{ label: "Source", value: "This computer's Metro helper" }];
  const server = connectedTo() ?? "not connected";
  if (s.kind === "me") {
    return [
      { label: "Source", value: "My Chat: your personal Chat, only you" },
      { label: "Server", value: server },
      { label: "Account", value: me() ?? "unknown" },
    ];
  }
  return [
    { label: "Source", value: s.kind === "space" ? `Space “${s.label}”` : "This server's instance" },
    { label: "Server", value: server },
  ];
}

/** "2026-11-23 10:15:02 UTC": always UTC and labelled, so it never reads as local. */
export function formatEventTime(ms: number): string {
  const iso = new Date(ms).toISOString();
  return `${iso.slice(0, 10)} ${iso.slice(11, 19)} UTC`;
}

export function MonitoringSettings() {
  const hasFeedback = () => monitoringResult()?.phase === "error" || monitoringResult()?.phase === "unsupported";
  const scope = (): PageScope | undefined => {
    switch (monitoringSource()?.kind) {
      case "device": return { kind: "computer" };
      case "me": return settingsAccountScope();
      case "space": return { kind: "space" };
      case "instance": return { kind: "organization" };
    }
  };
  return (
    <PageLayout testID="monitoring-settings" pad={compactWidth() ? 4 : 6} gap={6} footer={hasFeedback() ? <MonitoringFeedback /> : undefined}>
      <PageHeader title="Monitoring" subtitle="See recent events from one source at a time." scope={scope()} />
      <If when={monitoringSource()} else={<NoSources />}>
        {(s) => (
          <Column gap={4}>
            <SettingsGroup title="Source">
              <SettingRow label="Event source" stacked>
                <Row gap={2} align="center" wrap>
                  <Picker testID="monitoring-source" accessibilityLabel="Monitoring source" items={sourceItems(monitoringSources())}
                    value={s().key} onChange={(v) => v && selectMonitoringSource(v)} />
                  <Action testID="monitoring-refresh" label="Refresh" busyLabel="Loading…" busy={monitoringResult()?.phase === "loading"}
                    onPress={() => void loadMonitoring()} />
                </Row>
              </SettingRow>
              <SettingRow label="Scope" stacked>
                <FactList testID="monitoring-source-facts" facts={facts(s())} />
              </SettingRow>
            </SettingsGroup>
            <Result />
          </Column>
        )}
      </If>
    </PageLayout>
  );
}

function NoSources() {
  return (
    <SettingsGroup title="Events" testID="monitoring-none">
      <SettingRow label="No monitoring sources" description="Connect to a server or start the Metro desktop helper to see events." />
    </SettingsGroup>
  );
}

function MonitoringFeedback() {
  const result = monitoringResult;
  return (
    <>
      <If when={result()?.phase === "unsupported"}>
        <Banner variant="inline" tone="warning" testID="monitoring-unsupported" message={result()?.message ?? ""} />
      </If>
      <If when={result()?.phase === "error"}>
        <Banner variant="inline" tone="danger" testID="monitoring-error" message={result()?.message ?? ""}
          action={<Action label="Try again" testID="monitoring-retry" onPress={() => void loadMonitoring()} />} />
      </If>
    </>
  );
}

function Result() {
  return (
    <If when={monitoringResult()} else={<Label variant="caption" color="muted">Loading events…</Label>}>
      {(r) => (
        <Column gap={3}>
          <If when={r().phase === "loading" && r().events.length === 0}>
            <Label variant="caption" color="muted" testID="monitoring-loading">
              Loading events…
            </Label>
          </If>
          <If when={r().phase === "ready" && r().events.length === 0}>
            <Label variant="caption" color="muted" testID="monitoring-empty">
              No events yet.
            </Label>
          </If>
          <If when={r().truncated}>
            <Label variant="caption" color="muted" testID="monitoring-truncated">
              {`Showing the newest ${MAX_MONITORING_EVENTS} events.`}
            </Label>
          </If>
          <If when={r().dropped > 0}>
            <Label variant="caption" color="muted" testID="monitoring-dropped">
              {`${r().dropped} ${r().dropped === 1 ? "entry" : "entries"} couldn't be read and ${r().dropped === 1 ? "isn't" : "aren't"} shown.`}
            </Label>
          </If>
          <If when={r().events.length > 0}>
            <SettingsGroup title="Events" testID="monitoring-events">
              <Each of={r().events.map((e, i) => ({ e, i }))} key={(x) => `${x.i}:${x.e.timestamp}:${x.e.code}`}>
                {(x) => (
                  <Column>
                    <Divider />
                    <EventRow event={x().e} index={x().i} />
                  </Column>
                )}
              </Each>
            </SettingsGroup>
          </If>
        </Column>
      )}
    </If>
  );
}

function EventRow(props: { event: MonitoringEvent; index: number }) {
  return (
    <Column gap={1} padX={4} padY={3} testID={`monitoring-event:${props.index}`}>
      <Row gap={2} align="center" wrap>
        <Label variant="meta">{formatEventTime(props.event.at)}</Label>
        <Badge tone="neutral" variant="outline" label={props.event.component} />
        <Badge tone="system" label={props.event.code} />
      </Row>
      <Label variant="body">{props.event.summary}</Label>
    </Column>
  );
}
