import { effect, signal } from "nsh";
import type { InputRef, ViewRef } from "nsh";
import type { DirectoryPerson } from "../../api/directory";
import {
  Action, AnchoredPopover, Banner, Column, compactWidth, ComposerBar, ComposerBox, ComposerContext, ComposerField, IdentityCardFrame,
  Label, Row, ScrollArea, SendButton,
} from "../../ui";
import { ConnectionBanner, messageAuthorName, replyExcerpt, Transcript } from "../Conversation";
import { PersonAvatar } from "../profile/PersonAvatar";
import { personState } from "../profile/profileState";
import { DirectMessagesSection } from "./DirectMessages";
import {
  activeDm, dmDraft, dmEvents, dmHiding, dmListError, dmListLoaded, dmLoaded, dmProblem, dmReactionPending,
  dmReactions, dmReply, dmSending, dms, openMessages, reactDm, deleteDmMessage, dmDeleting, refreshDmList, sendDm,
} from "./state";

export function DmHeader() {
  return <If when={activeDm()} else={<Label variant="strong">Direct messages</Label>}>
    {dm => <PeerCard peer={dm().peer} />}
  </If>;
}

function PeerCard(props: { peer: DirectoryPerson }) {
  const picked = signal<string | null>(null);
  let opener: ViewRef | undefined;
  const name = () => props.peer.name;
  const id = () => props.peer.id;
  const bio = () => personState(id())?.profile?.bio;
  const close = () => { picked.set(null); opener?.focus(); };
  effect(() => { id(); picked.set(null); });
  return <Row gap={1} align="center">
    <PersonAvatar userId={id()} name={name()} verifiedName size="sm" />
    <Action compact variant="ghost" label={name()} testID="dm-peer-card" viewRef={node => { opener = node; }}
      accessibilityLabel={`About ${name()}`} onPress={() => picked.set(id())} />
    <If when={picked() === id()}>
      <AnchoredPopover anchor={opener} onClose={close} placement="bottom" align="start" accessibilityLabel={`About ${name()}`} testID="dm-profile">
        <IdentityCardFrame>
          <Row gap={2} align="center">
            <PersonAvatar userId={id()} name={name()} verifiedName size="md" />
            <Column grow>
              <Label variant="strong">{name()}</Label>
              <Label variant="caption" color="muted">{id()}</Label>
            </Column>
          </Row>
          <If when={bio()}><Label>{bio()}</Label></If>
          <Row><Action compact variant="ghost" label="Close" onPress={close} /></Row>
        </IdentityCardFrame>
      </AnchoredPopover>
    </If>
  </Row>;
}

export function DmView() {
  let input: InputRef | undefined;
  let replyOpener: ViewRef | undefined;
  const focusInput = () => { input?.focus(); input?.setSelection(dmDraft().length); };
  return <Column grow testID="dm-view">
    <ConnectionBanner />
    <If when={dmProblem()}><Banner tone="warning" message={dmProblem()} testID="dm-error">
      <If when={activeDm() && !dmLoaded()}>
        <Row><Action compact label="Retry" onPress={refreshDmList} /></Row>
      </If>
    </Banner></If>
    <If when={activeDm()} else={
      <ScrollArea pad={4} gap={2}>
        <DirectMessagesSection inbox visible />
        <If when={!dmListLoaded() && !dmListError()}><Label color="muted">Loading messages…</Label></If>
        <If when={dmListLoaded() && !dms().length}><Label color="muted">No conversations yet.</Label></If>
      </ScrollArea>
    }>
      {dm => <>
        <Row padX={4} padY={1} gap={2} align="center" wrap>
          <Action compact variant="ghost" label="All messages" testID="dm-inbox" onPress={openMessages} />
          <If when={compactWidth()}><PeerCard peer={dm().peer} /></If>
        </Row>
        <Transcript testID="dm-transcript" emptyTitle="Start the conversation"
          emptyText={`This is the start of your conversation with ${dm().peer.name}.`}
          direct={{ id: dm().id, events: dmEvents, loaded: dmLoaded, reactions: dmReactions, pending: dmReactionPending, react: reactDm,
            deleting: dmDeleting, remove: deleteDmMessage }}
          onReply={(event, opener) => {
            if (!dmEvents().includes(event)) return;
            dmReply.set(event);
            replyOpener = opener;
            focusInput();
          }} />
        <Column padX={4} padY={3}>
          <ComposerBox testID="dm-composer-box">
            <If when={dmReply()}>
              {event => <ComposerContext testID="dm-reply-context">
                <Column grow>
                  <Label variant="caption" color="muted" lines={2}>{`Reply to ${messageAuthorName(event())} · ${replyExcerpt(event())}`}</Label>
                </Column>
                <Action compact variant="ghost" label="Cancel reply" onPress={() => {
                  dmReply.set(null);
                  if (replyOpener) replyOpener.focus();
                  else focusInput();
                }} />
              </ComposerContext>}
            </If>
            <ComposerField testID="dm-composer" ref={node => { input = node; }} value={dmDraft()} onChange={dmDraft.set}
              onSubmit={() => void sendDm()} disabled={!dmLoaded() || dmHiding() === dm().id} placeholder={`Message ${dm().peer.name}`} />
            <ComposerBar trailing={<SendButton testID="dm-send" tooltip="Send (Return)" accessibilityLabel="Send message"
              busy={dmSending()} disabled={!dmLoaded() || !dmDraft().trim() || dmHiding() === dm().id} onPress={() => void sendDm()} />} />
          </ComposerBox>
        </Column>
      </>}
    </If>
  </Column>;
}
