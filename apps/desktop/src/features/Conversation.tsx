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
// with the tools, a hint shown while the field has the focus, "Ask agent"
// (explicitly asks the agent) and the plain send at the bottom right. Send
// never asks the agent by itself; who a message reaches is the store's and
// the server's rule (explicit recipients decide).

import { Clipboard, announce, computed, localeSettings, signal } from "nsh";
import type { CaretEvent, InputRef, JSX, ViewRef } from "nsh";
import {
  activeEvents,
  activeLoaded,
  activeReactions,
  actionError,
  draft,
  isReactable,
  me,
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
import {
  Action,
  AnchoredPopover,
  AskAgentButton,
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
  DraftSuggestion,
  EmojiChoice,
  EmojiRow,
  EmptyConversation,
  FeedList,
  HeaderNote,
  Label,
  MessageFrame,
  MessageHeader,
  MessageToolbar,
  ReactionChip,
  RecipientTag,
  Row,
  SendButton,
  Timestamp,
  ToolbarIconButton,
} from "../ui";
import type { ChatGlyphName, Tone } from "../ui";
import { audienceText, kindLabel, mentionsMe, mentionText, reactionSummary, toneForEvent } from "./eventKinds";
import { ActivityGroupRow, FoldableText } from "./activity/ActivityRow";
import { disclosureKey, disclosureScope } from "./activity/disclosure";
import { DEFAULT_AGENT_NAME, groupTranscript } from "./activity/group";
import { clockFor } from "./chatTime";
import type { Stamp } from "./chatTime";
import { PersonAvatar } from "./profile/PersonAvatar";
import { profileName } from "./profile/profileState";
import { headerAudience, transcriptRows } from "./transcriptRows";
import type { TranscriptRow } from "./transcriptRows";

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
              <ActivityGroupRow group={g().group} />
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

/** Who wrote an event, as the transcript shows them. */
type Author =
  | { kind: "person"; userId: string | null; name: string; mine: boolean }
  | { kind: "agent"; name: string }
  | { kind: "server"; name: string; tone: Tone; label: string };

function authorOf(e: RoomEvent, myId: string | null): Author {
  const kind = e.kind.trim().toLowerCase();
  if (kind === "message") {
    // Only the stable id says who it is; a display name never does.
    const userId = e.actorId ?? null;
    return { kind: "person", userId, name: profileName(userId) ?? (e.actor.trim() || userId || "unknown"), mine: !!userId && userId === myId };
  }
  if (kind === "agent") return { kind: "agent", name: e.activity?.name || DEFAULT_AGENT_NAME };
  return { kind: "server", name: e.actor.trim() || "system", tone: toneForEvent(e.kind, e.actor), label: kindLabel(e.kind) };
}

/** "@you", "@<their display name>" or "@<id>" when no profile is known; agent mentions as eventKinds says. */
function recipientText(m: NonNullable<RoomEvent["mentions"]>[number], myId: string | null): string {
  if (m.kind === "user" && m.id !== myId) {
    const name = profileName(m.id);
    if (name) return `@${name}`;
  }
  return mentionText(m, myId);
}

/** What a screen reader hears for a recipient: the display name and the stable id behind it. */
function recipientDescription(m: NonNullable<RoomEvent["mentions"]>[number], myId: string | null): string {
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
  const audience = () => audienceText(headerAudience(event().audience, event().mentions));
  const hasRecipients = () => !!audience() || (event().mentions?.length ?? 0) > 0;
  const continued = () => props.row().continued;
  const person = () => {
    const a = author();
    return a.kind === "person" ? a : null;
  };
  const server = () => {
    const a = author();
    return a.kind === "server" ? a : null;
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
              <AuthorGlyph
                testID={`avatar:${event().id}`}
                kind={author().kind === "agent" ? "agent" : server()?.tone === "runner" ? "runner" : "system"}
                tone={author().kind === "agent" ? "agent" : server()?.tone ?? "neutral"}
              />
            }
          >
            {(p) => <PersonAvatar userId={p().userId} name={p().name} testID={`avatar:${event().id}`} />}
          </If>
        </If>
      }
    >
      <If when={!continued() || hasRecipients()}>
        <MessageHeader testID={`header:${event().id}`}>
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
          <If when={audience()}>{(text) => <HeaderNote text={text()} testID={`audience:${event().id}`} />}</If>
          <Each of={event().mentions ?? []} key={(m) => (m.kind === "agent" ? "agent" : `user:${m.id}`)}>
            {(m) => (
              <RecipientTag
                testID={`recipient:${event().id}:${m().kind === "agent" ? "agent" : (m() as { id: string }).id}`}
                tone={m().kind === "agent" ? "agent" : "neutral"}
                self={m().kind === "user" && (m() as { id: string }).id === me()}
                label={recipientText(m(), me())}
                accessibilityLabel={recipientDescription(m(), me())}
              />
            )}
          </Each>
        </MessageHeader>
      </If>
      <FoldableText text={event().text} mono={tone() === "runner"} stateKey={disclosureKey(disclosureScope(), event().id, "all")} />
      <If when={reactable()}>
        <Reactions eventId={event().id} />
      </If>
    </MessageFrame>
  );
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

export function Composer(props: {
  placeholder: string;
  /** One line of help, shown while the field has the focus. */
  agentHint: string;
  disabled?: boolean;
  /** Inside the box, above the field (the recipients of a room message), and its popovers. */
  above?: JSX.Element;
  /** Tools at the bottom left of the box (the @ button). */
  tools?: JSX.Element;
  /** Return while a suggestion list is open: return true when it picked one (nothing is sent). */
  onSubmitIntercept?: () => boolean;
  /** The caret moved in the field (its box in the window anchors mention suggestions). */
  onCaretMove?: (e: CaretEvent) => void;
}) {
  const focused = signal(false);
  const canSend = () => !props.disabled && status().phase !== "signedOut" && draft().trim().length > 0 && !sending();
  const submit = () => {
    if (props.onSubmitIntercept?.()) return;
    void send(false);
  };
  return (
    <Column padX={4} padY={3} gap={2}>
      <ComposerFeedback />
      <ComposerBox testID="composer-box">
        {props.above}
        <ComposerField
          testID="composer"
          ref={(node) => (composerInput = node)}
          accessibilityLabel={mainPlace() === "chat" ? "Message your Chat" : "Message"}
          value={draft()}
          onChange={setDraft}
          onSubmit={submit}
          onFocusChange={(f) => focused.set(f)}
          onCaretMove={props.onCaretMove}
          placeholder={props.placeholder}
          disabled={props.disabled}
        />
        <ComposerBar
          leading={
            <>
              {props.tools}
              <If when={focused()}>
                <ComposerHint testID="composer-hint" text={props.agentHint} />
              </If>
            </>
          }
          trailing={
            <>
              <AskAgentButton
                testID="ask-agent"
                label="Ask agent"
                busyLabel="Asking…"
                accessibilityLabel="Send and ask the agent to answer"
                busy={sending() === "agent"}
                disabled={!canSend()}
                onPress={() => void send(true)}
              />
              <SendButton
                testID="send"
                accessibilityLabel="Send"
                tooltip="Send (Return)"
                busy={sending() === "message"}
                disabled={!canSend()}
                onPress={() => void send(false)}
              />
            </>
          }
        />
      </ComposerBox>
    </Column>
  );
}

export { kindLabel };
