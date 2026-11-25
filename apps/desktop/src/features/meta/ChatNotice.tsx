import { effect, signal, untracked } from "nsh";
import { chatUnavailable, openSettings, settingsOpen } from "../../store";
import { Action, Banner, Label, modalOpen, Row } from "../../ui";
import { focusComposer } from "../Conversation";
import { loadSpaceAccess, NOTICE_BODY, saveSpaceAccess, spaceAccess, spaceAccessBusy, spaceAccessError } from "./state";

const covered = () => settingsOpen() || modalOpen();

export function ChatNotice() {
  const shown = signal(false);
  const dismissed = signal(false);
  const visible = () => !chatUnavailable() && !dismissed() &&
    !!spaceAccess()?.enabled && (shown() || (!covered() && !spaceAccess()?.notice_shown));
  void loadSpaceAccess();
  return <If when={visible()}>
    <Notice onRender={() => {
      shown.set(true);
      void saveSpaceAccess({ notice_shown: true });
    }} onDismiss={() => {
      dismissed.set(true);
      focusComposer();
    }} />
  </If>;
}

function Notice(props: { onRender: () => void; onDismiss: () => void }) {
  // Only acknowledge when this banner is mounted, never from Settings or a GET.
  let started = false;
  effect(() => {
    if (started || covered() || spaceAccessBusy() || spaceAccess()?.notice_shown) return;
    started = true;
    untracked(props.onRender);
  });
  return <Banner testID="chat-spaces-notice" tone="neutral" title="Metro can use your Spaces here" message={NOTICE_BODY}>
    <Row gap={1} wrap>
      <Action compact variant="ghost" label="Got it" testID="chat-spaces-dismiss"
        disabled={spaceAccessBusy() || !spaceAccess()?.notice_shown} onPress={props.onDismiss} />
      <Action compact variant="ghost" label="Choose Spaces…" testID="chat-spaces-settings" onPress={() => openSettings("metro-chat")} />
      <Action compact variant="ghost" label="Turn off" testID="chat-spaces-disable" disabled={spaceAccessBusy()} onPress={async () => {
        if (await saveSpaceAccess({ enabled: false })) props.onDismiss();
      }} />
    </Row>
    <If when={spaceAccessError()}>
      <Label variant="caption" color="danger">{spaceAccessError()}</Label>
      <If when={!spaceAccess()?.notice_shown}>
        <Row><Action compact label="Retry" disabled={spaceAccessBusy()} onPress={() => void saveSpaceAccess({ notice_shown: true })} /></Row>
      </If>
    </If>
  </Banner>;
}
