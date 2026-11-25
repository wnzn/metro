// The personal Chat (outside every Space) and the human's meta controls: the
// agent runs in the user's Spaces, and sending a task to an explicitly chosen
// Space and room. The Chat's own agent can't see Spaces; these controls are
// the user's, and the server checks membership for each.
//
// An empty Chat offers a few suggested drafts. Pressing one only fills an
// empty message box: it never sends, and never replaces what the user typed.
// The task form stays folded behind "New task" until the user opens it (or
// presses Follow up), and it scrolls with the runs, so it never covers them.

import { cleanup, effect, keymap, signal } from "nsh";
import type { ViewRef } from "nsh";
import {
  activeEvents,
  activeLoaded,
  chatUnavailable,
  dispatchDraft,
  dispatchError,
  dispatching,
  dispatchNotice,
  dispatchRevalidating,
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
  setDispatchDraft,
  setDispatchRoom,
  setDispatchSpace,
  setDraft,
  spaces,
  stopRun,
} from "../store";
import type { Run } from "../store";
import { Action, ActionBar, Badge, Banner, Chip, Column, Field, Icon, IconButton, Label, Modal, modalOpen, PanelToggle, ResizeHandle, Row, runsPanel, ScrollArea, Spacer, Surface } from "../ui";
import { EmptyState, sidePanelFits, SuggestionChip } from "../ui/activity";
import { runStatusLabel, runTitle, runTone } from "./activity/runs";
import { Composer, ConnectionBanner, Transcript } from "./Conversation";
import { CreateSpaceForm } from "./CreateSpaceForm";
import { ModelOnboarding, needsChatModel } from "./ModelOnboarding";
import { roomTree } from "./Sidebar";

/** The task form is open (folded behind "New task" otherwise). */
const dispatchOpen = signal(false);

/** Drafts the empty Chat suggests. The personal agent has no tools, so none asks for any. */
export const CHAT_SUGGESTIONS: readonly { label: string; draft: string }[] = [
  { label: "Plan my day", draft: "Help me plan my day. Here's what I need to get done: " },
  { label: "Draft a status update", draft: "Help me write a short status update about: " },
  { label: "Explain something simply", draft: "Explain this in plain words: " },
];

export function ChatView() {
  const runsDialogOpen = signal(false);
  const showRuns = () => sidePanelFits() ? runsPanel.toggle() : runsDialogOpen.set(!runsDialogOpen());
  effect(() => { if (sidePanelFits()) runsDialogOpen.set(false); });
  try {
    cleanup(keymap.bind("mod+shift+b", () => { if (!modalOpen() || runsDialogOpen()) showRuns(); }, { when: "always" }));
  } catch { /* The header button works on hosts that reserve this shortcut. */ }
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
          <PanelToggle side="right" name="agent runs" testID="runs-toggle"
            status={runs().some(isActiveRun) ? `${runs().filter(isActiveRun).length} active` : undefined}
            attention={runs().some(isActiveRun)}
            expanded={sidePanelFits() ? runsPanel.open() : runsDialogOpen()} shortcut="Shift+B" onPress={showRuns} />
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
      <If when={sidePanelFits() && runsPanel.open()}>
        <ResizeHandle panel={runsPanel} edge="start" name="agent runs" testID="resize-runs" />
        <Surface variant="aside" testID="runs-aside" width={runsPanel.width()}>
        <RunsPanel visible={runsPanel.open() && sidePanelFits()} />
        </Surface>
      </If>
      <Modal testID="runs-dialog" title="Agent runs" open={runsDialogOpen() && !sidePanelFits()} onClose={() => runsDialogOpen.set(false)}>
        <RunsPanel visible={runsDialogOpen() && !sidePanelFits()} onClose={() => runsDialogOpen.set(false)} />
      </Modal>
    </Surface>
  );
}

