import { Clipboard, announce, computed, effect, signal } from "nsh";
import type { ViewRef } from "nsh";
import type { Mention, RoomEvent } from "../../api/client";
import type { SpaceAgent } from "../../api/spaces";
import { activeSpace, composerScope, me } from "../../store";
import { Action, AnchoredPopover, Badge, Column, IdentityCardFrame, Label, MentionMessageLine, MentionMessageText, MentionToken, Row } from "../../ui";
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

type Part = { key: string; text: string; token?: { piece: MentionPiece; ordinal: number } };
function messageLines(text: string, pieces: readonly MentionPiece[]): Part[][] {
  const lines: Part[][] = [[]];
  const line = () => lines[lines.length - 1];
  const addText = (start: number, end: number) => {
    for (const m of text.slice(start, end).matchAll(/\n|[^\s]+[^\S\n]*|[^\S\n]+/gu)) {
      if (m[0] === "\n") lines.push([]);
      else line().push({ key: `text:${start + m.index!}`, text: m[0] });
    }
  };
  let end = 0;
  for (const [ordinal, piece] of pieces.entries()) {
    if (piece.end > text.length) {
      if (piece.start < text.length) { addText(end, piece.start); return lines; }
      break;
    }
    if (end < piece.start) addText(end, piece.start);
    line().push({ key: `mention:${piece.start}`, text: piece.literal, token: { piece, ordinal } });
    end = piece.end;
  }
  if (end < text.length) addText(end, text.length);
  return lines;
}

export function MentionedBody(props: { event: RoomEvent; sid: string; pieces: readonly MentionPiece[]; stateKey: string }) {
  const open = () => isOpen(props.stateKey);
  const view = computed(() => outputView(props.event.text, open()));
  const lines = computed(() => {
    const v = view();
    if (!v.foldable || open()) return messageLines(v.shown, props.pieces);
    const visible = v.shown.split("\n", FOLD_LINES).join("\n");
    return messageLines(visible.length > 600 ? [...visible].slice(0, 600).join("") : visible, props.pieces);
  });
  const card = signal<{ piece: MentionPiece; scope: string; eventId: number; anchor: ViewRef } | null>(null);
  const identity = () => {
    const target = card();
    return target && target.scope === composerScope() && target.eventId === props.event.id
      ? localMentionIdentity(props.sid, target.piece.mention) : null;
  };
  effect(() => { if (card() && !identity()) card.set(null); });
  const close = () => {
    const previous = card();
    const canRestore = !!identity();
    card.set(null);
    if (canRestore) previous?.anchor.focus();
  };
  return (
    <Column gap={1}>
      <Each of={lines()} key="index">
        {line => <MentionMessageLine>
          <Each of={line()} key={p => p.key}>
            {(part) => (
              <If when={part().token} else={<MentionMessageText text={part().text} />}>
                {(token) => {
                  const piece = () => token().piece;
                  let anchor: ViewRef | null = null;
                  const info = computed(() => localMentionIdentity(props.sid, piece().mention));
                  const self = () => {
                    const m = piece().mention;
                    return m.kind === "user" && m.id === me();
                  };
                  const generic = () => {
                    const m = piece().mention;
                    return m.kind === "agent" && !m.agentId;
                  };
                  const label = () => info() ? `@${info()!.name}` : generic() ? "@agent" : piece().literal;
                  return <MentionToken
                    testID={`mention-token:${props.event.id}:${token().ordinal}`}
                    tone={info() ? self() ? "self" : piece().mention.kind === "user" ? "person" : "agent" : generic() ? "agent" : "unavailable"}
                    label={label()}
                    accessibilityLabel={info() ? `${info()!.name}, mentioned. Open card.` : generic() ? label() : `${label()}, unavailable`}
                    ref={node => { anchor = node; }}
                    onPress={info() ? () => {
                      if (info() && anchor) card.set({ piece: piece(), eventId: props.event.id, scope: composerScope(), anchor });
                    } : undefined}
                  />;
                }}
              </If>
            )}
          </Each>
        </MentionMessageLine>}
      </Each>
      <If when={view().toggleLabel}>
        {label => <Row><Action label={label()} variant="ghost" onPress={() => { close(); toggle(props.stateKey); }} /></Row>}
      </If>
      <If when={view().notice}>{text => <Label variant="caption" color="muted">{text()}</Label>}</If>
      <If when={identity()}>
        {info => (
          <AnchoredPopover anchor={card()?.anchor} placement="bottom" align="start" onClose={close}
            testID="identity-card" accessibilityLabel={`About ${info().name}`}>
            <IdentityCardFrame>
              <IdentityDetails info={info()} sid={props.sid} close={close} />
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
