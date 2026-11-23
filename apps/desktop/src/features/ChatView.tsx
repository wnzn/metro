// The personal Chat (outside every Space) and the human's meta controls: the
// agent runs in the user's Spaces, and sending a task to an explicitly chosen
// Space and room. The Chat's own agent can't see Spaces; these controls are
// the user's, and the server checks membership for each.
//
// An empty Chat offers a few suggested drafts. Pressing one only fills an
// empty message box: it never sends, and never replaces what the user typed.
// The task form stays folded behind "New task" until the user opens it (or
// presses Follow up), and it scrolls with the runs, so it never covers them.

import { signal } from "nsh";
import {
  activeEvents,
  activeLoaded,
  chatUnavailable,
  createSpace,
  creatingSpace,
  dispatchDraft,
  dispatchError,
  dispatching,
  dispatchNotice,
  dispatchTarget,
  draft,
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
  setDraft,
  spaces,
  stopRun,
} from "../store";
import type { Run } from "../store";
import { Action, Badge, Banner, Chip, Column, Field, Icon, IconButton, Label, Modal, Row, ScrollArea, Spacer, Surface } from "../ui";
import { EmptyState, sidePanelFits, SuggestionChip } from "../ui/activity";
import { runStatusLabel, runTitle, runTone } from "./activity/runs";
import { Composer, ConnectionBanner, Transcript } from "./Conversation";
import { roomTree } from "./Sidebar";

/** The task form is open (folded behind "New task" otherwise). */
const dispatchOpen = signal(false);
/** In a window too narrow for the side panel, the runs open in a dialog over the Chat. */
const runsDialogOpen = signal(false);

/** Drafts the empty Chat suggests. The personal agent has no tools, so none asks for any. */
export const CHAT_SUGGESTIONS: readonly { label: string; draft: string }[] = [
  { label: "Plan my day", draft: "Help me plan my day. Here's what I need to get done: " },
  { label: "Draft a status update", draft: "Help me write a short status update about: " },
  { label: "Explain something simply", draft: "Explain this in plain words: " },
];

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
              Only you see this chat, and its agent can't see your Spaces.
            </Label>
          </Column>
          <If when={!sidePanelFits()}>
            <Action
              compact
              testID="runs-toggle"
              label={runs().some(isActiveRun) ? `Runs · ${runs().filter(isActiveRun).length} active` : "Runs"}
              accessibilityLabel="Show agent runs"
              onPress={() => runsDialogOpen.set(true)}
            />
          </If>
        </Surface>
        <ConnectionBanner />
        <If when={spaces().length === 0}>
          <Onboarding />
        </If>
        <If
          when={chatUnavailable()}
          else={
            <If when={activeLoaded() && activeEvents().length === 0} else={<Transcript emptyText="No messages yet." />}>
              <ChatEmpty />
            </If>
          }
        >
          {(message) => (
            <Column grow pad={6} align="center" justify="center">
              <Label color="muted">{message()}</Label>
            </Column>
          )}
        </If>
        <Composer placeholder="Message your Chat" agentHint="No runner, workspace or thread tools here." disabled={!!chatUnavailable()} />
      </Surface>
      {/* Wide windows keep the runs beside the Chat; narrower ones open them
          in a dialog, so the Chat (and its draft) keeps its full width. */}
      <If when={sidePanelFits()}>
        <Surface variant="aside" testID="runs-aside">
          <RunsPanel />
        </Surface>
      </If>
      <Modal testID="runs-dialog" title="Agent runs" open={runsDialogOpen() && !sidePanelFits()} onClose={() => runsDialogOpen.set(false)}>
        <RunsPanel onClose={() => runsDialogOpen.set(false)} />
      </Modal>
    </Surface>
  );
}

/** An empty Chat: what it is for, and drafts that only fill an empty message box. */
function ChatEmpty() {
  const typed = () => draft().trim().length > 0;
  return (
    <EmptyState testID="chat-empty" title="Start a conversation" text="A suggestion only fills the message box.">
      <Row gap={2} wrap justify="center">
        <Each of={[...CHAT_SUGGESTIONS]} key={(s) => s.label}>
          {(s) => (
            <SuggestionChip
              testID={`chat-suggestion:${CHAT_SUGGESTIONS.indexOf(s())}`}
              label={s().label}
              disabled={typed()}
              accessibilityLabel={typed() ? `${s().label} (the message box already has text)` : `Start a draft: ${s().label}`}
              onPress={() => {
                // Never replaces what the user typed, and never sends.
                if (!typed()) setDraft(s().draft);
              }}
            />
          )}
        </Each>
      </Row>
    </EmptyState>
  );
}

function Onboarding() {
  const name = signal("");
  return (
    <Banner tone="neutral" testID="onboarding">
      <Column grow gap={2}>
        <Label variant="strong">You're not in any Space yet</Label>
        <Label variant="caption" color="muted">
          Create one, or ask a Space owner to add you.
        </Label>
        <Row gap={2} align="center">
          <Field testID="onboarding-space-name" accessibilityLabel="New Space name" value={name()} onChange={name.set} placeholder="Name your first Space" grow maxLength={80} onSubmit={() => void createSpace(name())} />
          <Action testID="onboarding-create-space" label="Create Space" busyLabel="Creating…" busy={creatingSpace()} variant="primary" onPress={() => void createSpace(name())} />
        </Row>
      </Column>
    </Banner>
  );
}

