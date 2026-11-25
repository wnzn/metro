// A Space's room or thread: its header, the transcript and the composer with
// @mentions.
//
// Mentions: typing "@" opens suggestions at the caret (the field's
// `onCaretMove` box in the window), the @ button opens them under itself
// with the focus on the first one. While the list is open and the caret is
// in the field, Up and Down move the highlight (wrapping, each move
// announced), Tab or Return inserts the highlighted suggestion and sends
// nothing, and Escape or a press outside closes it, as leaving the room
// does; Return sends only while the list is closed. In the list itself (the
// @ button puts the focus there) Up and Down move and Return or Space picks. Only this Space's agents and members are
// offered: the generic @agent (the default agent, or the sender's classifier
// pick), each named agent by its picture and @handle, each person by display
// name and picture with their stable id.
//
// Ask agent: with several agents, a chooser joined to it picks which agent it
// asks (./space/askAgent).

import { announce, cleanup, computed, effect, keymap, signal, untracked } from "nsh";
import type { JSX } from "nsh";
import type { Bounds, ViewRef } from "nsh";
import {
  actionError,
  activeRoom,
  activeLoaded,
  activeSpace,
  activeSpaceId,
  addMention,
  beginMentionVisit,
  chooseReplyTarget,
  closeReplyPane,
  composerScope,
  draft,
  inSpaceComposer,
  loadMembersForMentions,
  me,
  members,
  membersError,
  mentionDraft,
  mentionVisitKey,
  mentions,
  muteReplyAgent,
  openManage,
  openReplyThread,
  removeMention,
  previewMentionDraft,
  previewMentionMatches,
  rooms,
  roomsLoaded,
  replyPane,
  replyTarget,
  selectRoom,
  setDraft,
  status,
  switchingSpace,
  verifiedConfirmationDirectory,
} from "../store";
import {
  Action,
  AnchoredPopover,
  AskAgentChooser,
  AuthorGlyph,
  Badge,
  Banner,
  Column,
  ComposerHint,
  InlineLink,
  Label,
  Modal,
  PopoverNote,
  RecipientBar,
  RecipientChip,
  ResizeHandle,
  Row,
  SuggestionList,
  SuggestionOption,
  Surface,
  ToolbarIconButton,
  modalOpen,
  threadPanel,
} from "../ui";
import { sidePanelFits } from "../ui/activity";
import type { AskAgentChoice } from "../ui";
import type { SpaceAgent } from "../api/spaces";
import { RESERVED_HANDLE } from "../api/spaces";
import { Composer, ConnectionBanner, fillDraft, focusComposer, messageAuthorName, replyExcerpt, Transcript } from "./Conversation";
import { NewSpaceRunnerFeedback } from "./CreateSpaceForm";
import type { DraftIdea } from "./Conversation";
import type { RoomEvent } from "../store";
import { PersonAvatar } from "./profile/PersonAvatar";
import { ensureMentionPerson } from "./profile/profileState";
import { agentById, agentMentionOptions, defaultAgent, spaceAgents } from "./space/agents";
import { askAgentId, askChoice, chooseAskAgent, classifierOffered, CLASSIFIER_CHOICE, hasAgentChoice } from "./space/askAgent";
import { SpaceAgentAvatar } from "./space/SpaceAgentAvatar";
import { openSpaceSettings, spaceSettingsFor } from "./space/spaceSettingsState";
import { mentionIdentityKey, scanMentionDraft } from "./mentions/draft";
import type { MentionDirectory, MentionRecipient } from "./mentions/draft";
import { beginMentionAgentVisit, refreshMentionAgents, spaceMentionDirectory, verifiedMembers } from "./mentions/roster";
import { mentionSuggestion } from "./mentions/suggestions";
import { ThreadPane } from "./replies/ThreadPane";

/** Suggestions of an empty room: they fill the draft (and its recipients), never send. */
const ROOM_IDEAS: DraftIdea[] = [
  { id: "hello", label: "Say hello", apply: () => fillDraft("Hello everyone! ") },
  {
    id: "ask-agent",
    label: "Ask the agent",
    apply: () => {
      setDraft("");
      addMention({ kind: "agent" });
      fillDraft(`${draft()}what can you help with in this room?`);
    },
  },
  { id: "mention", label: "Mention someone", apply: () => fillDraft("@") },
];

