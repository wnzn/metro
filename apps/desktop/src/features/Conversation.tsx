// Pieces shared by the Chat and a Space's rooms: the connection banner, the
// transcript and the composer. They read the active place from the store.
//
// The transcript draws each entry with its author (a person's picture or
// initials and the name from their profile, their stable id on hover and for
// screen readers, "you" for the signed-in user; the agent and server actors
// with their own glyph, never initials) and the time the server recorded,
// in the device's zone. A date line starts each day; consecutive messages by
// one author share a header, and a continuation's time stays quiet until the
// pointer or keyboard focus is on it. A message addressed to the signed-in
// user carries an accent rail.
//
// Each message has a small toolbar (react, reply, forward, copy, and Delete
// where the person may delete it, after a confirmation) shown on hover and
// keyboard focus, and always on touch screens. Reactions open in a popover anchored
// to the toolbar's button. Explicit recipients are always shown; an audience
// phrase is left out only where a typed @agent already says the same.
//
// In personal Chat, Send asks the agent and Save note does not. In a Space,
// the composer has separate Ask agent and Send actions; explicit recipients
// still decide who a Space message reaches.

import { Clipboard, announce, cleanup, computed, effect, keymap, localeSettings, signal } from "nsh";
import type { CaretEvent, InputRef, JSX, ViewRef } from "nsh";
import {
  activeEvents,
  activeLoaded,
  activeReactions,
  activeSpace,
  actionError,
  canDelete,
  commandHint,
  composerCommandPlace,
  composerScope,
  compacting,
  deleteMessage,
  deletePending,
  eventsInRoom,
  focusTarget,
  draft,
  isReactable,
  me,
  notice,
  mainPlace,
  openLocation,
  openingReply,
  REACTION_EMOJI,
  reactionPending,
  reactionsInRoom,
  replyPane,
  roomEventsLoaded,
  roomEventScope,
  rooms,
  retryNow,
  runComposerCommand,
  saveChatNote,
  send,
  sending,
  setDraft,
  status,
  steeredIn,
  toggleReaction,
  activeRoomId,
} from "../store";
import { cancelQueuedTurn, queuedChatTurns } from "./meta/state";
import type { Reactions as ReactionMap, RoomEvent } from "../store";
import { choiceForTurn, ComposerModelControls } from "./composerModels";
import type { AttachedFile, EventAgent } from "../api/client";
import { agentById } from "./space/agents";
import { SpaceAgentAvatar } from "./space/SpaceAgentAvatar";
import {
  Action,
  AnchoredPopover,
  AskAgentButton,
  AttachmentNote,
  AttachmentTray,
  AuthorGlyph,
  AuthorName,
  AuthorTag,
  Badge,
  Banner,
  Column,
  ComposerBar,
  ComposerBox,
  ComposerField,
  ComposerHint,
  DaySeparator,
  DeleteConfirmation,
  DeletedMessage,
  DeleteMessageButton,
  DraftSuggestion,
  EmojiChoice,
  EmojiRow,
  EmptyConversation,
  FeedList,
  HeaderNote,
  InlineLink,
  Label,
  MessageFileCard,
  RemovedFileCard,
  MessageFiles,
  MessageFrame,
  MessageHeader,
  MessageToolbar,
  PopoverNote,
  ReactionChip,
  RecipientTag,
  Row,
  SendButton,
  StagedFileChip,
  SuggestionList,
  SuggestionOption,
  Timestamp,
  ToolbarIconButton,
} from "../ui";
import type { ChatGlyphName, Tone } from "../ui";
import { audienceText, kindLabel, mentionsMe, mentionText, reactionSummary, toneForEvent } from "./eventKinds";
import { ActivityGroupRow, FailureBody, FoldableText } from "./activity/ActivityRow";
import { failureText } from "./activity/failure";
import { disclosureKey, disclosureScope } from "./activity/disclosure";
import { DEFAULT_AGENT_NAME, groupTranscript, isSystemEvent } from "./activity/group";
import { ShellApprovalCard } from "./activity/ShellApprovalCard";
import { readShellApproval, readShellResolution } from "../api/shell";
import type { ShellResolution } from "../api/shell";
import { PageDraftCard } from "./pages/PageDraftCard";
import { SpacePostRow } from "./meta/SpacePostRow";
import { clockFor, stampOf } from "./chatTime";
import type { Stamp } from "./chatTime";
import { PersonAvatar } from "./profile/PersonAvatar";
import { profileName } from "./profile/profileState";
import { headerAudience, textNamesRecipients, transcriptRows } from "./transcriptRows";
import type { TranscriptRow } from "./transcriptRows";
import { stagedFor } from "./attachments/attachmentDraft";
import type { StagedFile } from "./attachments/attachmentDraft";
import { destinationCue, openMessageFile, pickAttachments, pickNote, picking } from "./attachments/attachmentSend";
import { unstage } from "./attachments/attachmentDraft";
import { formatSize } from "./library/libraryState";
import { mediaKindOf } from "../api/library";
import { MentionedBody, localMentionIdentity, sentMentionLabel } from "./mentions/MentionedBody";
import { verifiedAgents } from "./mentions/roster";
import { copyMentionText, leadingMentionsCoverRecipients, sentMentions } from "./mentions/sent";
import { SenderNotice } from "./senderNotices/SenderNotice";
import { openForward } from "./forward/state";
import { ReplySummary } from "./replies/ReplySummary";
import { commandSuggestions, parseCommand } from "./commands/registry";
import { CompactionMarker } from "./commands/CompactionMarker";

/** The user's zone and hour cycle; follows the system's settings as they change. */
const clock = computed(() => clockFor(localeSettings()));
type ReplyControl = (event: RoomEvent, opener?: ViewRef) => void;
type DirectTranscript = {
  id: string; events: () => readonly RoomEvent[]; loaded: () => boolean;
  reactions: () => ReactionMap; pending: () => string | null; react: (eventId: number, emoji: string) => void;
  /** Deleting the person's own messages here, when the place offers it. */
  deleting?: () => number | null; remove?: (event: RoomEvent) => Promise<boolean>;
};
type TranscriptActions = {
  roomId?: string; direct?: DirectTranscript; onReply?: ReplyControl; replyAnchor?: RoomEvent | null; readOnly?: boolean;
};