/** The runs list and task form: in the side panel, or in the dialog (with `onClose`). */
function RunsPanel(props: { onClose?: () => void }) {
  return (
    <Column grow>
      <Column padX={4} padY={3} gap={1}>
        <Row align="center" gap={2}>
          <Column grow>
            <Label variant="heading">Agent runs</Label>
          </Column>
          <If when={!dispatchOpen()}>
            <Action compact testID="dispatch-new" icon="plus" label="New task" accessibilityLabel="New task for a Space's agent" onPress={() => dispatchOpen.set(true)} />
          </If>
          <If when={props.onClose}>
            {(close) => <IconButton testID="runs-dialog-close" icon="close" accessibilityLabel="Close agent runs" onPress={() => close()()} />}
          </If>
        </Row>
        <Label variant="caption" color="muted">
          Active and recent, in Spaces you belong to.
        </Label>
      </Column>
      <ScrollArea padX={4} padY={1} gap={2} testID="runs-list">
        <If when={dispatchOpen()}>
          <Dispatch />
        </If>
        <If when={runsError()}>
          {(message) => (
            <Label variant="caption" color="danger" testID="runs-error">
              {message()}
            </Label>
          )}
        </If>
        <Each of={runs()} key={(run) => run.id}>
          {(run) => <RunCard run={run} onLeave={props.onClose} />}
        </Each>
        <If when={runsLoaded() && runs().length === 0 && !runsError()}>
          <RunsEmpty />
        </If>
      </ScrollArea>
    </Column>
  );
}

/** Task drafts the empty runs list suggests. Nothing names a Space or room: the user picks those. */
export const TASK_SUGGESTIONS: readonly { label: string; draft: string }[] = [
  { label: "Summarize a room", draft: "Summarize what happened in this room recently." },
  { label: "List open work", draft: "List the open tasks here and anything that's blocked." },
];

/**
 * No runs yet: what this list is for, and two task drafts. A suggestion opens
 * the task form and fills its draft only when the draft is empty. It never
 * chooses a Space or room, never touches the Chat's message box, and never
 * sends. Without a Space there is nowhere to send a task, so the suggestions
 * are disabled and say why.
 */
function RunsEmpty() {
  const noSpace = () => spaces().length === 0;
  const suggest = (text: string) => {
    if (noSpace()) return;
    dispatchOpen.set(true);
    if (dispatchDraft().trim() === "") dispatchDraft.set(text);
  };
  return (
    <Column gap={2} padY={3} align="center" testID="runs-empty">
      <Icon name="runner" color="muted" size={20} />
      <Label variant="caption" color="muted" testID="runs-empty-text">
        {noSpace()
          ? "No runs yet. Join or create a Space to send its agent a task."
          : "No runs yet. Tasks you send to a Space's agent show up here."}
      </Label>
      <Row gap={2} wrap justify="center">
        <Each of={[...TASK_SUGGESTIONS]} key={(s) => s.label}>
          {(s) => (
            <Action
              compact
              variant="secondary"
              icon="plus"
              testID={`runs-suggestion:${TASK_SUGGESTIONS.indexOf(s())}`}
              label={s().label}
              disabled={noSpace()}
              accessibilityLabel={
                noSpace()
                  ? `${s().label}: unavailable, you're not in any Space yet`
                  : dispatchDraft().trim() !== ""
                    ? `${s().label}: open the task form (your draft is kept)`
                    : `${s().label}: open the task form with this draft`
              }
              onPress={() => suggest(s().draft)}
            />
          )}
        </Each>
      </Row>
    </Column>
  );
}

function RunCard(props: { run: () => Run; /** Opening the run leaves the Chat: close the dialog first. */ onLeave?: () => void }) {
  const run = () => props.run();
  const scope = () => runScope(run());
  return (
    <Surface variant="card" pad={3} gap={2} testID={`run:${run().id}`}>
      <Row align="center" gap={2}>
        <Badge tone={runTone(run().status)} label={runStatusLabel(run().status)} />
        <Column grow>
          <Label variant="caption" color="muted" lines={1}>
            {`${scope().space} › #${scope().room}`}
          </Label>
        </Column>
      </Row>
      <Label variant="body" lines={2} testID={`run-title:${run().id}`}>
        {runTitle(run().task)}
      </Label>
      <Label variant="caption" color="muted">
        {`Started by ${run().actor === me() ? "you" : run().actor}`}
      </Label>
      <Row gap={2} align="center" wrap>
        <Action
          compact
          testID={`run-open:${run().id}`}
          label="Open"
          accessibilityLabel={`Open #${scope().room}`}
          onPress={() => {
            props.onLeave?.();
            void openRun(run());
          }}
        />
        <Action
          compact
          testID={`run-follow:${run().id}`}
          label="Follow up"
          accessibilityLabel={`Follow up in #${scope().room}`}
          onPress={() => {
            dispatchOpen.set(true);
            void followUp(run());
          }}
        />
        <Spacer />
        <If when={isActiveRun(run())}>
          <Action
            compact
            testID={`run-stop:${run().id}`}
            label="Stop"
            busyLabel="Stopping…"
            busy={runBusy() === run().id}
            variant="destructive"
            accessibilityLabel="Stop this run"
            onPress={() => void stopRun(run())}
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
    <Surface variant="card" pad={3} gap={2} testID="dispatch">
      <Row align="center" gap={2}>
        <Column grow>
          <Label variant="strong">Send a task to a Space's agent</Label>
        </Column>
        {/* Folding the form keeps the draft and the chosen Space and room. */}
        <Action compact testID="dispatch-close" label="Close" variant="ghost" accessibilityLabel="Close the task form (your draft is kept)" onPress={() => dispatchOpen.set(false)} />
      </Row>
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
          <Label variant="caption" color={targetRoom() ? "default" : "muted"} testID="dispatch-recipient">
            {targetSpace() && targetRoom() ? `To: agent in ${targetSpace()!.name} › #${targetRoom()!.name}` : "Choose a Space and a room."}
          </Label>
          <Row align="center" gap={2}>
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
