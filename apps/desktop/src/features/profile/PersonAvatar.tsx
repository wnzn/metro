// A person's picture as the server lets this user see it, else their initials.

import { effect } from "nsh";
import { Avatar } from "../../ui";
import type { AvatarSize } from "../../ui";
import { initialsOf, personTone } from "./identity";
import { ensurePerson, personState, pictureFailed, profileName } from "./profileState";

export function PersonAvatar(props: { userId: string | null | undefined; name: string; size?: AvatarSize; testID?: string; accessibilityLabel?: string }) {
  effect(() => {
    const id = props.userId;
    if (id) void ensurePerson(id);
  });
  const shownName = () => profileName(props.userId) ?? props.name;
  return (
    <Avatar
      testID={props.testID}
      size={props.size}
      initials={initialsOf(shownName())}
      tone={props.userId ? personTone(props.userId) : "neutral"}
      src={props.userId ? personState(props.userId)?.avatarUrl ?? null : null}
      accessibilityLabel={props.accessibilityLabel}
      onImageError={() => props.userId && pictureFailed(props.userId)}
    />
  );
}