export const messageAuthorName = (event: RoomEvent) => authorOf(event, me()).name;
export const replyExcerpt = (event: RoomEvent) => event.text.trim().replace(/\s+/g, " ").slice(0, 180) || "Message";

// ------------------------------------------------------------------ the composer's field

let composerInput: InputRef | null = null;

/** Puts the keyboard focus in the composer, the caret after its text. */
export function focusComposer(caret = draft().length) {
  const input = composerInput;
  if (!input) return;
  input.focus();
  input.setSelection(caret);
}

/** Replaces the draft with `text` and focuses it; never sends. */
export function fillDraft(text: string) {
  setDraft(text);
  focusComposer();
}

// ------------------------------------------------------------------ connection

export function ConnectionBanner() {
  const retrying = () => {
    const s = status();
    return s.phase === "retrying" ? s : null;
  };
  return (
    <If when={retrying()}>
      {(s) => (
        <Banner tone="warning" testID="connection-banner">
          <Column grow gap={1}>
            <Label variant="strong" color="warning">
              {`Connection lost — retrying in ${Math.round(s().retryInMs / 1000)}s (attempt ${s().attempt})`}
            </Label>
            <Label variant="caption" color="warning">
              {s().message}
            </Label>
          </Column>
          <Action testID="retry-now" label="Retry now" onPress={retryNow} />
        </Banner>
      )}
    </If>
  );
}

// ------------------------------------------------------------------ transcript

/** A suggestion of the empty transcript: it fills the draft (and may add recipients), never sends. */
export type DraftIdea = { id: string; label: string; apply: () => void };

export function Transcript(props: TranscriptActions & { emptyText: string; emptyTitle?: string; ideas?: DraftIdea[]; testID?: string }) {
  const chat = () => mainPlace() === "chat";
  const events = computed(() => (props.direct?.events() ?? (props.roomId ? eventsInRoom(props.roomId) : activeEvents()))
    .filter(e => e.kind !== "compaction" || !props.direct && e.compaction));
  const rows = computed(() => transcriptRows(groupTranscript(events()), events(), clock()));
  const shellResolutions = computed(() => {
    const out = new Map<string, ShellResolution>();
    for (const e of events()) {
      if (e.kind !== "shell_approval_resolved" || !isSystemEvent(e)) continue;
      const r = readShellResolution(e.text);
      if (r && !out.has(r.approvalId)) out.set(r.approvalId, r); // the first decision wins
    }
    return out;
  });
  const ideas = () => props.ideas ?? [];
  const glyph = (): ChatGlyphName => (chat() || props.direct ? "chat" : "hash");
  let feed: import("nsh").ListRef | undefined;
  let reached: ReturnType<typeof focusTarget> = null;
  effect(() => {
    const target = focusTarget();
    if (!target) reached = null;
    if (!target || reached === target || target.scope !== (props.roomId ? roomEventScope(props.roomId) : composerScope())) return;
    const index = rows().findIndex(row => row.type === "event" && row.event.id === target.eventId);
    if (index >= 0) {
      reached = target;
      queueMicrotask(() => feed?.scrollToIndex({ index, viewPosition: 0.5, animated: false }));
    }
  });
  return (
    <FeedList
      testID={props.testID ?? "transcript"}
      ref={node => { feed = node; }}
      each={rows()}
      keyOf={(row) => row.key}
      empty={
        <If
          when={props.direct?.loaded() ?? (props.roomId ? roomEventsLoaded(props.roomId) : activeLoaded())}
          else={
            <Column grow align="center" justify="center" pad={6}>
              <Label color="muted">Loading…</Label>
            </Column>
          }
        >
          <EmptyConversation
            testID="transcript-empty"
            glyph={glyph()}
            title={props.emptyTitle ?? (chat() ? "Your Chat with the agent" : "Start the conversation")}
            text={props.emptyText}
          >
            <Each of={ideas()} key={(idea) => idea.id}>
              {(idea) => <DraftSuggestion testID={`suggestion:${idea().id}`} label={idea().label} onPress={idea().apply} />}
            </Each>
          </EmptyConversation>
        </If>
      }
    >
      {(row) => <TranscriptRowView row={row} actions={props} shellResolutions={shellResolutions} />}
    </FeedList>
  );
}

type Of<T extends TranscriptRow["type"]> = Extract<TranscriptRow, { type: T }>;

/**
 * Each agent turn (tool calls, tool output, provider-shared summaries) shows as
 * one collapsed group; people's messages, the agent's replies, errors and
 * status stay in full.
 */
function TranscriptRowView(props: { row: () => TranscriptRow; actions: TranscriptActions; shellResolutions: () => ReadonlyMap<string, ShellResolution> }) {
  const row = props.row;
  const as = <T extends TranscriptRow["type"]>(type: T) => (row().type === type ? (row() as Of<T>) : null);
  const shellResolutionFor = (e: RoomEvent) => {
    const request = readShellApproval(e.text);
    return request ? props.shellResolutions().get(request.approvalId) ?? null : null;
  };
  const card = () => {
    const entry = as("event"), where = mainPlace();
    if (!entry) return null;
    if (entry.event.compaction) return entry;
    if (entry.event.kind === "shell_approval" && where === "space" && isSystemEvent(entry.event)) return entry;
    if (entry.event.kind === "page_draft" && (where === "space" || where === "chat")) return entry;
    return where === "chat" && entry.event.kind === "space_post" && entry.event.activity?.spacePost ? entry : null;
  };
  return (
    <If
      when={as("day")}
      else={
        <If when={as("group")} else={
          <If when={card()} else={<MessageRow row={() => row() as Of<"event">} actions={props.actions} />}>
            {entry => <If when={entry().event.compaction} else={
              <MessageFrame gutter={<StampGutter stamp={entry().stamp} id={entry().event.id} />} continued>
                <If when={entry().event.kind === "shell_approval"} else={
                  <If when={entry().event.kind === "space_post"} else={<PageDraftCard event={entry().event} />}>
                    <SpacePostRow event={entry().event} />
                  </If>
                }>
                  <ShellApprovalCard event={entry().event} resolution={shellResolutionFor(entry().event)}
                    roomId={props.actions.roomId ?? activeRoomId()} />
                </If>
              </MessageFrame>
            }><CompactionMarker event={entry().event} stamp={entry().stamp}
              scope={props.actions.roomId ? roomEventScope(props.actions.roomId) : composerScope()} /></If>}
          </If>
        }>
          {(g) => (
            <MessageFrame gutter={<StampGutter stamp={g().stamp} id={g().key} />} continued>
              <ActivityGroupRow
                group={g().group}
                roomId={props.actions.roomId}
                agentName={g().group.agent?.name}
                agentAvatar={g().group.agent && activeSpace()?.id ? <SpaceAgentAvatar spaceId={activeSpace()!.id} agent={g().group.agent!} size="sm" /> : undefined}
              />
            </MessageFrame>
          )}
        </If>
      }
    >
      {(d) => <DaySeparator label={d().label} testID={`day:${d().key}`} />}
    </If>
  );
}

