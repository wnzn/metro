// Pieces shared by the Chat and a Space's rooms: the connection banner, the
// transcript and the composer. They read the active place from the store.
//
// The transcript draws each entry with its author (picture or initials, the
// name from their profile, "you" for the signed-in user, the agent in its own
// tone) and the time the server recorded, in the device's zone. A date line
// starts each day; consecutive messages by one author share a header. An
// entry the server has no time for shows none.

import { computed, localeSettings, signal } from "nsh";
import type { JSX } from "nsh";
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
  AuthorName,
  AuthorTag,
  Avatar,
  Badge,
  Banner,
  Column,
  DaySeparator,
  FeedList,
  Field,
  Label,
  MessageFrame,
  MessageHeader,
  ReactionChip,
  Row,
  Surface,
  Timestamp,
} from "../ui";
import type { Tone } from "../ui";
import { audienceText, kindLabel, mentionsMe, mentionText, reactionSummary, toneForEvent } from "./eventKinds";
import { ActivityGroupRow, FoldableText } from "./activity/ActivityRow";
import { disclosureKey, disclosureScope } from "./activity/disclosure";
import { DEFAULT_AGENT_NAME, groupTranscript } from "./activity/group";
import { clockFor } from "./chatTime";
import type { Stamp } from "./chatTime";
import { initialsOf } from "./profile/identity";
import { PersonAvatar } from "./profile/PersonAvatar";
import { profileName } from "./profile/profileState";
import { transcriptRows } from "./transcriptRows";
import type { TranscriptRow } from "./transcriptRows";

/** The active place's transcript, with each agent turn folded into one group. */
const transcriptItems = computed(() => groupTranscript(activeEvents()));
/** The user's zone and hour cycle; follows the system's settings as they change. */
const clock = computed(() => clockFor(localeSettings()));
const rows = computed(() => transcriptRows(transcriptItems(), activeEvents(), clock()));

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

