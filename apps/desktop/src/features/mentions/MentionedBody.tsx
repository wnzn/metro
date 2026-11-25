import { Clipboard, announce, computed, effect, signal } from "nsh";
import type { ViewRef } from "nsh";
import type { Mention, RoomEvent } from "../../api/client";
import type { SpaceAgent } from "../../api/spaces";
import { activeSpace, composerScope, me } from "../../store";
import { Action, AnchoredPopover, Badge, Column, IdentityCardFrame, Label, MentionToken, MessageBody, Row } from "../../ui";
import { toolName } from "../activity/classify";
import { isOpen, toggle } from "../activity/disclosure";
import { FOLD_LINES, outputView } from "../activity/output";
import { PersonAvatar } from "../profile/PersonAvatar";
import { personState, profileName } from "../profile/profileState";
import { SpaceAgentAvatar } from "../space/SpaceAgentAvatar";
import { openSpaceSettings, spaceSettingsFor, startEditAgent } from "../space/spaceSettingsState";
import { verifiedAgents, verifiedMembers } from "./roster";
import type { MentionPiece } from "./sent";

type Identity = { kind: "user"; id: string; name: string; bio?: string; role: string }
  | { kind: "agent"; agent: SpaceAgent; name: string };

export function localMentionIdentity(sid: string, mention: Mention): Identity | null {
  if (spaceSettingsFor()) return null;
  if (mention.kind === "user") {
    const member = verifiedMembers(sid)?.find(m => m.userId === mention.id);
    if (!member) return null;
    return { kind: "user", id: member.userId, name: profileName(member.userId) ?? member.userId,
      role: member.role, bio: personState(member.userId)?.profile?.bio };
  }
  if (mention.addressedBy || !mention.agentId) return null;
  const agent = verifiedAgents(sid)?.find(a => a.id === mention.agentId);
  return agent ? { kind: "agent", agent, name: agent.name } : null;
}

export function sentMentionLabel(sid: string, mention: Mention, literal: string) {
  if (mention.kind === "agent" && !mention.agentId) return "@agent";
  const identity = localMentionIdentity(sid, mention);
  return identity ? `@${identity.name}` : literal;
}

export function MentionedBody(props: { event: RoomEvent; sid?: string; pieces: readonly MentionPiece[]; stateKey: string }) {
  const open = () => isOpen(props.stateKey);
  const view = computed(() => outputView(props.event.text, open()));
  const shown = computed(() => {
    const v = view();
    if (!v.foldable || open()) return v.shown;
    const visible = v.shown.split("\n", FOLD_LINES).join("\n");
    return visible.length > 600 ? [...visible.slice(0, 1200)].slice(0, 600).join("") : visible;
  });
  const card = signal<{ piece: MentionPiece; scope: string; eventId: number; anchor: ViewRef } | null>(null);
  const identity = () => {
    const target = card(), sid = props.sid;
    return target && sid && target.scope === composerScope() && target.eventId === props.event.id
      ? localMentionIdentity(sid, target.piece.mention) : null;
  };
  effect(() => { if (card() && !identity()) card.set(null); });
  const close = () => {
    const previous = card();
    const canRestore = !!identity();
    card.set(null);
    if (canRestore) previous?.anchor.focus();
  };
  const renderMention = (ordinal: number) => {
    const piece = () => props.pieces[ordinal];
    let anchor: ViewRef | null = null;
    const info = computed(() => props.sid && piece() ? localMentionIdentity(props.sid, piece().mention) : null);
    const self = () => {
      const m = piece().mention;
      return m.kind === "user" && m.id === me();
    };
    const generic = () => {
      const m = piece().mention;
      return m.kind === "agent" && !m.agentId;
    };
    const label = () => info() ? `@${info()!.name}` : generic() ? "@agent" : piece().literal;
    return <If when={piece()}>
      <MentionToken
        testID={`mention-token:${props.event.id}:${ordinal}`}
        tone={info() ? self() ? "self" : piece().mention.kind === "user" ? "person" : "agent" : generic() ? "agent" : "unavailable"}
        label={label()}
        accessibilityLabel={info() ? `${info()!.name}, mentioned. Open card.` : generic() ? label() : `${label()}, unavailable`}
        ref={node => { anchor = node; }}
        onPress={info() ? () => {
          if (anchor && info()) card.set({ piece: piece(), eventId: props.event.id, scope: composerScope(), anchor });
        } : undefined}
      />
    </If>;
  };
  return (
    <Column gap={1}>
      <MessageBody text={shown()} testID={`message-body:${props.event.id}`}
        scopeKey={props.stateKey} mentions={props.pieces} renderMention={renderMention} />
      <If when={view().toggleLabel}>
        {label => <Row><Action label={label()} variant="ghost" onPress={() => { close(); toggle(props.stateKey); }} /></Row>}
      </If>
      <If when={view().notice}>{text => <Label variant="caption" color="muted">{text()}</Label>}</If>
      <If when={identity()}>
        {info => (
          <AnchoredPopover anchor={card()?.anchor} placement="bottom" align="start" onClose={close}
            testID="identity-card" accessibilityLabel={`About ${info().name}`}>
            <IdentityCardFrame>
              <IdentityDetails info={info()} sid={props.sid!} close={close} />
            </IdentityCardFrame>
          </AnchoredPopover>
        )}
      </If>
    </Column>
  );
}

