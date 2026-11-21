// The personal Chat (outside every Space) and the human's meta controls: the
// agent runs in the user's Spaces, and sending a task to an explicitly chosen
// Space and room. The Chat's own agent can't see Spaces; these controls are
// the user's, and the server checks membership for each.

import { signal } from "nsh";
import {
  chatUnavailable,
  createSpace,
  creatingSpace,
  dispatchDraft,
  dispatchError,
  dispatching,
  dispatchNotice,
  dispatchTarget,
  followUp,
  isActiveRun,
  me,
  openRun,
  roomsOf,
  runBusy,
  runs,
  runScope,
  runsError,
  runsLoaded,
  sendDispatch,
  setDispatchRoom,
  setDispatchSpace,
  spaces,
  stopRun,
} from "../store";
import type { Run } from "../store";
import { Action, Badge, Banner, Chip, Column, Field, Label, Row, ScrollArea, Spacer, Surface } from "../ui";
import type { Tone } from "../ui";
import { Composer, ConnectionBanner, Transcript } from "./Conversation";
import { roomTree } from "./Sidebar";

export function ChatView() {
  return (
    <Surface variant="app" testID="chat-view">
      <Surface variant="panel" grow>
        <Surface variant="toolbar" padX={4} padY={3} gap={3}>
          <Column grow gap={1}>
            <Label variant="heading" testID="room-title">
              Chat
            </Label>
            <Label variant="caption" color="muted" lines={2}>
              Your personal thread with the agent. Only you see it, and it can't see your Spaces. Their runs are on the right.
            </Label>
          </Column>
        </Surface>
        <ConnectionBanner />
        <If when={spaces().length === 0}>
          <Onboarding />
        </If>
        <If
          when={chatUnavailable()}
          else={<Transcript emptyText="Nothing here yet. Ask your agent something, or send a task to a Space on the right." />}
        >
          {(message) => (
            <Column grow pad={6} align="center" justify="center">
              <Label color="muted">{message()}</Label>
            </Column>
          )}
        </If>
        <Composer placeholder="Message your Chat — Return to send" agentHint="Ask agent answers here. This agent has no runner, workspace or thread tools." disabled={!!chatUnavailable()} />
      </Surface>
      <RunsAside />
    </Surface>
  );
}

function Onboarding() {
  const name = signal("");
  return (
    <Banner tone="neutral" testID="onboarding">
      <Column grow gap={2}>
        <Label variant="strong">You're not in any Space yet</Label>
        <Label variant="caption" color="muted">
          A Space has its own rooms (starting with a lobby), members and optional runner. Create one, or share your user ID with a Space owner so they can add you:
        </Label>
        <Label variant="mono" testID="onboarding-id">
          {me() ?? ""}
        </Label>
        <Row gap={2} align="center">
          <Field testID="onboarding-space-name" accessibilityLabel="New Space name" value={name()} onChange={name.set} placeholder="Name your first Space" grow maxLength={80} onSubmit={() => void createSpace(name())} />
          <Action testID="onboarding-create-space" label="Create Space" busyLabel="Creating…" busy={creatingSpace()} variant="primary" onPress={() => void createSpace(name())} />
        </Row>
      </Column>
    </Banner>
  );
}

const statusTone = (run: Run): Tone => {
  if (isActiveRun(run)) return "runner";
  if (/(fail|error)/i.test(run.status)) return "danger";
  if (/(stop|cancel)/i.test(run.status)) return "warning";
  if (/(done|complete|succe|finish)/i.test(run.status)) return "success";
  return "neutral";
};

function RunsAside() {
  return (
    <Surface variant="aside" testID="runs-aside">
      <Column pad={4} gap={1}>
        <Label variant="heading">Agent runs</Label>
        <Label variant="caption" color="muted">
          Active and recent, in Spaces you belong to.
        </Label>
      </Column>
      <ScrollArea padX={4} gap={2} testID="runs-list">
        <If when={runsError()}>
          {(message) => (
            <Label variant="caption" color="danger" testID="runs-error">
              {message()}
            </Label>
          )}
        </If>
        <Each of={runs()} key={(run) => run.id}>
          {(run) => <RunCard run={run} />}
        </Each>
        <If when={runsLoaded() && runs().length === 0 && !runsError()}>
          <Label variant="caption" color="muted">
            No runs yet.
          </Label>
        </If>
      </ScrollArea>
      <Dispatch />
    </Surface>
  );
}

