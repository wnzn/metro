import { spaces } from "../../store";
import { Action, Banner, Checkbox, Column, Label, Row, ScrollArea, SettingRow, Toggle } from "../../ui";
import { loadSpaceAccess, NOTICE_BODY, saveSpaceAccess, setSpaceIncluded, spaceAccess, spaceAccessBusy, spaceAccessError, spaceAccessSaved, spaceAccessUnsupported } from "./state";

export function MetaSettings() {
  void loadSpaceAccess();
  return <ScrollArea padX={4} padY={3} gap={3} testID="metro-chat-settings">
    <Label heading variant="heading">Metro in Chat</Label>
    <If when={spaceAccessUnsupported()}>
      <Label color="muted">This feature needs a newer Metro Server.</Label>
    </If>
    <If when={spaceAccessError()}>
      <Banner title={spaceAccessError()} tone="danger">
        <Row><Action compact label="Reload settings" disabled={spaceAccessBusy()} onPress={() => void loadSpaceAccess()} /></Row>
      </Banner>
    </If>
    <If when={spaceAccess()} else={<If when={spaceAccessBusy()}><Label color="muted">Loading…</Label></If>}>
      {access => <>
        <SettingRow label="Use my Spaces in Chat" description="Metro reads and posts only where you can, as you.">
          <Toggle label="Use my Spaces in Chat" testID="metro-chat-enabled" checked={access().enabled}
            disabled={spaceAccessBusy()} onChange={enabled => void saveSpaceAccess({ enabled })} />
        </SettingRow>
        <Column gap={1}>
          <Label heading variant="strong">Spaces Metro can use</Label>
          <If when={spaces().length === 0}><Label color="muted">You're not in any Spaces.</Label></If>
          <Each of={spaces()} key={s => s.id}>{s =>
            <Checkbox label={s().name} testID={`metro-chat-space:${s().id}`}
              checked={!access().excluded_space_ids.includes(s().id)} disabled={!access().enabled || spaceAccessBusy()}
              onChange={included => setSpaceIncluded(s().id, included)} />
          }</Each>
        </Column>
        <Label variant="caption" color="muted">{NOTICE_BODY}</Label>
        <If when={spaceAccessSaved()}><Label variant="caption" color="muted">Saved</Label></If>
      </>}
    </If>
  </ScrollArea>;
}
