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
// Each message has a small toolbar (react, copy) shown on hover and keyboard
// focus, and always on touch screens. Reactions open in a popover anchored
// to the toolbar's button. Explicit recipients are always shown; an audience
// phrase is left out only where a typed @agent already says the same.
//
// The composer is one raised box: recipients, the growing field, and a bar
// with the tools, "Ask agent"
// (explicitly asks the agent) and the plain send at the bottom right. Send
// never asks the agent by itself; who a message reaches is the store's and
// the server's rule (explicit recipients decide).

import { Clipboard, announce, cleanup, computed, effect, localeSettings, signal, untracked } from "nsh";
import type { CaretEvent, InputRef, JSX, ViewRef } from "nsh";
import {
  activeEvents,
  activeLoaded,
  activeReactions,
  activeSpace,
  actionError,
  composerScope,
  draft,
  isReactable,
  me,
  members,
  mentions,
  notice,
  mainPlace,
  REACTION_EMOJI,
  reactionPending,
  retryNow,
  send,
  sending,
  setDraft,
  status,
  toggleReaction,
} from "../store";
import type { RoomEvent } from "../store";
import { choiceForTurn, ComposerModelControls, knownNoSystemOne } from "./composerModels";
import { composerAddress } from "./composerAddress";
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
  DaySeparator,
  DraftSuggestion,
  EmojiChoice,
  EmojiRow,
  EmptyConversation,
  FeedList,
  HeaderNote,
  Label,
  MessageFileCard,
  RemovedFileCard,
  MessageFiles,
  MessageFrame,
  MessageHeader,
  MessageToolbar,
  ReactionChip,
  RecipientTag,
  Row,
  SendButton,
  StagedFileChip,
  Timestamp,
  ToolbarIconButton,
} from "../ui";
import type { ChatGlyphName, Tone } from "../ui";
import { audienceText, kindLabel, mentionsMe, mentionText, reactionSummary, toneForEvent } from "./eventKinds";
import { ActivityGroupRow, FailureBody, FoldableText } from "./activity/ActivityRow";
import { failureText } from "./activity/failure";
import { disclosureKey, disclosureScope } from "./activity/disclosure";
import { DEFAULT_AGENT_NAME, groupTranscript } from "./activity/group";
import { clockFor } from "./chatTime";
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

/** The active place's transcript, with each agent turn folded into one group. */
const transcriptItems = computed(() => groupTranscript(activeEvents()));
/** The user's zone and hour cycle; follows the system's settings as they change. */
const clock = computed(() => clockFor(localeSettings()));
const rows = computed(() => transcriptRows(transcriptItems(), activeEvents(), clock()));

// ------------------------------------------------------------------ the composer's field

let composerInput: InputRef | null = null;