/** An empty Chat: what it is for, and drafts that only fill an empty message box. */
function ChatEmpty() {
  const typed = () => draft().trim().length > 0;
  return (
    <Column grow>
      {/* Shows only while nothing can answer yet (no model of mine, none shared, no operator model). */}
      <Column padX={6} padY={needsChatModel() ? 4 : 0}>
        <ModelOnboarding testID="chat-onboarding" />
      </Column>
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
    </Column>
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
        <CreateSpaceForm onboarding name={name} setName={name.set} />
      </Column>
    </Banner>
  );
}

/** The runs list and task form: in the side panel, or in the dialog (with `onClose`). */
function RunsPanel(props: { visible: boolean; onClose?: () => void }) {
  type Move = NonNullable<ReturnType<typeof followUp>>;
  const choice = signal<{ run: Run; decision: Move; opener?: ViewRef } | null>(null);
  const resolving = signal<Move | null>(null);
  const moving = signal<Move | null>(null);
  const cancelMove = () => { choice.set(null); resolving.set(null); moving.set(null); };
  const visibleForm = () => props.visible && dispatchOpen();
  const activeChoice = () => {
    const item = choice();
    return visibleForm() && item?.decision.current() && runDestination(item.run) ? item : null;
  };
  const verification = () => moving()?.current() ? "Checking the new destination…"
    : resolving()?.current() ? "Looking up the room or thread…"
    : dispatchRevalidating() ? "Checking the selected destination…" : null;
  effect(() => { if (!props.visible) cancelMove(); });
  cleanup(cancelMove);
  const follow = async (run: Run, opener?: ViewRef) => {
    cancelMove();
    dispatchOpen.set(true);
    const decision = followUp(run);
    if (!decision) return;
    if (!runDestination(run)) {
      resolving.set(decision);
      const stillHere = () => resolving() === decision && visibleForm();
      try {
        if (!await decision.lookup(stillHere) || !stillHere()) return;
      } finally {
        if (resolving() === decision) resolving.set(null);
      }
    }
    if (visibleForm() && decision.current() && runDestination(run)) choice.set({ run, decision, opener });
  };
  const closeChoice = () => {
    const opener = choice()?.opener;
    choice.set(null);
    if (opener) void opener.measure().then(layout => {
      if (layout && props.visible && !choice()) opener.focus();
    });
  };
  const move = async (decision: Move) => {
    if (activeChoice()?.decision !== decision) return;
    closeChoice();
    moving.set(decision);
    try {
      await decision.move(() => moving() === decision && visibleForm());
    } finally {
      if (moving() === decision) moving.set(null);
    }
  };
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
          <Dispatch verification={verification()} onEdit={cancelMove} onClose={() => {
            cancelMove();
            dispatchOpen.set(false);
          }} />
        </If>
        <If when={runsError()}>
          {(message) => (
            <Label variant="caption" color="danger" testID="runs-error">
              {message()}
            </Label>
          )}
        </If>
        <Each of={runs()} key={(run) => run.id}>
          {(run) => <RunCard run={run} onFollow={(next, opener) => void follow(next, opener)} onLeave={props.onClose} />}
        </Each>
        <If when={runsLoaded() && runs().length === 0 && !runsError()}>
          <RunsEmpty />
        </If>
      </ScrollArea>
      <If when={activeChoice()}>
        {item => (
          <Modal open size="compact" title="Move task draft" testID="dispatch-move-confirm" onClose={closeChoice}>
            <Column pad={4} gap={3}>
              <Label heading variant="title">Move this draft?</Label>
              <Label>{`Follow up in ${runDestination(item().run)} instead? Your draft text stays the same. Nothing is sent.`}</Label>
              <Label variant="caption" color="muted">{dispatchRecipient()}</Label>
              <ActionBar>
                <Action label="Keep draft" testID="dispatch-keep-draft" onPress={closeChoice}
                  viewRef={node => {
                    const active = choice();
                    void node.measure().then(layout => {
                      if (layout && active && choice() === active && props.visible && active.decision.current()) node.focus();
                    });
                  }} />
                <Action label="Move draft" testID="dispatch-move-draft" variant="primary" disabled={dispatching()} onPress={() => void move(item().decision)} />
              </ActionBar>
            </Column>
          </Modal>
        )}
      </If>
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
    if (dispatchDraft().trim() === "") setDispatchDraft(text);
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

function runDestination(run: Run) {
  const { space, room } = runScope(run);
  return space && room ? `${space} › #${room}` : undefined;
}

function RunCard(props: { run: () => Run; onFollow: (run: Run, opener?: ViewRef) => void; /** Opening the run leaves the Chat: close the dialog first. */ onLeave?: () => void }) {
  const run = () => props.run();
  const scope = () => runScope(run());
  const destination = () => runDestination(run());
  const lookupScope = () => scope().space ? ` in ${scope().space}` : "";
  let followButton: ViewRef | undefined;
  return (
    <Surface variant="card" pad={3} gap={2} testID={`run:${run().id}`}>
      <Row align="center" gap={2}>
        <Badge tone={runTone(run().status)} label={runStatusLabel(run().status)} />
        <Column grow>
          <Label variant="caption" color="muted" lines={1}>
            {destination() ?? (scope().space ? `${scope().space} › Room or thread not loaded` : "Destination unavailable")}
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
          accessibilityLabel={destination() ? `Open ${destination()}` : `Open run destination${lookupScope()}`}
          onPress={() => {
            props.onLeave?.();
            void openRun(run());
          }}
        />
        <Action
          compact
          testID={`run-follow:${run().id}`}
          label="Follow up"
          accessibilityLabel={destination() ? `Follow up in ${destination()}` : `Look up the follow-up destination${lookupScope()}`}
          viewRef={node => { followButton = node; }}
          onPress={() => props.onFollow(run(), followButton)}
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

const dispatchSpace = () => spaces().find(s => s.id === dispatchTarget()?.spaceId);
const dispatchRoom = () => roomsOf(dispatchTarget()?.spaceId ?? "").find(r => r.id === dispatchTarget()?.roomId);
function dispatchRecipient() {
  const space = dispatchSpace(), room = dispatchRoom();
  return space && room ? `To: agent in ${space.name} › #${room.name}` : "Choose a Space and a room.";
}

function Dispatch(props: { verification: string | null; onEdit: () => void; onClose: () => void }) {
  const target = () => dispatchTarget();
  const ready = () => !!dispatchSpace() && !!dispatchRoom() && dispatchDraft().trim().length > 0 && !dispatching() && !props.verification;
  return (
    <Surface variant="card" pad={3} gap={2} testID="dispatch">
      <Row align="center" gap={2}>
        <Column grow>
          <Label variant="strong">Send a task to a Space's agent</Label>
        </Column>
        {/* Folding the form keeps the draft and the chosen Space and room. */}
        <Action compact testID="dispatch-close" label="Close" variant="ghost" accessibilityLabel="Close the task form (your draft is kept)" onPress={props.onClose} />
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
              {(s) => <Chip testID={`dispatch-space:${s().id}`} label={s().name} selected={target()?.spaceId === s().id} onPress={() => {
                props.onEdit();
                void setDispatchSpace(s().id);
              }} />}
            </Each>
          </Row>
          <If when={dispatchSpace()}>
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
                    onPress={() => {
                      props.onEdit();
                      setDispatchRoom(row().room.id);
                    }}
                  />
                )}
              </Each>
            </Row>
          </If>
          <Field
            testID="dispatch-text"
            accessibilityLabel="Task for the agent"
            value={dispatchDraft()}
            onChange={text => {
              props.onEdit();
              setDispatchDraft(text);
            }}
            placeholder="Describe the task or follow-up"
            multiline
            keepFocusOnSubmit
          />
          <Label variant="caption" color={dispatchRoom() ? "default" : "muted"} testID="dispatch-recipient">
            {dispatchRecipient()}
          </Label>
          <If when={props.verification}>
            {message => <Label variant="caption" color="muted" testID="dispatch-verifying">{message()}</Label>}
          </If>
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
