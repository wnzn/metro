import { announce, cleanup, signal } from "nsh";
import type { RoomEvent } from "../../api/client";
import { ApiError, apiClient, describe, mainPlace, openExactRoom, readHostedAccess, sessionGuard, settingsOpen, spaces } from "../../store";
import { Action, Column, Disclosure, Label, MessageBody, Row } from "../../ui";
import { disclosureKey, disclosureScope, isOpen, setOpen } from "../activity/disclosure";

export function SpacePostRow(props: { event: RoomEvent }) {
  const post = () => props.event.activity!.spacePost!;
  const key = () => disclosureKey(disclosureScope(), props.event.id, "space-post");
  const title = () => `Metro sent a message to ${post().spaceName}`;
  const noAccess = signal(false);
  const pending = signal(false);
  const error = signal("");
  const denied = () => noAccess() || !spaces().some(s => s.id === post().spaceId);
  let mounted = true;
  cleanup(() => { mounted = false; });
  const open = async () => {
    const c = apiClient(), target = post();
    if (!c || denied() || pending() || mainPlace() !== "chat" || settingsOpen()) return;
    const grant = sessionGuard(target.spaceId);
    const here = () => mounted && grant() && post() === target && mainPlace() === "chat" && !settingsOpen();
    pending.set(true);
    error.set("");
    try {
      const opened = await readHostedAccess(c, () => openExactRoom(target.spaceId, target.roomId, here, target.eventId));
      if (!opened && here()) error.set("That room or thread isn't available.");
    } catch (problem) {
      if (!here()) return;
      if (problem instanceof ApiError && (problem.kind === "forbidden" || problem.kind === "not_found")) {
        noAccess.set(true);
        announce("You no longer have access");
      } else error.set(describe(problem, "Could not open the message"));
    } finally {
      if (mounted) pending.set(false);
    }
  };
  return <Disclosure testID={`space-post:${props.event.id}`} title={title()} open={isOpen(key())}
    accessibilityLabel={`${title()}, ${isOpen(key()) ? "collapse" : "expand"} for details`}
    onToggle={value => setOpen(key(), value)}>
    <Column gap={1}>
      <Label variant="caption" color="muted">{`To: ${post().spaceName} › #${post().roomName}${post().threadName ? ` › ${post().threadName}` : ""}`}</Label>
      <Label variant="caption" color="muted">{`Agent: @${post().agent.handle}`}</Label>
    </Column>
    <MessageBody text={props.event.text} scopeKey={key()} testID={`space-post-message:${props.event.id}`} />
    <Row align="center" gap={2} wrap>
      <Action compact label="Open" testID={`space-post-open:${props.event.id}`}
        accessibilityLabel={denied() ? "Open posted message. You no longer have access" : "Open posted message"}
        tooltip={denied() ? "You no longer have access" : undefined}
        disabled={denied()} busy={pending()} busyLabel="Opening…" onPress={() => void open()} />
      <If when={denied()}><Label variant="caption" color="muted">You no longer have access</Label></If>
      <If when={error() && !denied()}><Label variant="caption" color="danger">{error()}</Label></If>
    </Row>
  </Disclosure>;
}