/** Puts the keyboard focus in the composer, the caret after its text. */
export function focusComposer() {
  const input = composerInput;
  if (!input) return;
  input.focus();
  input.setSelection(draft().length);
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

export function Transcript(props: { emptyText: string; emptyTitle?: string; ideas?: DraftIdea[] }) {
  const chat = () => mainPlace() === "chat";
  const ideas = () => props.ideas ?? [];
  const glyph = (): ChatGlyphName => (chat() ? "chat" : "hash");
  return (
    <FeedList
      testID="transcript"
      each={rows()}
      keyOf={(row) => row.key}
      empty={
        <If
          when={activeLoaded()}
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
      {(row) => <TranscriptRowView row={row} />}
    </FeedList>
  );
}

type Of<T extends TranscriptRow["type"]> = Extract<TranscriptRow, { type: T }>;

/**
 * Each agent turn (tool calls, tool output, provider-shared summaries) shows as
 * one collapsed group; people's messages, the agent's replies, errors and
 * status stay in full.
 */
function TranscriptRowView(props: { row: () => TranscriptRow }) {
  const row = props.row;
  const as = <T extends TranscriptRow["type"]>(type: T) => (row().type === type ? (row() as Of<T>) : null);
  return (
    <If
      when={as("day")}
      else={
        <If when={as("group")} else={<MessageRow row={() => row() as Of<"event">} />}>
          {(g) => (
            <MessageFrame gutter={<StampGutter stamp={g().stamp} id={g().key} />} continued>
              <ActivityGroupRow
                group={g().group}
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

/** "@you", "@<their display name>" or "@<id>" when no profile is known; agent mentions as eventKinds says. */
function recipientText(m: NonNullable<RoomEvent["mentions"]>[number], myId: string | null): string {
  if (m.kind === "agent" && m.agentId) {
    const a = agentById(activeSpace()?.id, m.agentId);
    if (m.addressedBy === "classifier") return `${a?.name ?? "agent"} · detected by System One`;
    if (m.addressedBy === "schedule") return `${a?.name ?? "agent"} · scheduled prompt`;
    return a ? `@${a.handle}` : "@agent";
  }
  if (m.kind === "user" && m.id !== myId) {
    const name = profileName(m.id);
    if (name) return `@${name}`;
  }
  return mentionText(m, myId);
}

/** What a screen reader hears for a recipient: the display name and the stable id behind it. */
function recipientDescription(m: NonNullable<RoomEvent["mentions"]>[number], myId: string | null): string {
  if (m.kind === "agent" && m.agentId) {
    const a = agentById(activeSpace()?.id, m.agentId);
    return `Addressed to ${a ? `${a.name} (@${a.handle})` : "an agent"}${m.addressedBy ? ` (${recipientText(m, myId)})` : ""}`;
  }
  if (m.kind === "agent") return `Addressed to the agent${m.addressedBy ? ` (${mentionText(m, myId)})` : ""}`;
  if (m.id === myId) return "Addressed to you";
  const name = profileName(m.id);
  return name && name !== m.id ? `Addressed to ${name} (@${m.id})` : `Addressed to @${m.id}`;
}

function MessageRow(props: { row: () => Of<"event"> }) {
  const event = () => props.row().event;
  const tone = () => toneForEvent(event().kind, event().actor);
  const author = () => authorOf(event(), me());
  const forMe = () => mentionsMe(event().mentions, me());
  const audience = () => headerAudience(event().audience, event().mentions);
  const hasRecipients = () => audience().agentBadge || audience().peopleNote || (event().mentions?.length ?? 0) > 0;
  const continued = () => props.row().continued;
  /**
   * A follow-up's recipients: in a slim line level with the time in the
   * gutter, and left out when the text already starts by naming them all.
   */
  const followUpRecipients = () =>
    continued() && hasRecipients() && !(textNamesRecipients(event().text, event().mentions) && !audience().agentBadge && !audience().peopleNote);
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
  const reactable = () => isReactable(event());
  const copyable = () => event().text.trim().length > 0;
  const palette = signal(false);
  const copied = signal(false);
  let reactButton: ViewRef | null = null;
  const closePalette = () => {
    palette.set(false);
    reactButton?.focus();
  };
  const copy = () => {
    void Clipboard.writeText(event().text).then(
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
      testID={`event:${event().id}`}
      highlight={forMe()}
      continued={continued()}
      selectable
      toolbar={
        <If when={reactable() || copyable()}>
          <MessageToolbar open={palette()} testID={`toolbar:${event().id}`}>
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
            <If when={copyable()}>
              <ToolbarIconButton
                testID={`copy:${event().id}`}
                icon={copied() ? "check" : "copy"}
                accessibilityLabel={copied() ? "Copied" : "Copy the message text"}
                tooltip={copied() ? "Copied" : "Copy text"}
                onPress={copy}
              />
            </If>
          </MessageToolbar>
          <If when={palette()}>
            <ReactionPopover eventId={event().id} anchor={reactButton} onClose={closePalette} />
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
                label={recipientText(m(), me())}
                accessibilityLabel={recipientDescription(m(), me())}
              />
            )}
          </Each>
        </MessageHeader>
      </If>
      <If
        when={failure()}
        else={
          <If when={event().text.trim().length > 0}>
            <FoldableText text={event().text} mono={tone() === "runner"} stateKey={disclosureKey(disclosureScope(), event().id, "all")} />
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
      <If when={reactable()}>
        <Reactions eventId={event().id} />
      </If>
    </MessageFrame>
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
function ReactionPopover(props: { eventId: number; anchor: ViewRef | null; onClose: () => void }) {
  const byEmoji = () => activeReactions()[props.eventId] ?? {};
  const mine = (emoji: string) => (byEmoji()[emoji] ?? []).includes(`user:${me()}`);
  const pick = (emoji: string) => {
    props.onClose();
    void toggleReaction(props.eventId, emoji);
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
function Reactions(props: { eventId: number }) {
  const byEmoji = () => activeReactions()[props.eventId] ?? {};
  const emojis = () => Object.keys(byEmoji()).filter((e) => (byEmoji()[e]?.length ?? 0) > 0);
  const pending = (emoji: string) => reactionPending() === `${props.eventId}|${emoji}`;
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
                onPress={() => void toggleReaction(props.eventId, emoji())}
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
  /** The field took or lost the keyboard focus. */
  onFocusChange?: (focused: boolean) => void;
  /** The agent Ask agent asks (a Space with several agents); none: the default (or the classifier's pick). */
  askAgentId?: string;
  /** Ask agent's label when it names the chosen agent ("Ask Coder"). */
  askLabel?: string;
  /** The chosen agent's picture on Ask agent. */
  askPicture?: JSX.Element;
  /** The agent chooser joined to Ask agent. */
  askChooser?: JSX.Element;
}) {
  const hasFiles = () => stagedFor(composerScope()).length > 0;
  const ready = () => !props.disabled && status().phase !== "signedOut" && !sending();
  const canSend = () => ready() && (draft().trim().length > 0 || hasFiles());
  // Asking the agent needs a typed question: files alone are saved, not sent to it.
  const canAsk = () => ready() && draft().trim().length > 0;
  const personal = () => mainPlace() === "chat";
  const addressed = computed(() => composerAddress(draft(), personal() ? [] : (members() ?? []).map(m => m.userId)));
  const toAgent = () => addressed().agent || (!personal() && mentions().some(m => m.kind === "agent"));
  // Decide once when a new draft starts (including a suggestion). A catalog
  // that finishes loading while somebody writes must never promote their Send.
  const askDraft = signal(false);
  let started = false;
  effect(() => {
    const filled = personal() && !!draft().trim();
    if (filled !== started) {
      started = filled;
      askDraft.set(filled && untracked(knownNoSystemOne));
    }
  });
  const primaryAsk = () => personal() && askDraft() && !!draft().trim() && (!addressed().people || toAgent());
  const post = (ask: boolean) => {
    if (!(ask ? canAsk() : canSend())) return;
    if (draft().trim() && (ask || toAgent())) {
      const { error, choice } = choiceForTurn();
      if (error) { actionError.set(error); return; }
      void send(ask, choice, ask ? props.askAgentId : undefined);
    } else void send(false);
  };
  const submit = () => {
    if (props.onSubmitIntercept?.()) return;
    post(primaryAsk());
  };
  return (
    <Column padX={4} padY={3} gap={2}>
      <ComposerFeedback />
      <ComposerBox testID="composer-box">
        {props.above}
        <StagedFiles />
        <ComposerField
          testID="composer"
          ref={(node) => (composerInput = node)}
          accessibilityLabel={mainPlace() === "chat" ? "Message your Chat" : "Message"}
          value={draft()}
          onChange={setDraft}
          onSubmit={submit}
          onFocusChange={(f) => props.onFocusChange?.(f)}
          onCaretMove={props.onCaretMove}
          placeholder={props.placeholder}
          disabled={props.disabled}
        />
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
              <ComposerModelControls />
            </>
          }
          trailing={
            <>
              <AskAgentButton
                testID="ask-agent"
                primary={primaryAsk()}
                label={`${props.askLabel ?? "Ask agent"}${primaryAsk() ? " ↵" : ""}`}
                picture={props.askPicture}
                chooser={props.askChooser}
                busyLabel="Asking…"
                accessibilityLabel={`Send and ask the agent to answer. ${primaryAsk() ? "Return asks the agent. " : ""}${props.agentHint}${hasFiles() ? " Attached files are saved to the Library, not sent to the agent." : ""}`}
                tooltip={primaryAsk() ? "Ask agent (Return)" : props.agentHint}
                busy={sending() === "agent"}
                disabled={!canAsk()}
                onPress={() => post(true)}
              />
              <SendButton
                testID="send"
                note={primaryAsk() && !toAgent()}
                accessibilityLabel={primaryAsk() && !toAgent() ? "Save note without asking the agent" : hasFiles() ? "Send with files" : "Send"}
                tooltip={primaryAsk() ? (toAgent() ? "Send to mentioned recipients" : "Save note without asking the agent") : "Send (Return)"}
                busy={sending() === "message"}
                disabled={!canSend()}
                onPress={() => post(false)}
              />
            </>
          }
        />
      </ComposerBox>
    </Column>
  );
}

export { kindLabel };