const roomsPending = () => switchingSpace() && !roomsLoaded();

export function SpaceView() {
  // Its header (breadcrumb, runner, Manage, Space settings) is the Shell's one
  // header row: RoomHeaderTitle and RoomHeaderActions below.
  let announcedFor: string | null = null;
  let opener: ViewRef | undefined;
  const closeThread = () => {
    const button = opener;
    opener = undefined;
    closeReplyPane();
    if (button) void button.measure().then(layout => {
      if (layout && !replyPane()) button.focus();
    });
  };
  const reply = (event: RoomEvent, button?: ViewRef) => {
    if (activeRoom()?.parentRoomId) {
      chooseReplyTarget(event);
      focusComposer();
    } else void openReplyThread(event).then(ok => {
      if (!ok) return;
      opener = button;
      queueMicrotask(() => focusComposer());
    });
  };
  const pickThreadReply = (event: RoomEvent) => {
    chooseReplyTarget(event);
    focusComposer();
  };
  effect(() => {
    if (!replyPane() || !sidePanelFits() || modalOpen()) return;
    return keymap.bind("escape", closeThread, { when: "always" });
  });
  effect(() => {
    const pending = roomsPending() ? activeSpace() : null;
    if (pending && pending.id !== announcedFor) announce(`Loading rooms in ${pending.name}.`);
    announcedFor = pending?.id ?? null;
  });
  return (
    <Surface variant="panel" grow testID="space-view">
      <ConnectionBanner />
      <NewSpaceRunnerFeedback />
      <If when={!!activeRoom()} else={
        <If when={roomsPending()} else={
          <Column grow align="center" justify="center" pad={6} gap={2}>
            <If when={actionError()}>
              {error => <Banner tone="danger" variant="inline" testID="rooms-error" message={error()} />}
            </If>
            <If when={roomsLoaded() && rooms().length === 0} else={<Label color="muted">Select a room in the sidebar.</Label>}>
              <Label color="muted" testID="rooms-empty">No rooms in this Space yet.</Label>
            </If>
          </Column>
        }>
          <Column grow align="center" justify="center" pad={6} testID="rooms-loading">
            <Label color="muted">Rooms will appear here when they’re ready.</Label>
          </Column>
        </If>
      }>
        <Row grow>
          <Column grow>
            <Transcript roomId={activeRoom()?.id} onReply={reply} emptyTitle="Start the conversation"
              emptyText="Messages here are seen by this Space's members." ideas={replyPane() ? [] : ROOM_IDEAS} />
            <If when={!replyPane()}><RoomComposer /></If>
          </Column>
          <If when={replyPane() && sidePanelFits()}>
            <ResizeHandle panel={threadPanel} edge="start" name="thread" testID="resize-thread" />
            <Surface variant="aside" testID="thread-aside" width={threadPanel.width()}>
              <ThreadPane onReply={pickThreadReply} onClose={closeThread} composer={<RoomComposer thread />} />
            </Surface>
          </If>
        </Row>
        <Modal testID="thread-dialog" title="Thread" size="full-height" open={!!replyPane() && !sidePanelFits()} onClose={closeThread}>
          <If when={replyPane() && !sidePanelFits()}>
            <ThreadPane onReply={pickThreadReply} onClose={closeThread} composer={<RoomComposer thread />} />
          </If>
        </Modal>
      </If>
    </Surface>
  );
}

const ROOM_DESCRIPTION = "Members of this Space share its rooms and its agents.";

/** The room's breadcrumb: Space › # room; a thread links back to its parent. The description is its tooltip. */
export function RoomHeaderTitle() {
  const parent = () => {
    const id = activeRoom()?.parentRoomId;
    return id ? rooms().find((r) => r.id === id) ?? null : null;
  };
  return (
    // Grows into the room the header row leaves; the names truncate before the parent's link does.
    <Row grow align="center" gap={2} testID="room-header">
      <Label variant="body" color="muted" lines={1} tooltip={ROOM_DESCRIPTION}>
        {activeSpace()?.name ?? ""}
      </Label>
      <Label variant="body" color="muted">
        ›
      </Label>
      <Label variant="heading" lines={1} testID="room-title" heading tooltip={parent() ? undefined : ROOM_DESCRIPTION}>
        {activeRoom() ? `# ${activeRoom()!.name}` : roomsPending() ? "Loading rooms…" : "No room selected"}
      </Label>
      <If when={parent()}>
        {(p) => (
          <Row align="center" gap={1}>
            <Label variant="caption" color="muted" lines={1}>
              from
            </Label>
            <InlineLink testID="open-parent" label={`# ${p().name}`} accessibilityLabel={`Thread delegated from # ${p().name}. Open the parent room`} onPress={() => selectRoom(p().id)} />
          </Row>
        )}
      </If>
    </Row>
  );
}

