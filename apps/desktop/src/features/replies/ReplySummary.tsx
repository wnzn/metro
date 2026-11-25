import type { ViewRef } from "nsh";
import type { RoomEvent } from "../../store";
import { InlineLink } from "../../ui";
import { PersonAvatar } from "../profile/PersonAvatar";
import { profileName } from "../profile/profileState";

export function ReplySummary(props: {
  eventId: number;
  summary: NonNullable<RoomEvent["replies"]>;
  last?: string;
  onOpen: (opener?: ViewRef) => void;
}) {
  let opener: ViewRef | undefined;
  const label = () => `${props.summary.count} ${props.summary.count === 1 ? "reply" : "replies"}`;
  const names = () => props.summary.participantIds.map(id => profileName(id) ?? id);
  return <InlineLink testID={`replies:${props.eventId}`} ref={node => { opener = node; }}
    label={`${label()}${props.last ? ` · ${props.last}` : ""}`}
    accessibilityLabel={`Open thread, ${label()}${props.last ? `, last reply at ${props.last}` : ""}${names().length ? `, ${names().join(", ")}` : ""}`}
    onPress={() => props.onOpen(opener)}
    leading={<Each of={props.summary.participantIds} key={id => id}>
      {id => <PersonAvatar userId={id()} name={profileName(id()) ?? id()} size="sm" testID={`reply-participant:${props.eventId}:${id()}`} />}
    </Each>} />;
}
