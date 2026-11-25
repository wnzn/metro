// A Space agent's picture as this user may see it (a member of its Space),
// else its initial on its colour. For the transcript, the @ list, the
// Ask-agent picker, activity rows and Space settings.
//
// `agent` can be a listed agent or the badge a run's events carry
// (`activity.agent`: { id, handle, name, avatar_version?, color }).

import { AgentAvatar } from "../../ui";
import type { AgentAvatarSize } from "../../ui";
import { initialOf } from "../../api/spaces";
import { agentPictureFailed, agentPictureUrl } from "./agents";

export type AgentLike = {
  id: string;
  name: string;
  color: string;
  initial?: string;
  /** Either spelling: the listed agent's `avatarVersion` or an event badge's `avatar_version`. */
  avatarVersion?: string | null;
  avatar_version?: string | null;
};

export function SpaceAgentAvatar(props: { spaceId: string; agent: AgentLike; size?: AgentAvatarSize; testID?: string; accessibilityLabel?: string }) {
  const version = () => props.agent.avatarVersion ?? props.agent.avatar_version ?? null;
  return (
    <AgentAvatar
      testID={props.testID}
      size={props.size}
      initial={props.agent.initial || initialOf(props.agent.name)}
      color={props.agent.color}
      src={agentPictureUrl(props.spaceId, props.agent.id, version())}
      accessibilityLabel={props.accessibilityLabel}
      onImageError={() => agentPictureFailed(props.spaceId, props.agent.id, version())}
    />
  );
}
