import type { JSX } from "nsh";
import { replyAnchor, replyPane } from "../../store";
import type { RoomEvent } from "../../store";
import { Column, IconButton, Label, Row } from "../../ui";
import { ThreadAnchor, Transcript } from "../Conversation";

export function ThreadPane(props: { onReply: (event: RoomEvent) => void; onClose: () => void; composer: JSX.Element }) {
  return (
    <Column grow testID="reply-thread">
      <Row pad={3} gap={2} align="center" justify="between">
        <Label heading variant="heading">Thread</Label>
        <IconButton icon="close" accessibilityLabel="Close thread" onPress={props.onClose} testID="close-thread" />
      </Row>
      <If when={replyAnchor()} else={
        <Column grow pad={4}><Label color="muted">Loading thread…</Label></Column>
      }>
        {anchor => <>
          <Column padX={3} padY={2}><ThreadAnchor event={anchor()} /></Column>
          <Transcript roomId={replyPane()?.roomId} testID="thread-transcript" onReply={props.onReply}
            replyAnchor={anchor()} emptyTitle="No replies yet" emptyText="Keep this conversation together by replying here." />
          {props.composer}
        </>}
      </If>
    </Column>
  );
}