/** Room actions: runner status, an owner's Members shortcut, and Space settings. */
export function RoomHeaderActions() {
  const paired = () => activeSpace()?.runnerPaired;
  return (
    <>
      <Badge testID="runner-state" tone={paired() ? "runner" : "neutral"}
        label={paired() === true ? "Runner attached" : paired() === false ? "No runner" : "Runner not confirmed"} />
      <If when={activeSpace()?.role === "owner"}>
        <Action testID="manage-space" label="Members" onPress={() => openManage()} />
      </If>
      <ToolbarIconButton
        testID="space-settings-open"
        icon="settings"
        accessibilityLabel={`Open settings for ${activeSpace()?.name ?? "this Space"}`}
        tooltip="Space settings"
        disabled={!activeSpace()}
        onPress={() => {
          const sid = activeSpaceId();
          if (sid) openSpaceSettings(sid);
        }}
      />
    </>
  );
}

/** One suggestion of the mention list; `agent`: the Space agent it names (its picture leads the row). */
type MentionOption = { key: string; mention: MentionRecipient; title: string; detail: string; note?: string; testID: string; label: string; agent?: SpaceAgent };

/**
 * The generic @agent, the Space's named agents and the members (never the
 * signed-in user for general suggestions) whose handle, id or name matches.
 * Ambiguity choices are restricted to the original stable candidates.
 */
export function mentionOptions(
  query: string,
  list: readonly { userId: string; role?: string }[] | null,
  verifiedPeople: MentionDirectory["members"],
  myId: string | null,
  spaceId?: string | null,
  choices?: readonly MentionRecipient[],
): MentionOption[] {
  const q = query.toLowerCase();
  const out: MentionOption[] = [];
  const keys = choices && new Set(choices.map(mentionIdentityKey));
  const names = new Map(verifiedPeople.map(m => [m.id, m.displayName]));
  const def = defaultAgent(spaceId);
  const several = spaceAgents(spaceId).length > 1;
  if (keys ? keys.has("generic") : "agent".startsWith(q)) {
    const who = def ? `${def.name}, the default agent` : "this Space's agent";
    const classifier = several && classifierOffered(spaceId);
    out.push({
      key: "generic",
      mention: { kind: "agent" },
      title: "agent",
      detail: classifier ? `${def?.name ?? "default"} or your classifier's pick` : def && several ? `${def.name} · default` : "starts agent work",
      testID: "pick:agent",
      label: `Mention @agent: starts ${classifier ? `${who}, or the agent your classifier picks` : who}`,
      agent: def ?? undefined,
    });
  }
  for (const o of agentMentionOptions(spaceId, "")) {
    const a = o.agent;
    if (keys ? !keys.has(o.key) : !(a.id.toLowerCase().includes(q) || a.name.toLowerCase().includes(q) || a.handle !== RESERVED_HANDLE && a.handle.startsWith(q))) continue;
    // The reserved handle is always generic; show the original agent's
    // actual id for disambiguation and offer it by name/id, not by @agent.
    if (!keys && a.handle === RESERVED_HANDLE && !q) continue;
    const detail = a.handle === RESERVED_HANDLE || choices ? `@${a.id}` : o.detail;
    out.push({ key: o.key, mention: o.mention, title: o.title, detail, testID: o.testID,
      label: `Mention ${a.name} (${detail}): ${a.description}`, agent: a });
  }
  for (const m of list ?? []) {
    if (!names.has(m.userId)) continue;
    const name = names.get(m.userId);
    if (keys ? !keys.has(`user:${m.userId}`)
      : m.userId === myId || (!m.userId.toLowerCase().includes(q) && !(name ?? "").toLowerCase().includes(q))) continue;
    const owner = m.role === "owner";
    out.push({
      key: `user:${m.userId}`,
      mention: { kind: "user", id: m.userId },
      title: name ?? m.userId,
      detail: `@${m.userId}`,
      note: owner ? "owner" : undefined,
      testID: `pick:${m.userId}`,
      label: `Mention ${name && name !== m.userId ? `${name} (@${m.userId})` : `@${m.userId}`}${owner ? ", owner" : ""}: notifies them only`,
    });
  }
  return out;
}

