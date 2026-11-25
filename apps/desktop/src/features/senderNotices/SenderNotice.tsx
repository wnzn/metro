import {
  activeSenderNotice, dismissSenderNotice, openSettings, prepareSenderNoticeDraft, senderNoticeIsCurrent,
} from "../../store";
import type { RoomEvent, SenderNotice as Notice } from "../../store";
import { Action, Banner, Column, IconButton, Label, Row } from "../../ui";
import { openSpaceSettings } from "../space/spaceSettingsState";
import { addressingText, routingText } from "./response";

// Fixed in-app destinations only. Do not use anything from the note's text
// as a link or reopen the Space that happened to be behind another view.
function settingsFor(note: Notice, which: "addressing" | "routing") {
  if (!senderNoticeIsCurrent(note)) return;
  const source = note.routing?.kind === "model_unavailable" ? note.routing.source : null;
  if (note.destination.kind === "room" &&
    (which === "addressing" || source === "agent_default" || source === "space_default")) {
    openSpaceSettings(note.destination.spaceId, "preferences");
  } else {
    // The Settings shell maps these compatibility aliases to the new pages.
    openSettings(which === "addressing" || !source || source === "case" || source === "routing_default"
      ? "routing" : "providers");
  }
}

export function SenderNotice(props: { event: () => RoomEvent; focusComposer: (caret: number) => void }) {
  return (
    <If when={activeSenderNotice(props.event())}>
      {(current) => <NoticeCard notice={current()} focusComposer={props.focusComposer} />}
    </If>
  );
}

function NoticeCard(props: { notice: Notice; focusComposer: (caret: number) => void }) {
  const id = () => props.notice.eventId;
  const ask = () => {
    const caret = prepareSenderNoticeDraft(props.notice);
    if (caret !== null) props.focusComposer(caret);
  };
  return (
    <Banner tone="warning" variant="inline" title="Only you see this" testID={`addressing-note:${id()}`}
      action={<IconButton icon="close" testID={`addressing-note-dismiss:${id()}`}
        accessibilityLabel="Dismiss sender-only notice" tooltip="Dismiss" onPress={() => dismissSenderNotice(props.notice)} />}>
      <Column gap={2}>
        <If when={props.notice.addressing}>
          {(note) => (
            <Column gap={1}>
              <Label variant="caption">{addressingText(note())}</Label>
              <Row gap={1} wrap>
                <If when={note().kind === "classification_failed"}>
                  <Action compact variant="ghost" label="Ask agent" testID={`addressing-note-ask:${id()}`} onPress={ask} />
                </If>
                <Action compact variant="ghost" label="Settings" accessibilityLabel="Open Space preferences"
                  testID={`addressing-note-settings:${id()}`} onPress={() => settingsFor(props.notice, "addressing")} />
              </Row>
            </Column>
          )}
        </If>
        <If when={props.notice.routing}>
          {(note) => (
            <Column gap={1} testID={`routing-note:${id()}`}>
              <If when={props.notice.destination.kind === "room"}>
                <Label variant="strong">One agent turn couldn’t start</Label>
              </If>
              <Label variant="caption">{routingText(note(), props.notice.destination.kind === "room")}</Label>
              <Row>
                <Action compact variant="ghost" label="Settings" accessibilityLabel="Open settings for this model choice"
                  testID={`routing-note-settings:${id()}`} onPress={() => settingsFor(props.notice, "routing")} />
              </Row>
            </Column>
          )}
        </If>
      </Column>
    </Banner>
  );
}