function StampGutter(props: { stamp: Stamp | null; id: string | number }): JSX.Element {
  return (
    <If when={props.stamp}>
      {(s) => <Timestamp compact text={s().short} full={s().full} testID={`time:${props.id}`} />}
    </If>
  );
}

/** An error or failure the server posted (never a person's message). */
const isFailureKind = (kind: string) => /(error|fail)/.test(kind.trim().toLowerCase());

/** Who wrote an event, as the transcript shows them. */
type Author =
  | { kind: "person"; userId: string | null; name: string; mine: boolean }
  | { kind: "agent"; name: string; badge: EventAgent | null }
  | { kind: "server"; name: string; tone: Tone; label: string };

function authorOf(e: RoomEvent, myId: string | null): Author {
  const kind = e.kind.trim().toLowerCase();
  if (kind === "message") {
    // Only the stable id says who it is; a display name never does.
    const userId = e.actorId ?? null;
    return { kind: "person", userId, name: profileName(userId) ?? (e.actor.trim() || userId || "unknown"), mine: !!userId && userId === myId };
  }
  if (kind === "agent") {
    // A Space agent's reply shows that agent (a thread's agent keeps the thread's name).
    const badge = mainPlace() === "space" ? e.activity?.agent ?? null : null;
    return { kind: "agent", name: e.activity?.name || badge?.name || DEFAULT_AGENT_NAME, badge };
  }
  return { kind: "server", name: e.actor.trim() || "system", tone: toneForEvent(e.kind, e.actor), label: kindLabel(e.kind) };
}

function recipientAgent(id: string | undefined) {
  const sid = activeSpace()?.id;
  return mainPlace() === "space" ? (sid ? verifiedAgents(sid)?.find(a => a.id === id) : undefined) : agentById(sid, id);
}

function recipientText(m: NonNullable<RoomEvent["mentions"]>[number], myId: string | null, lastKnown?: string): string {
  if (m.kind === "agent" && !m.addressedBy && mainPlace() === "chat") return "to the agent";
  // An agent the classifier picked shows like any agent recipient.
  if (m.kind === "agent" && m.addressedBy && m.addressedBy !== "classifier" && (m.agentId || mainPlace() === "space")) {
    const a = recipientAgent(m.agentId);
    const reason = { schedule: "scheduled prompt", reply: "reply", metro: "via Metro" };
    return `${a?.name ?? "agent"} · ${reason[m.addressedBy]}`;
  }
  if (mainPlace() === "space") {
    const sid = activeSpace()?.id;
    const fallback = lastKnown ?? (m.kind === "user" ? "Person unavailable" : m.agentId ? "Agent unavailable" : "@agent");
    return sid ? sentMentionLabel(sid, m, fallback) : fallback;
  }
  if (m.kind === "user" && m.id !== myId) {
    const name = profileName(m.id);
    if (name) return `@${name}`;
  }
  return mentionText(m, myId);
}

/** What a screen reader hears for a recipient: the display name and the stable id behind it. */
function recipientDescription(m: NonNullable<RoomEvent["mentions"]>[number], myId: string | null, lastKnown?: string): string {
  if (m.kind === "agent" && m.agentId) {
    const a = recipientAgent(m.agentId);
    return `Addressed to ${a ? `${a.name} (@${a.handle})` : "an agent"}${m.addressedBy && m.addressedBy !== "classifier" ? ` (${recipientText(m, myId)})` : ""}`;
  }
  if (m.kind === "agent") return `Addressed to the agent${m.addressedBy && m.addressedBy !== "classifier" ? ` (${mentionText(m, myId)})` : ""}`;
  if (m.id === myId) return "Addressed to you";
  const sid = activeSpace()?.id;
  if (mainPlace() === "space") {
    const identity = sid && localMentionIdentity(sid, m);
    if (!identity) return lastKnown ? `Addressed to ${lastKnown}, unavailable` : "Addressed to an unavailable person";
    return identity.name !== m.id ? `Addressed to ${identity.name} (@${m.id})` : `Addressed to @${m.id}`;
  }
  const name = profileName(m.id);
  return name && name !== m.id ? `Addressed to ${name} (@${m.id})` : `Addressed to @${m.id}`;
}

export function ThreadAnchor(props: { event: RoomEvent }) {
  return <MessageRow row={() => ({
    type: "event", key: `anchor:${props.event.id}`, event: props.event,
    stamp: stampOf(props.event.createdAt, clock()), continued: false,
  })} actions={{ readOnly: true }} />;
}

/** What remains of a deleted message that anchors a thread: one quiet line and its replies. */
function DeletedRow(props: { row: () => Of<"event">; actions: TranscriptActions }) {
  const event = () => props.row().event;
  const replies = () => props.actions.onReply && !props.actions.readOnly && event().replies?.count ? event().replies : undefined;
  return (
    <MessageFrame testID={`${props.actions.readOnly ? "anchor" : "event"}:${event().id}`}
      gutter={<AuthorGlyph testID={`avatar:${event().id}`} kind="system" tone="neutral" />}>
      <DeletedMessage testID={`deleted:${event().id}`} />
      <If when={replies()}>
        {r => <ReplySummary eventId={event().id} summary={r()} last={stampOf(r().lastAt, clock())?.short}
          onOpen={opener => { if (!openingReply()) props.actions.onReply?.(event(), opener); }} />}
      </If>
    </MessageFrame>
  );
}