function RoomComposer(props: { thread?: boolean }) {
  /** Opened with the @ button (not by typing). */
  const explicit = signal(false);
  /** The draft the user closed the suggestions at: typing on opens them again. */
  const dismissedAt = signal<string | null>(null);
  /** The caret's box in the window, as the field last reported it. */
  const caret = signal<Bounds | null>(null);
  const selection = signal<{ start: number; end: number } | null>(null);
  const highlighted = signal<string | null>(null);
  const optionRefs = new Map<string, ViewRef>();
  /** The composer's field has the keyboard focus. */
  const fieldFocused = signal(false);
  let atButton: ViewRef | null = null;
  const preview = computed(previewMentionDraft);
  const matches = computed(() => previewMentionMatches(preview()));
  const ambiguous = () => matches().find(d => d.kind === "ambiguous");
  const currentDirectory = () => ambiguous()?.preflight ? verifiedConfirmationDirectory()
    : activeSpaceId() ? spaceMentionDirectory(activeSpaceId()!) : null;
  const unresolved = () => matches().find(d => d.kind === "unresolved");
  const chips = computed(() => mentions().filter(a => !matches().some(m => m.kind === "ambiguous" && m.start === a.start)));
  const replyAgent = () => {
    const target = replyTarget();
    if (!target || target.event.kind !== "agent" || target.muted) return null;
    const saved = target.event.activity?.agent;
    const sid = activeSpaceId();
    return saved ? { id: saved.id, name: agentById(sid, saved.id)?.name ?? saved.name } :
      defaultAgent(sid) ?? { id: undefined, name: "the agent" };
  };
  const unresolvedHint = () => {
    const m = unresolved();
    if (!m) return "";
    const directory = currentDirectory();
    const value = preview();
    const matchNow = directory && scanMentionDraft({
      ...value, decisions: value.decisions?.filter(d => d.start !== m.start),
    }, directory, { finalize: true }).some(x => x.start === m.start && x.kind !== "unresolved");
    return !directory || matchNow
      ? `${m.literal} isn't verified for this draft; it's sent as text.`
      : `${m.literal} isn't anyone in this Space; it's sent as text.`;
  };
  const partial = computed(() => {
    const s = selection();
    if (s && s.start !== s.end) return null;
    return mentionSuggestion(mentionDraft(), s?.end ?? draft().length);
  });
  const query = () => partial()?.query ?? "";
  const options = computed(() => {
    const directory = currentDirectory();
    if (!directory) return [];
    const decision = ambiguous();
    return mentionOptions(query(), members(), directory.members, me(), activeSpaceId(), decision?.choices);
  });
  const deliberate = () => options().findIndex(o => o.key === highlighted());
  const active = () => {
    const index = deliberate();
    return index < 0 && !ambiguous() ? 0 : index;
  };
  const typing = () => !!partial() && fieldFocused() && (options().length > 0 || !currentDirectory() && !query());
  const open = () => inSpaceComposer() && (explicit() || dismissedAt() !== draft() && (typing() || !!ambiguous() && fieldFocused()));

  // The same roster feeds sent chips even if the picker was never opened.
  const visitKey = computed(() => spaceSettingsFor() ? null : mentionVisitKey());
  effect(() => {
    if (!visitKey()) return;
    const sid = untracked(activeSpaceId);
    if (!sid) return;
    const scope = untracked(composerScope);
    const endVisit = untracked(() => beginMentionVisit(sid, scope));
    const endAgentVisit = untracked(() => beginMentionAgentVisit(sid));
    const refresh = () => {
      if (!inSpaceComposer() || activeSpaceId() !== sid) return;
      void loadMembersForMentions();
      refreshMentionAgents(sid);
    };
    const timer = setInterval(refresh, 10_000);
    return () => { clearInterval(timer); endAgentVisit(); endVisit(); };
  });
  effect(() => {
    const sid = activeSpaceId();
    const ids = (sid ? verifiedMembers(sid) : null)?.map(m => m.userId) ?? [];
    untracked(() => { for (const id of ids) void ensureMentionPerson(id); });
  });
  // Keep a deliberated stable key through reorder, never through a different
  // occurrence or an option disappearing from the current roster.
  let lastQuery = "";
  effect(() => {
    const m = ambiguous() ?? partial();
    const key = `${composerScope()}:${m?.start ?? ""}:${m?.end ?? ""}:${m?.literal ?? ""}:${query()}:${!!ambiguous()}`;
    const id = highlighted();
    const present = id !== null && options().some(o => o.key === id);
    untracked(() => {
      if (key !== lastQuery || !present) highlighted.set(null);
      else optionRefs.get(id)?.reveal({ vertical: "nearest", animated: false });
      lastQuery = key;
    });
  });
  // Leaving the room (or Space) closes it, whatever the draft still says.
  let shownFor: string | null = null;
  effect(() => {
    const key = inSpaceComposer() ? composerScope() : null;
    if (key === shownFor) return;
    shownFor = key;
    untracked(() => {
      explicit.set(false);
      highlighted.set(null);
      caret.set(null);
      selection.set(null);
      dismissedAt.set(draft());
    });
  });
  // Up and Down move the highlight while the caret is in the field and the
  // list is open: an app binding with `when: "always"` fires even though the
  // input would move its caret. Bound only then; removed on blur, close,
  // room switch and unmount (the effect's cleanup), so the caret keeps its
  // arrows otherwise.
  //
  // Tab inserts the highlighted suggestion as Return does (and is bound only
  // then, so Tab leaves the field as usual otherwise). Each move is
  // announced, so a screen reader says the option the caret's keys now pick.
  effect(() => {
    if (!open() || !fieldFocused()) return;
    const move = (step: number) => () => {
      const n = options().length;
      if (n === 0) return;
      const from = active();
      const index = from < 0 ? (step > 0 ? 0 : n - 1) : (from + step + n) % n;
      const option = options()[index];
      highlighted.set(option.key);
      optionRefs.get(option.key)?.reveal({ vertical: "nearest", animated: false });
      announce(`${option.label}, ${index + 1} of ${n}`);
    };
    return keymap.bind(
      { ...(options().length ? { down: move(1), up: move(-1) } : {}),
        ...(options().length || ambiguous() ? { tab: () => void intercept() } : {}), escape: close },
      { when: "always", repeat: true },
    );
  });
  function close() {
    explicit.set(false);
    highlighted.set(null);
    dismissedAt.set(draft());
    if (inSpaceComposer()) focusComposer(Math.min(selection()?.end ?? draft().length, draft().length));
  }
  const choose = (key: string) => {
    const option = open() && options().find(o => o.key === key);
    if (!option) {
      announce("That recipient is no longer available. Check the list and try again.");
      return;
    }
    const current = selection();
    const pos = current && current.start === current.end ? Math.min(current.end, draft().length) : draft().length;
    const end = addMention(option.mention, ambiguous() ?? partial() ?? { start: pos, end: pos, literal: "" });
    if (end === null) {
      announce("That recipient is no longer available. Check the list and try again.");
      return;
    }
    explicit.set(false);
    highlighted.set(null);
    caret.set(null);
    selection.set({ start: end, end });
    focusComposer(end);
  };
  const toggle = () => {
    if (open()) close();
    else {
      dismissedAt.set(null);
      explicit.set(true);
      if (ambiguous()) focusComposer(Math.min(selection()?.end ?? draft().length, draft().length));
      if (!currentDirectory()) {
        void loadMembersForMentions();
        const sid = activeSpaceId();
        if (sid) refreshMentionAgents(sid);
      }
    }
  };
  /** Return in the field: picks the highlighted suggestion instead of sending. */
  function intercept() {
    const option = options()[active()];
    if (ambiguous() && (!open() || deliberate() < 0 || !option)) {
      dismissedAt.set(null);
      announce("Pick who you mean before sending.");
      return true;
    }
    if (!open() || !option) return false;
    choose(option.key);
    return true;
  }
  /** Next to the caret while typing; under the button when it opened the list or no caret was reported yet. */
  const anchor = () => (typing() && !explicit() ? caret() ?? atButton : atButton);

  return (
    <Composer
      placeholder={props.thread ? "Reply in thread…" : activeRoom() ? `Message #${activeRoom()!.name}` : "Select a room"}
      agentHint={
        hasAgentChoice(activeSpaceId())
          ? "Return sends · Shift+Return adds a line · @agent asks the default agent, @handle a named one, Ask agent the one you chose; @name only notifies them."
          : "Return sends · Shift+Return adds a line · @agent or Ask agent asks this Space's agent; @name only notifies them."
      }
      askAgentId={askAgentId(activeSpaceId())}
      replyAgentId={replyAgent()?.id}
      askLabel={askLabel()}
      askPicture={askPicture()}
      askChooser={askChooser()}
      disabled={!activeRoom() || (props.thread && !activeLoaded())}
      onSubmitIntercept={intercept}
      onCaretMove={(e) => caret.set(e.pageBounds)}
      onSelectionChange={selection.set}
      onFocusChange={(f) => fieldFocused.set(f)}
      tools={
        <ToolbarIconButton
          testID="mention"
          icon="at"
          accessibilityLabel="Mention the agent or a member"
          tooltip="Mention someone (@)"
          active={open()}
          disabled={!activeRoom()}
          ref={(node) => (atButton = node)}
          onPress={toggle}
        />
      }
      above={
        <>
          <If when={replyTarget()}>
            {target => <Column gap={1} testID="reply-context">
              <Label variant="caption" color="muted" lines={2} testID="composer-reply-quote">
                {messageAuthorName(target().event)} · {replyExcerpt(target().event)}
              </Label>
              <If when={replyAgent()}>
                {agent => <RecipientBar><RecipientChip testID="reply-agent-chip" agent
                  label={`Replying to ${agent().name}`} accessibilityLabel={`Don't ask ${agent().name} to reply`}
                  onRemove={() => { muteReplyAgent(); focusComposer(); }} /></RecipientBar>}
              </If>
            </Column>}
          </If>
          <If when={chips().length > 0}>
            <RecipientBar testID="mention-chips">
              <Each of={chips()} key={(a) => mentionIdentityKey(a.mention)}>
                {(a) => (
                  <RecipientChip
                    testID={`mention-chip:${chipId(a().mention)}`}
                    label={chipLabel(a().mention, a().literal)}
                    agent={a().mention.kind === "agent"}
                    accessibilityLabel={`Remove mention ${chipLabel(a().mention, a().literal)}`}
                    onRemove={() => removeMention(a().mention)}
                  />
                )}
              </Each>
            </RecipientBar>
          </If>
          <If when={ambiguous()}>
            <ComposerHint testID="mention-pick-hint" text="Pick who you mean." />
          </If>
          <If when={unresolved()}>
            <ComposerHint testID="mention-unresolved" text={unresolvedHint()} />
          </If>
          <If when={open()}>
            <AnchoredPopover
              anchor={anchor()}
              onClose={close}
              placement="top"
              align="start"
              role="listbox"
              bounded
              testID="mention-picker"
              accessibilityLabel={`Mention in #${activeRoom()?.name ?? ""}`}
            >
              <If when={ambiguous()} else={<PopoverNote lines={2} text={`Agents and members of ${activeSpace()?.name ?? "this Space"} · ↑↓ then Return`} />}>
                {(m) => <PopoverNote lines={2} testID="mention-ambiguous" text={`Which ${m().literal.slice(1)}?`} />}
              </If>
              <SuggestionList testID="mention-options-scroll" accessibilityLabel="Mention suggestions">
                <Each of={options()} key={(o) => o.key}>
                  {(o, index) => {
                    const key = o().key;
                    cleanup(() => optionRefs.delete(key));
                    return (
                      <SuggestionOption
                        testID={o().testID}
                        title={o().title}
                        detail={o().detail}
                        note={o().note}
                        active={index() === active()}
                        accessibilityLabel={o().label}
                        leading={
                          o().mention.kind === "agent" ? (
                            <If when={o().agent && activeSpaceId() ? o().agent : null} else={<AuthorGlyph kind="agent" tone="agent" size="sm" />}>
                              {(a) => <SpaceAgentAvatar spaceId={activeSpaceId()!} agent={a()} size="sm" />}
                            </If>
                          ) : (
                            <PersonAvatar userId={(o().mention as { id: string }).id} name={o().title} verifiedName size="sm" />
                          )
                        }
                        ref={(node) => {
                         optionRefs.set(key, node);
                         if (highlighted() === key) node.reveal({ vertical: "nearest", animated: false });
                         // Never give an ambiguous chooser an implicit first row.
                         if (index() === 0 && explicit() && !ambiguous()) node.focus();
                       }}
                        focusable={!ambiguous()}
                        onPress={() => choose(key)}
                      />
                    );
                  }}
                </Each>
              </SuggestionList>
              <If when={membersError()}>{(message) => <PopoverNote lines={2} tone="danger" text={message()} />}</If>
              <If when={options().length === 0}>
                <PopoverNote lines={2} testID="mention-none" text={!currentDirectory() ? ambiguous()?.preflight
                  ? "The recipient list changed. Edit or remove this mention before sending."
                  : "The recipient list isn't ready. Check your connection."
                  : ambiguous() ? "Those recipients aren't available. Edit or remove the mention." : `No recipient matches “${query()}”.`} />
              </If>
            </AnchoredPopover>
          </If>
        </>
      }
    />
  );
}

