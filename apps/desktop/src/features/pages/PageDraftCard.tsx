import { cleanup, signal } from "nsh";
import type { RoomEvent } from "../../api/client";
import { isPageStatus } from "../../api/pages";
import { forgetPageDraft, goPages, hasSpace, pageDraftUnavailable, scopeVisible, spaceGrants } from "../../pagesState";
import { activeRoomId, activeSpaceId, apiClient, describe, mainPlace, me, readHostedAccess, sessionGuard, settingsOpen } from "../../store";
import { Action, Badge, Column, Label, Row, Surface } from "../../ui";

export function PageDraftCard(props: { event: RoomEvent }) {
  const sid = activeSpaceId(), rid = activeRoomId();
  const inChat = mainPlace() === "chat";
  const guard = sessionGuard(inChat ? undefined : sid ?? undefined);
  const event = () => props.event;
  const card = () => event().activity?.page_draft;
  const expired = signal(false);
  const pending = signal(false);
  const error = signal("");
  let mounted = true;
  cleanup(() => { mounted = false; });
  const owner = () => guard() && !!me() && me() === card()?.author_id && (inChat ? mainPlace() === "chat"
    : !!sid && !!rid && hasSpace(sid) && mainPlace() === "space" && activeSpaceId() === sid && activeRoomId() === rid);
  const review = async () => {
    const draft = card(), c = apiClient();
    if (!draft || !c || !owner() || pending() || pageDraftUnavailable(draft.draft_id)) return;
    if (Date.now() >= Date.parse(draft.expires_at)) { expired.set(true); return; }
    if (!inChat) {
      goPages({ kind: "draft", id: draft.draft_id, spaceId: sid!, returnRoom: rid! });
      return;
    }
    // Chat proposals can belong to a Space or the personal library. The card
    // has no scope; resolve it through the authenticated draft read.
    const grants = spaceGrants();
    const valid = () => mounted && owner() && !settingsOpen() && draft.draft_id === card()?.draft_id && !pageDraftUnavailable(draft.draft_id);
    pending.set(true);
    error.set("");
    try {
      const next = await readHostedAccess(c, () => c.getPageDraft(draft.draft_id));
      if (!valid()) return;
      if (next.id !== draft.draft_id || !scopeVisible(next) || (next.space_id && !grants.get(next.space_id)?.())) {
        error.set("This draft is no longer available.");
        return;
      }
      goPages({ kind: "draft", id: next.id, spaceId: next.space_id ?? undefined, returnChat: true });
    } catch (problem) {
      if (valid()) {
        if (isPageStatus(problem, 403) || isPageStatus(problem, 404)) forgetPageDraft(draft.draft_id);
        else error.set(describe(problem, "Could not open the draft"));
      }
    } finally {
      if (mounted) pending.set(false);
    }
  };
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
          <Action compact testID={`review-page-draft:${event().id}`} label="Review draft"
            accessibilityLabel={`Review draft: ${card()?.title || "Page draft"}`}
            busy={pending()} busyLabel="Opening…" onPress={() => void review()} />
        }><Label variant="caption" color="muted">This draft is no longer available.</Label></If>
        <If when={error()}><Label variant="caption" color="danger">{error()}</Label></If>
      </Column>
    </If>
  </Surface>;
}