function IdentityDetails(props: { info: Identity; sid: string; close: () => void }) {
  const user = () => props.info.kind === "user" ? props.info : null;
  const agent = () => props.info.kind === "agent" ? props.info.agent : null;
  return (
    <If when={user()} else={
      <If when={agent()}>
        {a => <>
          <Row gap={2} align="center">
            <SpaceAgentAvatar spaceId={props.sid} agent={a()} size="sm" />
            <Column grow gap={1}>
              <Label variant="strong" lines={2}>{a().name}</Label>
              <Label variant="caption" color="muted">@{a().handle}</Label>
            </Column>
            <If when={a().isDefault}><Badge tone="agent" label="Default" /></If>
          </Row>
          <Label lines={4}>{a().description}</Label>
          <Label variant="caption" color="muted" lines={4}>
            {a().tools.length ? a().tools.map(toolName).join(" · ") : "No tools enabled"}
          </Label>
          <Row>
            <Action testID="identity-card-agent-settings" label="Agent settings" viewRef={node => node.focus()} onEscape={props.close} onPress={() => {
              const sid = props.sid, id = a().id, current = localMentionIdentity(sid, { kind: "agent", agentId: id });
              if (!current) return;
              props.close();
              openSpaceSettings(sid, "agents");
              if (activeSpace()?.role === "owner") startEditAgent(id);
            }} />
          </Row>
        </>}
      </If>
    }>
      {u => <>
        <Row gap={2} align="center">
          <PersonAvatar userId={u().id} name={u().name} size="sm" />
          <Column grow gap={1}>
            <Label variant="strong" lines={2}>{u().name}</Label>
            <Label variant="caption" color="muted" lines={2}>@{u().id}</Label>
          </Column>
          <Badge tone="neutral" label={u().role === "owner" ? "Owner" : "Member"} />
        </Row>
        <If when={u().bio}>{bio => <Label lines={4}>{bio()}</Label>}</If>
        <Row>
          <Action testID="identity-card-copy-id" label="Copy ID" viewRef={node => node.focus()} onEscape={props.close} onPress={() => {
            if (localMentionIdentity(props.sid, { kind: "user", id: u().id })) {
              void Clipboard.writeText(u().id).then(() => announce("User ID copied"), () => announce("Could not copy the ID"));
            }
          }} />
        </Row>
      </>}
    </If>
  );
}