const chipId = (m: MentionRecipient) => m.kind === "user" ? m.id : m.agentId ? `agent:${m.agentId}` : "agent";
function chipLabel(m: MentionRecipient, literal: string) {
  const sid = activeSpaceId();
  const directory = sid && spaceMentionDirectory(sid);
  if (m.kind === "user") {
    const person = directory && directory.members.find(x => x.id === m.id);
    return person ? `@${person.displayName ?? person.id}` : literal;
  }
  if (!m.agentId) return "@agent";
  const agent = directory && directory.agents.find(a => a.id === m.agentId);
  return agent ? `@${agent.name}` : literal;
}

// ---------------------------------------------------------------- Ask agent's chooser

/** The chosen agent, when Ask agent asks a named one (several agents). */
const askedAgent = () => {
  const sid = activeSpaceId();
  const c = askChoice(sid);
  return c && c !== CLASSIFIER_CHOICE ? agentById(sid, c) : null;
};

function askLabel(): string | undefined {
  const sid = activeSpaceId();
  if (!hasAgentChoice(sid)) return undefined;
  if (askChoice(sid) === CLASSIFIER_CHOICE) return "Ask agent";
  const a = askedAgent();
  return a ? `Ask ${a.name}` : undefined;
}

function askPicture(): JSX.Element | undefined {
  const sid = activeSpaceId();
  const a = askedAgent();
  return sid && a ? <SpaceAgentAvatar spaceId={sid} agent={a} size="xs" /> : undefined;
}