function RunCard(props: { run: () => Run }) {
  const scope = () => runScope(props.run());
  return (
    <Surface variant="card" pad={3} gap={2} testID={`run:${props.run().id}`}>
      <Row align="center" gap={2}>
        <Badge tone={statusTone(props.run())} label={props.run().status} />
        <Column grow>
          <Label variant="caption" color="muted" lines={1}>
            {`${scope().space} › #${scope().room}`}
          </Label>
        </Column>
      </Row>
      <Label variant="body" lines={3}>
        {props.run().task}
      </Label>
      <Label variant="caption" color="muted">
        {`started by ${props.run().actor === me() ? "you" : props.run().actor}`}
      </Label>
      <Row gap={2} wrap>
        <Action testID={`run-open:${props.run().id}`} label="Open" variant="ghost" onPress={() => void openRun(props.run())} />
        <Action testID={`run-follow:${props.run().id}`} label="Follow up" variant="ghost" onPress={() => void followUp(props.run())} />
        <If when={isActiveRun(props.run())}>
          <Action
            testID={`run-stop:${props.run().id}`}
            label="Stop"
            busyLabel="Stopping…"
            busy={runBusy() === props.run().id}
            variant="danger"
            onPress={() => void stopRun(props.run())}
          />
        </If>
      </Row>
    </Surface>
  );
}

function Dispatch() {
  const target = () => dispatchTarget();
  const targetSpace = () => spaces().find((s) => s.id === target()?.spaceId) ?? null;
  const targetRoom = () => roomsOf(target()?.spaceId ?? "").find((r) => r.id === target()?.roomId) ?? null;
  const ready = () => !!targetSpace() && !!targetRoom() && dispatchDraft().trim().length > 0 && !dispatching();
  return (
    <Surface variant="footer" pad={4} gap={2} testID="dispatch">
      <Label variant="strong">Send a task to a Space's agent</Label>
      <If
        when={spaces().length > 0}
        else={
          <Label variant="caption" color="muted">
            Join or create a Space first.
          </Label>
        }
      >
        <Column gap={2}>
          <Label variant="caption" color="muted">
            Space
          </Label>
          <Row gap={1} wrap>
            <Each of={spaces()} key={(s) => s.id}>
              {(s) => <Chip testID={`dispatch-space:${s().id}`} label={s().name} selected={target()?.spaceId === s().id} onPress={() => void setDispatchSpace(s().id)} />}
            </Each>
          </Row>
          <If when={targetSpace()}>
            <Label variant="caption" color="muted">
              Room or thread
            </Label>
            <Row gap={1} wrap>
              <Each of={roomTree(roomsOf(target()?.spaceId ?? ""))} key={(row) => row.room.id}>
                {(row) => (
                  <Chip
                    testID={`dispatch-room:${row().room.id}`}
                    label={`${row().depth > 1 ? "↳ " : "#"}${row().room.name}`}
                    selected={target()?.roomId === row().room.id}
                    onPress={() => setDispatchRoom(row().room.id)}
                  />
                )}
              </Each>
            </Row>
          </If>
          <Field
            testID="dispatch-text"
            accessibilityLabel="Task for the agent"
            value={dispatchDraft()}
            onChange={dispatchDraft.set}
            placeholder="Describe the task or follow-up"
            multiline
            keepFocusOnSubmit
          />
          <Row align="center" gap={2}>
            <Column grow>
              <Label variant="caption" color={targetRoom() ? "default" : "muted"} lines={2} testID="dispatch-recipient">
                {targetSpace() && targetRoom() ? `To: agent in ${targetSpace()!.name} › #${targetRoom()!.name}` : "Choose a Space and a room."}
              </Label>
            </Column>
            <Spacer />
            <Action
              testID="dispatch-send"
              label="Send to agent"
              busyLabel="Sending…"
              busy={dispatching()}
              disabled={!ready()}
              variant="primary"
              onPress={() => void sendDispatch()}
            />
          </Row>
          <If when={dispatchError()}>
            {(message) => (
              <Label variant="caption" color="danger" testID="dispatch-error">
                {message()}
              </Label>
            )}
          </If>
          <If when={dispatchNotice()}>
            {(message) => (
              <Label variant="caption" color="muted" testID="dispatch-notice">
                {message()}
              </Label>
            )}
          </If>
        </Column>
      </If>
    </Surface>
  );
}
