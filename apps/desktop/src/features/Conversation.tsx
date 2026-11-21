// Pieces shared by the Chat and a Space's rooms: the connection banner, the
// transcript and the composer. They read the active place from the store.

import { signal } from "nsh";
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
  place,
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
import { Action, Badge, Banner, Column, FeedList, Field, Label, Rail, ReactionChip, Row, Surface } from "../ui";
import { audienceText, kindLabel, mentionsMe, mentionText, reactionSummary, toneForEvent } from "./eventKinds";

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
      each={activeEvents()}
      keyOf={(event) => String(event.id)}
      empty={
        <Column grow align="center" justify="center" pad={6}>
          <Label color="muted">{activeLoaded() ? props.emptyText : "Loading…"}</Label>
        </Column>
      }
    >
      {(event) => <EventRow event={event} />}
    </FeedList>
  );
}

function EventRow(props: { event: () => RoomEvent }) {
  const tone = () => toneForEvent(props.event().kind, props.event().actor);
  const forMe = () => mentionsMe(props.event().mentions, me());
  const audience = () => audienceText(props.event().audience);
  return (
    <Row gap={3} testID={`event:${props.event().id}`}>
      <Rail tone={forMe() ? "warning" : tone()} />
      <Column grow gap={1} padY={1} selectable>
        <Row align="center" gap={2} wrap>
          <Label variant="strong">{props.event().actor || "unknown"}</Label>
          <Badge tone={tone()} label={kindLabel(props.event().kind)} />
          <If when={audience()}>
            {(text) => (
              <Label variant="caption" color="muted">
                {text()}
              </Label>
            )}
          </If>
          <Each of={props.event().mentions ?? []} key={(m) => (m.kind === "agent" ? "agent" : `user:${m.id}`)}>
            {(m) => (
              <Badge
                testID={`recipient:${props.event().id}:${m().kind === "agent" ? "agent" : (m() as { id: string }).id}`}
                tone={m().kind === "agent" ? "agent" : mentionText(m(), me()) === "@you" ? "warning" : "neutral"}
                label={mentionText(m(), me())}
              />
            )}
          </Each>
        </Row>
        <EventText text={props.event().text} mono={tone() === "runner"} />
        <If when={isReactable(props.event())}>
          <Reactions eventId={props.event().id} />
        </If>
      </Column>
    </Row>
  );
}

const CLAMP_LINES = 6;

/** Long output (tool results, listings) starts folded so the conversation stays readable. */
function EventText(props: { text: string; mono: boolean }) {
  const expanded = signal(false);
  const lineCount = () => props.text.split("\n").length;
  const long = () => lineCount() > CLAMP_LINES + 2 || props.text.length > 1200;
  return (
    <Column gap={1}>
      <Label variant={props.mono ? "mono" : "body"} lines={long() && !expanded() ? CLAMP_LINES : undefined}>
        {props.text}
      </Label>
      <If when={long()}>
        <Row>
          <Action
            label={expanded() ? "Show less" : `Show all ${lineCount()} lines`}
            variant="ghost"
            onPress={() => expanded.set(!expanded())}
          />
        </Row>
      </If>
    </Column>
  );
}

function Reactions(props: { eventId: number }) {
  const palette = signal(false);
  const byEmoji = () => activeReactions()[props.eventId] ?? {};
  const emojis = () => Object.keys(byEmoji()).filter((e) => (byEmoji()[e]?.length ?? 0) > 0);
  const pending = (emoji: string) => reactionPending() === `${props.eventId}|${emoji}`;
  const pick = (emoji: string) => {
    palette.set(false);
    void toggleReaction(props.eventId, emoji);
  };
  return (
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
      <ReactionChip testID={`react:${props.eventId}`} emoji="+" count={0} mine={false} agent={false} accessibilityLabel="Add a reaction" onPress={() => palette.set(!palette())} />
      <If when={palette()}>
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
        accessibilityLabel={place() === "chat" ? "Message your Chat" : "Message"}
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