function askChooser(): JSX.Element | undefined {
  const sid = activeSpaceId();
  if (!sid || !hasAgentChoice(sid)) return undefined;
  const choices = (): AskAgentChoice[] => {
    const current = askChoice(sid);
    const agents = [...spaceAgents(sid)].sort((a, b) => Number(b.isDefault) - Number(a.isDefault));
    const out: AskAgentChoice[] = agents.map((a) => ({
      id: a.id,
      title: a.name,
      detail: a.isDefault ? `@${a.handle} · default` : `@${a.handle}`,
      leading: <SpaceAgentAvatar spaceId={sid} agent={a} size="sm" />,
      checked: current === a.id,
      accessibilityLabel: `Ask ${a.name}${a.isDefault ? ", the default agent" : ""}: ${a.description}`,
      onSelect: () => chooseAskAgent(sid, a.id),
    }));
    if (classifierOffered(sid)) {
      out.push({
        id: CLASSIFIER_CHOICE,
        title: "Let the classifier choose",
        detail: "Your System One picks per question",
        leading: <AuthorGlyph kind="agent" tone="agent" size="sm" />,
        checked: current === CLASSIFIER_CHOICE,
        accessibilityLabel: "Let the classifier choose: your System One connection picks the agent for each question",
        onSelect: () => chooseAskAgent(sid, CLASSIFIER_CHOICE),
        separated: true,
      });
    }
    return out;
  };
  return (
    <AskAgentChooser
      testID="ask-agent-choose"
      heading="Which agent to ask"
      choices={choices()}
      disabled={!activeRoom() || status().phase === "signedOut"}
      accessibilityLabel={`Choose which agent Ask agent asks (now ${askLabel()?.replace(/^Ask /, "") ?? "the default"})`}
    />
  );
}