function MessageRow(props: { row: () => Of<"event">; actions: TranscriptActions }) {
  return (
    <If when={props.row().event.kind !== "deleted"} else={<DeletedRow row={props.row} actions={props.actions} />}>
      <LiveMessageRow row={props.row} actions={props.actions} />
    </If>
  );
}

function LiveMessageRow(props: { row: () => Of<"event">; actions: TranscriptActions }) {
  const event = () => props.row().event;
  const spaceId = () => mainPlace() === "space" ? activeSpace()?.id : undefined;
  const pieces = computed(() => spaceId() ? sentMentions(event()) : []);
  const storedSpelling = (m: NonNullable<RoomEvent["mentions"]>[number]) =>
    pieces().find(piece => piece.mention === m)?.literal;
  const tone = () => toneForEvent(event().kind, event().actor);
  const author = () => authorOf(event(), me());
  const forMe = () => mentionsMe(event().mentions, me());
  const audience = () => headerAudience(event().audience, event().mentions);
  const hasRecipients = () => audience().agentBadge || audience().peopleNote || (event().mentions?.length ?? 0) > 0;
  const continued = () => props.row().continued && !event().via;
  /**
   * A follow-up's recipients: in a slim line level with the time in the
   * gutter. In a Space only actual leading bindings can stand in for it.
   */
  const leadingRecipients = () => spaceId()
    ? leadingMentionsCoverRecipients(event(), pieces())
    : textNamesRecipients(event().text, event().mentions);
  const followUpRecipients = () =>
    continued() && hasRecipients() && !(leadingRecipients() && !audience().agentBadge && !audience().peopleNote);
  /** A failure says one plain sentence; the technical text folds under Details. */
  const failure = () => (isFailureKind(event().kind) ? failureText(event().text) : null);
  const person = () => {
    const a = author();
    return a.kind === "person" ? a : null;
  };
  const server = () => {
    const a = author();
    return a.kind === "server" ? a : null;
  };
  /** The Space agent that wrote it, when its event names one: its picture replaces the generic glyph. */
  const agentBadge = () => {
    const a = author();
    return a.kind === "agent" && a.badge && activeSpace()?.id ? a.badge : null;
  };
  const roomId = () => props.actions.roomId;
  const reactable = () => !props.actions.readOnly && isReactable(event());
  const copyable = () => !props.actions.readOnly && event().text.trim().length > 0;
  const replyable = () => !!props.actions.onReply && (event().kind === "message" || event().kind === "agent" || event().kind === "thread");
  const forwardable = () => !props.actions.readOnly && (event().kind === "message" || event().kind === "agent");
  const direct = () => props.actions.direct;
  const deletable = () => !props.actions.readOnly && (direct()
    ? !!direct()!.remove && canDelete(event(), "dm")
    : canDelete(event(), mainPlace() === "chat" ? "chat" : "space"));
  const deleting = () => (direct() ? direct()!.deleting?.() ?? null : deletePending()) === event().id;
  const nearby = () => direct()?.events() ?? (roomId() ? eventsInRoom(roomId()!) : activeEvents());
  const bodyScope = () => direct() ? `${disclosureScope()}\u0001dm:${direct()!.id}` : disclosureScope();
  const showQuote = () => !!event().replyTo && props.actions.replyAnchor?.id !== event().replyTo;
  const quoted = () => nearby().find(e => e.id === event().replyTo);
  const forwarded = () => event().forwarded;
  const forwardedTime = () => stampOf(forwarded()?.sentAt, clock());
  const sourceRoom = () => rooms().find(r => r.id === forwarded()?.sameSpace?.roomId);
  // A thread opened without a reply yet shows nothing under its message.
  const replies = () => props.actions.onReply && !props.actions.readOnly && event().replies?.count
    ? event().replies : undefined;
  const palette = signal(false);
  const confirming = signal(false);
  const copied = signal(false);
  let reactButton: ViewRef | null = null;
  let deleteButton: ViewRef | null = null;
  let replyButton: ViewRef | undefined;
  let forwardButton: ViewRef | undefined;
  let frame: ViewRef | undefined;
  const reply = () => { if (!openingReply()) props.actions.onReply?.(event(), replyButton); };
  const forward = (opener?: ViewRef) => {
    const sid = spaceId(), rid = roomId(), id = event().id;
    const dm = direct();
    if (dm) openForward({ kind: "dm", dm_id: dm.id, event_id: id }, event(), opener,
      () => direct()?.id === dm.id && dm.events().some(e => e.id === id));
    else if (sid && rid) openForward({ kind: "space", space_id: sid, room_id: rid, event_id: id }, event(), opener,
      () => spaceId() === sid && eventsInRoom(rid).some(e => e.id === id) &&
        (!rooms().some(r => r.id === rid && r.anchorEventId !== undefined) || replyPane()?.roomId === rid));
    else if (mainPlace() === "chat") openForward({ kind: "chat", event_id: event().id }, event(), opener);
  };
  const closePalette = () => {
    palette.set(false);
    reactButton?.focus();
  };
  const closeConfirm = () => {
    confirming.set(false);
    deleteButton?.focus();
  };
  const confirmDelete = async () => {
    const target = event();
    const dm = direct();
    const done = dm?.remove ? await dm.remove(target) : await deleteMessage(target, roomId());
    // Gone from the transcript: nothing to give the focus back to.
    if (done) confirming.set(false);
  };
  const deleteDetail = () => {
    if (direct()) return "It's removed for both of you. This can't be undone.";
    const where = mainPlace() === "chat" ? "It's removed from your Chat" : "It's removed for everyone in this Space";
    return `${where}, with its reactions, and agents won't see it again. Work it already started keeps going. This can't be undone.`;
  };
  const copy = () => {
    const sid = spaceId();
    const text = sid ? copyMentionText(event().text, pieces(), piece => sentMentionLabel(sid, piece.mention, piece.literal)) : event().text;
    void Clipboard.writeText(text).then(
      () => {
        copied.set(true);
        announce("Message copied");
        setTimeout(() => copied.set(false), 1500);
      },
      () => announce("Could not copy the message"),
    );
  };
  return (
    <MessageFrame
      testID={`${props.actions.readOnly ? "anchor" : "event"}:${event().id}`}
      viewRef={node => { frame = node; }}
      shortcuts={{ ...(replyable() ? { r: reply } : {}), ...(forwardable() ? { f: () => forward(frame) } : {}) }}
      highlight={forMe()}
      continued={continued()}
      selectable
      toolbar={
        <If when={reactable() || copyable() || replyable() || forwardable() || deletable()}>
          <MessageToolbar open={palette() || confirming()} testID={`toolbar:${event().id}`}>
            <If when={reactable()}>
              <ToolbarIconButton
                testID={`react:${event().id}`}
                icon="reaction"
                accessibilityLabel={palette() ? "Close the reaction choices" : "Add a reaction"}
                tooltip="Add a reaction"
                active={palette()}
                ref={(node) => (reactButton = node)}
                onPress={() => palette.set(!palette())}
              />
            </If>
            <If when={replyable()}>
              <ToolbarIconButton testID={`reply:${event().id}`} icon="reply" tooltip="Reply" accessibilityLabel={`Reply to ${author().name}`}
                ref={node => { replyButton = node; }} onPress={reply} />
            </If>
            <If when={forwardable()}>
              <ToolbarIconButton testID={`forward:${event().id}`} icon="forward" tooltip="Forward" accessibilityLabel="Forward this message"
                ref={node => { forwardButton = node; }} onPress={() => forward(forwardButton)} />
            </If>
            <If when={copyable()}>
              <ToolbarIconButton
                testID={`copy:${event().id}`}
                icon={copied() ? "check" : "copy"}
                accessibilityLabel={copied() ? "Copied" : "Copy the message text"}
                tooltip={copied() ? "Copied" : "Copy text"}
                onPress={copy}
              />
            </If>
            <If when={deletable()}>
              <DeleteMessageButton
                testID={`delete:${event().id}`}
                accessibilityLabel="Delete this message"
                tooltip="Delete"
                active={confirming()}
                ref={node => { deleteButton = node; }}
                onPress={() => confirming.set(!confirming())}
              />
            </If>
          </MessageToolbar>
          <If when={palette()}>
            <ReactionPopover eventId={event().id} roomId={roomId()} direct={direct()} anchor={reactButton} onClose={closePalette} />
          </If>
          <If when={confirming() && deletable()}>
            <AnchoredPopover anchor={deleteButton} onClose={closeConfirm} placement="top" align="end" role="dialog"
              testID={`confirm-delete:${event().id}`} accessibilityLabel="Delete this message?">
              <DeleteConfirmation testID={`delete-dialog:${event().id}`} title="Delete this message?" detail={deleteDetail()}
                busy={deleting()} onConfirm={() => void confirmDelete()} onCancel={closeConfirm} />
            </AnchoredPopover>
          </If>
        </If>
      }
      gutter={
        <If when={!continued()} else={<StampGutter stamp={props.row().stamp} id={event().id} />}>
          <If
            when={person()}
            else={
              <If
                when={agentBadge()}
                else={
                  <AuthorGlyph
                    testID={`avatar:${event().id}`}
                    kind={author().kind === "agent" ? "agent" : server()?.tone === "runner" ? "runner" : "system"}
                    tone={author().kind === "agent" ? "agent" : server()?.tone ?? "neutral"}
                  />
                }
              >
                {(b) => <SpaceAgentAvatar testID={`avatar:${event().id}`} spaceId={activeSpace()!.id} agent={b()} />}
              </If>
            }
          >
            {(p) => <PersonAvatar userId={p().userId} name={p().name} testID={`avatar:${event().id}`} />}
          </If>
        </If>
      }
    >
      <If when={!continued() || followUpRecipients()}>
        <MessageHeader testID={`header:${event().id}`} slim={continued()}>
          <If when={!continued()}>
            <AuthorName
              testID={`author:${event().id}`}
              name={author().name}
              agent={author().kind === "agent"}
              detail={person()?.userId && person()!.userId !== person()!.name ? `@${person()!.userId}` : undefined}
              accessibilityLabel={author().kind === "agent" ? `${author().name}, agent` : undefined}
            />
            <If when={person()?.mine}>
              <AuthorTag testID={`you:${event().id}`} label="you" tone="neutral" />
            </If>
            <If when={event().via === "metro"}>
              <AuthorTag testID={`via-metro:${event().id}`} label="via Metro" tooltip={`Sent from ${author().name}'s Chat by Metro`} />
            </If>
            <If when={server()}>{(a) => <Badge tone={a().tone} label={a().label} testID={`kind:${event().id}`} />}</If>
            <If when={props.row().stamp}>
              {(s) => <Timestamp text={s().short} full={s().full} testID={`time:${event().id}`} />}
            </If>
          </If>
          <If when={audience().peopleNote}>
            <HeaderNote text={audienceText("users") ?? ""} testID={`audience:${event().id}`} accessibilityLabel={audience().spoken ?? undefined} />
          </If>
          <If when={audience().agentBadge}>
            <RecipientTag
              testID={`recipient:${event().id}:ask-agent`}
              slim={continued()}
              tone="agent"
              label="to the agent"
              accessibilityLabel={`${audience().spoken ?? "Addressed to the agent"} (Ask agent)`}
            />
          </If>
          <Each of={event().mentions ?? []} key={(m) => (m.kind === "agent" ? `agent:${m.agentId ?? ""}` : `user:${m.id}`)}>
            {(m) => (
              <RecipientTag
                testID={`recipient:${event().id}:${m().kind === "agent" ? ((m() as { agentId?: string }).agentId ? `agent:${(m() as { agentId: string }).agentId}` : "agent") : (m() as { id: string }).id}`}
                tone={m().kind === "agent" ? "agent" : "neutral"}
                slim={continued()}
                self={m().kind === "user" && (m() as { id: string }).id === me()}
                label={recipientText(m(), me(), storedSpelling(m()))}
                accessibilityLabel={recipientDescription(m(), me(), storedSpelling(m()))}
              />
            )}
          </Each>
        </MessageHeader>
      </If>
      <If when={showQuote()}>
        <Label variant="caption" color="muted" lines={2} testID={`reply-quote:${event().id}`}>
          {quoted() ? `Reply to ${messageAuthorName(quoted()!)} · ${replyExcerpt(quoted()!)}` : "Reply to an earlier message"}
        </Label>
      </If>
      <If when={forwarded()}>
        {f => <Row gap={1} align="center" wrap testID={`forwarded:${event().id}`}>
          <If when={f().sameSpace && sourceRoom() && spaceId() && !props.actions.readOnly}
            else={<Label variant="caption" color="muted">Forwarded from {f().authorName}</Label>}>
            <InlineLink label={`Forwarded from ${f().authorName} in ${sourceRoom()?.anchorEventId ? "Thread" : `#${sourceRoom()?.name}`}`}
              onPress={() => { const s = f().sameSpace; if (s) void openLocation({ spaceId: spaceId()!, roomId: s.roomId, eventId: s.eventId }); }} />
          </If>
          <If when={forwardedTime()}>
            {s => <Timestamp text={`· ${s().short}`} full={s().full} testID={`forwarded-time:${event().id}`} />}
          </If>
        </Row>}
      </If>
      <If
        when={failure()}
        else={
          <If when={event().text.trim().length > 0}>
            <If when={event().kind === "message" || event().kind === "agent"}
              else={<FoldableText text={event().text} mono={tone() === "runner"} stateKey={disclosureKey(disclosureScope(), event().id, "all")} />}>
              <MentionedBody event={event()} sid={spaceId()} pieces={pieces()} stateKey={disclosureKey(bodyScope(), event().id, "all")} />
            </If>
          </If>
        }
      >
        {(f) => <FailureBody sentence={f().sentence} details={f().details} stateKey={disclosureKey(disclosureScope(), event().id, "details")} testID={`failure:${event().id}`} />}
      </If>
      <If when={event().attachments?.length ? event().attachments! : null}>
        {(files) => (
          <MessageFiles testID={`files:${event().id}`}>
            <Each of={files()} key="index">
              {(f, index) => (
                <If when={!f().removed ? (f() as AttachedFile) : null} else={<RemovedFileCard testID={`file-removed:${event().id}:${index()}`} />}>
                  {(file) => (
                    <MessageFileCard
                      testID={`file:${event().id}:${file().assetId}`}
                      name={file().name}
                      detail={fileDetail(file())}
                      kind={mediaKindOf(file().mimeType)}
                      onOpen={() => openFile(file())}
                    />
                  )}
                </If>
              )}
            </Each>
          </MessageFiles>
        )}
      </If>
      <If when={!direct() && event().kind === "message"}>
        <DeliveryNote eventId={event().id} events={nearby()} />
      </If>
      <If when={reactable()}>
        <Reactions eventId={event().id} roomId={roomId()} direct={direct()} />
      </If>
      <If when={replies()}>
        {r => <ReplySummary eventId={event().id} summary={r()} last={stampOf(r().lastAt, clock())?.short}
          onOpen={opener => { if (!openingReply()) props.actions.onReply?.(event(), opener); }} />}
      </If>
      <If when={!props.actions.readOnly && !direct()}><SenderNotice event={event} focusComposer={focusComposer} /></If>
    </MessageFrame>
  );
}

/** "Steered" on a message delivered into a running agent turn; "Queued" on the user's own queued Chat message. */
function DeliveryNote(props: { eventId: number; events: readonly RoomEvent[] }) {
  const queued = () => (mainPlace() === "chat" ? queuedChatTurns().get(props.eventId) ?? null : null);
  return (
    <If when={!steeredIn(props.events).has(props.eventId)} else={
      <HeaderNote testID={`steered:${props.eventId}`} text="Steered" accessibilityLabel="Added to the agent's running reply" />
    }>
      <If when={queued()}>
        {(turn) => (
          <Row gap={2} align="center" testID={`queued:${props.eventId}`}>
            <HeaderNote text="Queued" accessibilityLabel="Queued: runs after the current reply" />
            <InlineLink testID={`unqueue:${props.eventId}`} label="Cancel" accessibilityLabel="Remove from the queue"
              onPress={() => void cancelQueuedTurn(turn())} />
          </Row>
        )}
      </If>
    </If>
  );
}

/** A message file's quiet line: its size and the library it is in (the message's place says which). */
function fileDetail(f: AttachedFile): string {
  const where = mainPlace() === "chat" ? "your Library" : `${activeSpace()?.name ?? "Space"} Library`;
  return `${formatSize(f.sizeBytes)} · ${where}`;
}

/** Opens a message's file in the Library viewer (membership checked here and by the server). */
function openFile(f: AttachedFile) {
  const sid = activeSpace()?.id;
  if (mainPlace() !== "chat" && (mainPlace() !== "space" || !sid)) {
    notice.set("This Space is no longer available.");
    return;
  }
  const problem = openMessageFile(f, mainPlace() === "chat" ? null : sid!);
  if (problem) notice.set(problem);
}

/**
 * The reaction choices, anchored to the toolbar's button: the focus moves to
 * the first emoji; Left and Right move, Return or Space picks, Escape or a
 * press outside closes and gives the focus back to the button.
 */
type ReactionTarget = { eventId: number; roomId?: string; direct?: DirectTranscript };
const reactionsFor = (target: ReactionTarget) =>
  (target.direct?.reactions() ?? (target.roomId ? reactionsInRoom(target.roomId) : activeReactions()))[target.eventId] ?? {};
const reactTo = (target: ReactionTarget, emoji: string) =>
  target.direct ? target.direct.react(target.eventId, emoji) : void toggleReaction(target.eventId, emoji, target.roomId);

function ReactionPopover(props: ReactionTarget & { anchor: ViewRef | null; onClose: () => void }) {
  const byEmoji = () => reactionsFor(props);
  const mine = (emoji: string) => (byEmoji()[emoji] ?? []).includes(`user:${me()}`);
  const pick = (emoji: string) => {
    props.onClose();
    reactTo(props, emoji);
  };
  return (
    <AnchoredPopover
      anchor={props.anchor}
      onClose={props.onClose}
      placement="top"
      align="end"
      role="menu"
      testID={`palette:${props.eventId}`}
      accessibilityLabel="Reactions"
    >
      <EmojiRow accessibilityLabel="Choose a reaction">
        <Each of={[...REACTION_EMOJI]} key={(e) => e}>
          {(emoji, index) => (
            <EmojiChoice
              testID={`pick-reaction:${props.eventId}:${emoji()}`}
              emoji={emoji()}
              mine={mine(emoji())}
              accessibilityLabel={mine(emoji()) ? `Remove your ${emoji()} reaction` : `React with ${emoji()}`}
              ref={(node) => {
                if (index() === 0) node.focus();
              }}
              onPress={() => pick(emoji())}
            />
          )}
        </Each>
      </EmojiRow>
    </AnchoredPopover>
  );
}

/** The reactions so far; nothing when there are none. */
function Reactions(props: ReactionTarget) {
  const byEmoji = () => reactionsFor(props);
  const emojis = () => Object.keys(byEmoji()).filter((e) => (byEmoji()[e]?.length ?? 0) > 0);
  const pending = (emoji: string) => (props.direct ? props.direct.pending() : reactionPending()) === `${props.eventId}|${emoji}`;
  return (
    <If when={emojis().length > 0}>
      <Row gap={1} wrap align="center" testID={`reactions:${props.eventId}`}>
        <Each of={emojis()} key={(e) => e}>
          {(emoji) => {
            const summary = () => reactionSummary(byEmoji()[emoji()] ?? [], me());
            return (
              <ReactionChip
                testID={`reaction:${props.eventId}:${emoji()}`}
                emoji={emoji()}
                count={summary().count}
                mine={summary().mine}
                agent={summary().agent}
                busy={pending(emoji())}
                accessibilityLabel={`${emoji()} ${summary().description}. ${summary().mine ? "Remove yours" : "Add yours"}`}
                onPress={() => reactTo(props, emoji())}
              />
            );
          }}
        </Each>
      </Row>
    </If>
  );
}

// ------------------------------------------------------------------ composer

/**
 * What went wrong with the last action, else a notice about the place: one
 * banner, never both, and none while the connection banner already says the
 * server can't be reached.
 */
function ComposerFeedback() {
  const offline = () => status().phase === "retrying";
  return (
    <If
      when={actionError() && !offline() ? actionError() : null}
      else={
        <If when={notice()}>
          {(message) => (
            <Banner tone="system" testID="notice">
              <Label variant="caption" color="system">
                {message()}
              </Label>
            </Banner>
          )}
        </If>
      }
    >
      {(message) => (
        <Banner tone="danger" testID="action-error">
          <Label variant="caption" color="danger">
            {message()}
          </Label>
        </Banner>
      )}
    </If>
  );
}

/** One staged file, with a small picture for images (a `blob:` URL of the picked bytes, revoked when it goes). */
function StagedChip(props: { scope: string; file: StagedFile }) {
  let url: string | null = null;
  if (props.file.kind === "png" || props.file.kind === "jpeg") {
    try {
      url = URL.createObjectURL(props.file.file);
    } catch {
      url = null;
    }
  }
  cleanup(() => {
    if (url) URL.revokeObjectURL(url);
  });
  return (
    <StagedFileChip
      testID={`staged:${props.file.name}`}
      name={props.file.name}
      detail={props.file.assetId ? `${formatSize(props.file.size)} · in the Library` : formatSize(props.file.size)}
      kind={mediaKindOf(props.file.mime)}
      previewSrc={url}
      busy={sending() !== null}
      onRemove={() => unstage(props.scope, props.file.key)}
    />
  );
}

/** The files staged for the active place, where they go, and what a pick left out. */
function StagedFiles() {
  const scope = () => composerScope();
  const files = () => stagedFor(scope());
  const note = () => {
    const n = pickNote();
    return n && n.scope === scope() ? n.text : null;
  };
  return (
    <If when={files().length > 0} else={<If when={note()}>{(t) => <AttachmentNote testID="attach-note" text={t()} />}</If>}>
      <AttachmentTray testID="attachments" cue={destinationCue(scope())} note={note()}>
        <Each of={files()} key={(f) => f.key}>
          {(f) => <StagedChip scope={scope()} file={f()} />}
        </Each>
      </AttachmentTray>
    </If>
  );
}

export function Composer(props: {
  placeholder: string;
  /** How sending works here (Return, @mentions, what Ask agent asks): Ask agent's tooltip and spoken description. */
  agentHint: string;
  disabled?: boolean;
  /** Inside the box, above the field (the recipients of a room message), and its popovers. */
  above?: JSX.Element;
  /** Tools at the bottom left of the box, after the paperclip (the @ button). */
  tools?: JSX.Element;
  /** Return while a suggestion list is open: return true when it picked one (nothing is sent). */
  onSubmitIntercept?: () => boolean;
  /** The caret moved in the field (its box in the window anchors mention suggestions). */
  onCaretMove?: (e: CaretEvent) => void;
  onSelectionChange?: (selection: { start: number; end: number }) => void;
  /** The field took or lost the keyboard focus. */
  onFocusChange?: (focused: boolean) => void;
  /** The agent Ask agent asks (a Space with several agents); none: the default (or the classifier's pick). */
  askAgentId?: string;
  replyAgentId?: string;
  /** Ask agent's label when it names the chosen agent ("Ask Coder"). */
  askLabel?: string;
  /** The chosen agent's picture on Ask agent. */
  askPicture?: JSX.Element;
  /** The agent chooser joined to Ask agent. */
  askChooser?: JSX.Element;
  /** Beside Chat Send while a reply is running (Stop, and what Send does meanwhile). Send stays. */
  stopReply?: JSX.Element;
}) {
  const hasFiles = () => stagedFor(composerScope()).length > 0;
  const ready = () => !props.disabled && status().phase !== "signedOut" && !sending() && !compacting();
  const canSend = () => ready() && (draft().trim().length > 0 || hasFiles());
  // A Space Ask needs a typed question; Chat Send can start a turn with files alone.
  const canAsk = () => ready() && draft().trim().length > 0;
  const personal = () => mainPlace() === "chat";
  const parsed = computed(() => parseCommand(draft(), composerCommandPlace()));
  const command = () => parsed() !== null;
  const focused = signal(false);
  const dismissed = signal<string | null>(null);
  const selected = signal(0);
  const suggestions = computed(() => ready() && !hasFiles() ? commandSuggestions(draft(), composerCommandPlace()) : []);
  const suggestionKey = () => `${composerScope()}:${draft()}`;
  const listOpen = () => suggestions().length > 0 && dismissed() !== suggestionKey();
  const choose = (index: number) => {
    const item = suggestions()[index];
    if (!listOpen() || !item) return false;
    setDraft(`/${item.name} `);
    focusComposer();
    return true;
  };
  effect(() => {
    if (!listOpen() || !focused()) return;
    const move = (by: number) => () => {
      const options = suggestions();
      if (!options.length) return;
      const i = (selected() + by + options.length) % options.length;
      selected.set(i);
      announce(`/${options[i].name}, ${i + 1} of ${options.length}`);
    };
    return keymap.bind({
      down: move(1), up: move(-1), tab: () => { choose(selected()); },
      escape: () => dismissed.set(suggestionKey()),
    }, { when: "always", repeat: true });
  });
  const post = (ask: boolean) => {
    if (!(ask && !personal() ? canAsk() : canSend())) return;
    if (!personal()) {
      void send(ask, choiceForTurn, ask ? props.askAgentId : undefined, props.replyAgentId);
    } else if (ask) {
      const { error, choice } = choiceForTurn();
      if (error) { actionError.set(error); return; }
      void send(ask, choice);
    } else void send(false);
  };
  const submit = () => {
    if (command()) {
      if (!choose(selected()) && ready()) void runComposerCommand();
      return;
    }
    if (props.onSubmitIntercept?.()) return;
    post(personal());
  };
  return (
    <Column padX={4} padY={3} gap={2}>
      <ComposerFeedback />
      <ComposerBox testID="composer-box">
        {props.above}
        <StagedFiles />
        <If when={listOpen()}>
          <Column padX={3} padY={2}>
            <SuggestionList accessibilityLabel="Commands" testID="command-suggestions">
              <PopoverNote text="Commands" />
              <Each of={suggestions()} key={item => item.name}>
                {(item, index) => <SuggestionOption title={`/${item().name} ${item().args}`} detail={item().description}
                  accessibilityLabel={`/${item().name} ${item().args} — ${item().description}`}
                  testID={`command-option:${item().name}`} active={index() === selected()} onPress={() => { choose(index()); }} />}
              </Each>
            </SuggestionList>
          </Column>
        </If>
        <ComposerField
          testID="composer"
          ref={(node) => (composerInput = node)}
          accessibilityLabel={mainPlace() === "chat" ? "Message your Chat" : "Message"}
          value={draft()}
          onChange={(text) => { dismissed.set(null); selected.set(0); setDraft(text); }}
          onSubmit={submit}
          onFocusChange={(f) => { focused.set(f); props.onFocusChange?.(f); }}
          onCaretMove={props.onCaretMove}
          onSelectionChange={props.onSelectionChange}
          placeholder={props.placeholder}
          disabled={props.disabled}
        />
        <If when={command()}>
          <Column padX={3} gap={1}>
            <If when={commandHint()} else={
              <If when={parsed()?.command?.privacy(composerCommandPlace())}>
                {hint => <ComposerHint text={hint()} />}
              </If>
            }>{hint => <ComposerHint text={hint()} testID="command-hint" />}</If>
          </Column>
        </If>
        <ComposerBar
          leading={
            <>
              <ToolbarIconButton
                testID="attach"
                icon="attach"
                accessibilityLabel={mainPlace() === "chat" ? "Attach files (saved to your personal Library)" : "Attach files (saved to this Space's Library)"}
                tooltip="Attach files"
                active={picking()}
                disabled={props.disabled || status().phase === "signedOut" || picking()}
                onPress={() => void pickAttachments()}
              />
              {props.tools}
              <If when={!command()}><ComposerModelControls /></If>
            </>
          }
          trailing={
            <If when={command()} else={<If when={personal()} else={
              <>
                <AskAgentButton
                  testID="ask-agent"
                  label={props.askLabel ?? "Ask agent"}
                  picture={props.askPicture}
                  chooser={props.askChooser}
                  busyLabel="Asking…"
                  accessibilityLabel={`Send and ask the agent to answer. ${props.agentHint}${hasFiles() ? " Attached files are saved to the Library, not sent to the agent." : ""}`}
                  tooltip={props.agentHint}
                  busy={sending() === "agent"}
                  disabled={!canAsk()}
                  onPress={() => post(true)}
                />
                <SendButton
                  testID="send"
                  accessibilityLabel={hasFiles() ? "Send with files" : "Send"}
                  tooltip="Send (Return)"
                  busy={sending() === "message"}
                  disabled={!canSend()}
                  onPress={() => post(false)}
                />
              </>
            }>
              <SendButton
                testID="save-note"
                note
                accessibilityLabel="Save note without asking the agent"
                tooltip="Save note without asking the agent"
                busy={sending() === "message"}
                disabled={!canSend()}
                onPress={() => void saveChatNote()}
              />
              {props.stopReply}
              <SendButton
                testID="send"
                accessibilityLabel={hasFiles() ? "Send to your agent. The agent cannot read attached files." : "Send to your agent"}
                tooltip="Send to your agent (Return)"
                busy={sending() === "agent"}
                disabled={!canSend()}
                onPress={() => post(true)}
              />
            </If>}>
              <Action compact variant="primary" testID="run-command" label="Run command" busyLabel="Summarizing…"
                busy={compacting()} disabled={!ready()} onPress={submit} />
            </If>
          }
        />
      </ComposerBox>
    </Column>
  );
}

export { kindLabel };