export function Transcript(props: { emptyText: string }) {
  return (
    <FeedList
      testID="transcript"
      each={rows()}
      keyOf={(row) => row.key}
      empty={
        <Column grow align="center" justify="center" pad={6}>
          <Label color="muted">{activeLoaded() ? props.emptyText : "Loading…"}</Label>
        </Column>
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

/** The agent's tone, or a server actor's (runner, system, errors). */
const serverTone = (a: Author): Tone => (a.kind === "agent" ? "agent" : a.kind === "server" ? a.tone : "neutral");

/** "@you", "@<their display name>" or "@<id>" when no profile is known; agent mentions as eventKinds says. */
function recipientText(m: NonNullable<RoomEvent["mentions"]>[number], myId: string | null): string {
  if (m.kind === "user" && m.id !== myId) {
    const name = profileName(m.id);
    if (name) return `@${name}`;
  }
  return mentionText(m, myId);
}

function MessageRow(props: { row: () => Of<"event"> }) {
  const event = () => props.row().event;
  const tone = () => toneForEvent(event().kind, event().actor);
  const author = () => authorOf(event(), me());
  const forMe = () => mentionsMe(event().mentions, me());
  const audience = () => audienceText(event().audience);
  const hasRecipients = () => !!audience() || (event().mentions?.length ?? 0) > 0;
  const continued = () => props.row().continued;
  const person = () => {
    const a = author();
    return a.kind === "person" ? a : null;
  };
  const palette = signal(false);
  return (
    <MessageFrame
      testID={`event:${event().id}`}
      highlight={forMe()}
      continued={continued()}
      selectable
      trailing={isReactable(event()) ? <AddReaction eventId={event().id} palette={palette} /> : undefined}
      gutter={
        <If when={!continued()} else={<StampGutter stamp={props.row().stamp} id={event().id} />}>
          <If
            when={person()}
            else={<Avatar initials={initialsOf(author().name)} tone={serverTone(author())} />}
          >
            {(p) => <PersonAvatar userId={p().userId} name={p().name} testID={`avatar:${event().id}`} />}
          </If>
        </If>
      }
    >
      <If when={!continued() || hasRecipients()}>
        <MessageHeader testID={`header:${event().id}`}>
          <If when={!continued()}>
            <AuthorName testID={`author:${event().id}`} name={author().name} agent={author().kind === "agent"} />
            <If when={person()?.mine}>
              <AuthorTag testID={`you:${event().id}`} label="you" tone="neutral" />
            </If>
            <If when={author().kind === "agent"}>
              <AuthorTag label="agent" tone="agent" />
            </If>
            <If when={author().kind === "server" ? (author() as Extract<Author, { kind: "server" }>) : null}>
              {(a) => <Badge tone={a().tone} label={a().label} testID={`kind:${event().id}`} />}
            </If>
            <If when={props.row().stamp}>
              {(s) => <Timestamp text={s().short} full={s().full} testID={`time:${event().id}`} />}
            </If>
          </If>
          <If when={audience()}>
            {(text) => (
              <Label variant="caption" color="muted">
                {text()}
              </Label>
            )}
          </If>
          <Each of={event().mentions ?? []} key={(m) => (m.kind === "agent" ? "agent" : `user:${m.id}`)}>
            {(m) => (
              <Badge
                testID={`recipient:${event().id}:${m().kind === "agent" ? "agent" : (m() as { id: string }).id}`}
                tone={m().kind === "agent" ? "agent" : mentionText(m(), me()) === "@you" ? "warning" : "neutral"}
                label={recipientText(m(), me())}
              />
            )}
          </Each>
        </MessageHeader>
      </If>
      <FoldableText text={event().text} mono={tone() === "runner"} stateKey={disclosureKey(disclosureScope(), event().id, "all")} />
      <If when={isReactable(event())}>
        <Reactions eventId={event().id} palette={palette} />
      </If>
    </MessageFrame>
  );
}

/** The "+" that opens the reaction palette, at the message's top right. */
function AddReaction(props: { eventId: number; palette: { (): boolean; set(v: boolean): void } }) {
  return (
    <ReactionChip
      testID={`react:${props.eventId}`}
      emoji="+"
      count={0}
      mine={false}
      agent={false}
      accessibilityLabel={props.palette() ? "Close the reaction choices" : "Add a reaction"}
      onPress={() => props.palette.set(!props.palette())}
    />
  );
}

/** The reactions so far, and the palette while it is open; nothing when there are neither. */
function Reactions(props: { eventId: number; palette: { (): boolean; set(v: boolean): void } }) {
  const byEmoji = () => activeReactions()[props.eventId] ?? {};
  const emojis = () => Object.keys(byEmoji()).filter((e) => (byEmoji()[e]?.length ?? 0) > 0);
  const pending = (emoji: string) => reactionPending() === `${props.eventId}|${emoji}`;
  const pick = (emoji: string) => {
    props.palette.set(false);
    void toggleReaction(props.eventId, emoji);
  };
  return (
    <If when={emojis().length > 0 || props.palette()}>
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
                onPress={() => pick(emoji())}
              />
            );
          }}
        </Each>
        <If when={props.palette()}>
          <Row gap={1} testID={`palette:${props.eventId}`}>
            <Each of={[...REACTION_EMOJI]} key={(e) => e}>
              {(emoji) => (
                <ReactionChip
                  testID={`pick-reaction:${props.eventId}:${emoji()}`}
                  emoji={emoji()}
                  count={0}
                  mine={(byEmoji()[emoji()] ?? []).includes(`user:${me()}`)}
                  agent={false}
                  accessibilityLabel={`React with ${emoji()}`}
                  onPress={() => pick(emoji())}
                />
              )}
            </Each>
          </Row>
        </If>
      </Row>
    </If>
  );
}

export function Composer(props: {
  placeholder: string;
  agentHint: string;
  disabled?: boolean;
  /** Shown above the field (mention picker and chips in rooms). */
  above?: JSX.Element;
  /** Extra controls before Ask agent / Send. */
  tools?: JSX.Element;
}) {
  const canSend = () => !props.disabled && status().phase !== "signedOut" && draft().trim().length > 0 && !sending();
  return (
    <Surface variant="footer" padX={4} padY={3} gap={2}>
      <If when={actionError()}>
        {(message) => (
          <Banner tone="danger" testID="action-error">
            <Label color="danger">{message()}</Label>
          </Banner>
        )}
      </If>
      <If when={notice()}>
        {(message) => (
          <Label variant="caption" color="muted" testID="notice">
            {message()}
          </Label>
        )}
      </If>
      {props.above}
      <Field
        testID="composer"
        accessibilityLabel={mainPlace() === "chat" ? "Message your Chat" : "Message"}
        value={draft()}
        onChange={setDraft}
        placeholder={props.placeholder}
        multiline
        keepFocusOnSubmit
        disabled={props.disabled}
        onSubmit={() => void send(false)}
      />
      <Row align="center" gap={2}>
        <Column grow>
          <Label variant="caption" color="muted" lines={2}>
            {props.agentHint}
          </Label>
        </Column>
        {props.tools}
        <Action
          testID="ask-agent"
          label="Ask agent"
          busyLabel="Asking…"
          busy={sending() === "agent"}
          disabled={!canSend()}
          onPress={() => void send(true)}
        />
        <Action
          testID="send"
          label="Send"
          busyLabel="Sending…"
          busy={sending() === "message"}
          disabled={!canSend()}
          variant="primary"
          onPress={() => void send(false)}
        />
      </Row>
    </Surface>
  );
}

export { kindLabel };
