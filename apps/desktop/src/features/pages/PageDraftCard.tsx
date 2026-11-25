import { signal } from "nsh";
import type { RoomEvent } from "../../api/client";
import { goPages, hasSpace, pageDraftUnavailable } from "../../pagesState";
import { activeRoomId, activeSpaceId, mainPlace, me, sessionGuard } from "../../store";
import { Action, Badge, Column, Label, Row, Surface } from "../../ui";

export function PageDraftCard(props: { event: RoomEvent }) {
  const sid = activeSpaceId(), rid = activeRoomId();
  const guard = sessionGuard(sid ?? undefined);
  const event = () => props.event;
  const card = () => event().activity?.page_draft;
  const expired = signal(false);
  const owner = () => !!sid && !!rid && guard() && hasSpace(sid) && mainPlace() === "space"
    && activeSpaceId() === sid && activeRoomId() === rid && !!me() && me() === card()?.author_id;
  return <Surface variant="card" gap={2} pad={3} testID={`page-draft-card:${event().id}`}>
    <Row gap={2} align="center" wrap>
      <Badge tone="agent" label="Page draft" />
      <Label variant="meta">{event().activity?.agent?.name ?? event().activity?.name ?? "Agent"}</Label>
    </Row>
    <Label variant="strong">{card()?.title || "Page draft"}</Label>
    <Label color="muted">{card()?.summary || event().text}</Label>
    <Label variant="caption" color="muted">Nothing has been published by this proposal.</Label>
    <If when={owner()}>
      <Column gap={1} align="start">
        <If when={expired() || Date.now() >= Date.parse(card()?.expires_at ?? "") || pageDraftUnavailable(card()?.draft_id ?? "")} else={
          <Action compact testID={`review-page-draft:${event().id}`} label="Review draft" accessibilityLabel={`Review draft: ${card()?.title || "Page draft"}`} onPress={() => {
            const draft = card();
            if (!draft || !owner() || pageDraftUnavailable(draft.draft_id)) return;
            if (Date.now() >= Date.parse(draft.expires_at)) { expired.set(true); return; }
            goPages({ kind: "draft", id: draft.draft_id, spaceId: sid!, returnRoom: rid! });
          }} />
        }><Label variant="caption" color="muted">This draft is no longer available.</Label></If>
      </Column>
    </If>
  </Surface>;
}
