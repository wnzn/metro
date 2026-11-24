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
// @ button puts the focus there) Up and Down move and Return or Space picks. Only the agent and members of this Space are
// offered, each person by display name and picture with their stable id.

import { announce, computed, effect, keymap, signal, untracked } from "nsh";
import type { Bounds, ViewRef } from "nsh";
import {
  activeRoom,
  activeSpace,
  addMention,
  demoPending,
  draft,
  loadMembersForMentions,
  me,
  members,
  membersError,
  mentionQuery,
  mentions,
  openManage,
  removeMention,
  rooms,
  runDemo,
  selectRoom,
  setDraft,
  status,
} from "../store";
import type { Mention } from "../store";
import {
  Action,
  AnchoredPopover,
  AuthorGlyph,
  Badge,
  InlineLink,
  Label,
  PopoverNote,
  RecipientBar,
  RecipientChip,
  Row,
  SuggestionList,
  SuggestionOption,
  Surface,
  ToolbarIconButton,
} from "../ui";
import { Composer, ConnectionBanner, fillDraft, focusComposer, Transcript } from "./Conversation";
import type { DraftIdea } from "./Conversation";
import { mentionText } from "./eventKinds";
import { PersonAvatar } from "./profile/PersonAvatar";
import { profileName } from "./profile/profileState";

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

export function SpaceView() {
  // Its header (breadcrumb, runner, Run demo, Manage) is the Shell's one
  // header row: RoomHeaderTitle and RoomHeaderActions below.
  return (
    <Surface variant="panel" grow testID="space-view">
      <ConnectionBanner />
      <Transcript emptyTitle="Start the conversation" emptyText="Messages here are seen by this Space's members." ideas={ROOM_IDEAS} />
      <RoomComposer />
    </Surface>
  );
}

const ROOM_DESCRIPTION = "Members of this Space share its rooms and its agent.";

/** The room's breadcrumb: Space › # room; a thread links back to its parent. The description is its tooltip. */
export function RoomHeaderTitle() {
  const parent = () => {
    const id = activeRoom()?.parentRoomId;
    return id ? rooms().find((r) => r.id === id) ?? null : null;
  };
  return (
    <Row align="center" gap={2} testID="room-header">
      <Label variant="body" color="muted" lines={1} tooltip={ROOM_DESCRIPTION}>
        {activeSpace()?.name ?? ""}
      </Label>
      <Label variant="body" color="muted">
        ›
      </Label>
      <Label variant="heading" lines={1} testID="room-title" heading tooltip={parent() ? undefined : ROOM_DESCRIPTION}>
        {activeRoom() ? `# ${activeRoom()!.name}` : "No room selected"}
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

/** The room's actions at the end of the header row: whether a runner serves it, Run demo, Manage (owners). */
export function RoomHeaderActions() {
  const paired = () => !!activeSpace()?.runnerPaired;
  return (
    <>
      <Badge testID="runner-state" tone={paired() ? "runner" : "neutral"} label={paired() ? "Runner attached" : "No runner"} />
      <Action
        testID="run-demo"
        label="Run demo"
        busyLabel="Requesting…"
        accessibilityLabel={paired() ? "Run the deterministic runner demo (workspace.list)" : "Run demo (needs a runner attached to this Space)"}
        busy={demoPending()}
        disabled={!activeRoom() || !paired() || status().phase !== "online"}
        onPress={() => void runDemo()}
      />
      <If when={activeSpace()?.role === "owner"}>
        <Action testID="manage-space" label="Manage" onPress={() => void openManage()} />
      </If>
    </>
  );
}

/** One suggestion of the mention list. */
type MentionOption = { key: string; mention: Mention; title: string; detail: string; note?: string; testID: string; label: string };

/** The agent and the members (never the signed-in user) whose id or display name matches what follows "@". */
export function mentionOptions(query: string, list: readonly { userId: string; role?: string }[] | null, myId: string | null): MentionOption[] {
  const q = query.toLowerCase();
  const out: MentionOption[] = [];
  if ("agent".startsWith(q)) {
    out.push({ key: "agent", mention: { kind: "agent" }, title: "agent", detail: "starts agent work", testID: "pick:agent", label: "Mention @agent: starts agent work" });
  }
  for (const m of list ?? []) {
    if (m.userId === myId) continue;
    const name = profileName(m.userId);
    if (!m.userId.toLowerCase().includes(q) && !(name ?? "").toLowerCase().includes(q)) continue;
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

function RoomComposer() {
  /** Opened with the @ button (not by typing). */
  const explicit = signal(false);
  /** The draft the user closed the suggestions at: typing on opens them again. */
  const dismissedAt = signal<string | null>(null);
  /** The caret's box in the window, as the field last reported it. */
  const caret = signal<Bounds | null>(null);
  const highlighted = signal(0);
  /** The composer's field has the keyboard focus. */
  const fieldFocused = signal(false);
  let atButton: ViewRef | null = null;
  /** The keyboard focus is in the list (the button put it there, or Tab). */
  let focusInList = false;

  const query = () => mentionQuery(draft());
  const typing = () => query() !== null && dismissedAt() !== draft();
  const open = () => !!activeRoom() && (explicit() || typing());
  const options = computed(() => mentionOptions(query() ?? "", members(), me()));
  const active = () => Math.max(0, Math.min(highlighted(), options().length - 1));

  // Typing "@" opens the list; members load once per Space and room.
  let loadedFor = "";
  effect(() => {
    if (!open()) return;
    const key = `${activeSpace()?.id}/${activeRoom()?.id}`;
    if (loadedFor !== key) {
      loadedFor = key;
      void loadMembersForMentions();
    }
  });
  // A new query starts at the first suggestion.
  effect(() => {
    query();
    untracked(() => highlighted.set(0));
  });
  // Leaving the room (or Space) closes it, whatever the draft still says.
  const place = computed(() => `${activeSpace()?.id ?? ""}/${activeRoom()?.id ?? ""}`);
  let shownFor: string | null = null;
  effect(() => {
    const key = place();
    if (key === shownFor) return;
    shownFor = key;
    untracked(() => {
      explicit.set(false);
      caret.set(null);
      dismissedAt.set(draft());
      focusInList = false;
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
    if (!open() || !fieldFocused() || options().length === 0) return;
    const move = (step: number) => () => {
      const n = options().length;
      if (n === 0) return;
      highlighted.set((active() + step + n) % n);
      announce(`${options()[active()]!.label}, ${active() + 1} of ${n}`);
    };
    return keymap.bind(
      { down: move(1), up: move(-1), tab: () => void intercept() },
      { when: "always", repeat: true },
    );
  });
  // Typing something that is not a mention closes the list the button opened.
  effect(() => {
    draft();
    untracked(() => {
      if (explicit() && query() === null && !focusInList) explicit.set(false);
    });
  });

  const close = () => {
    focusInList = false;
    explicit.set(false);
    dismissedAt.set(draft());
    focusComposer();
  };
  const choose = (m: Mention) => {
    focusInList = false;
    addMention(m);
    explicit.set(false);
    caret.set(null);
    focusComposer();
  };
  const toggle = () => {
    if (open()) close();
    else {
      dismissedAt.set(null);
      explicit.set(true);
    }
  };
  /** Return in the field: picks the highlighted suggestion instead of sending. */
  const intercept = () => {
    if (!open() || options().length === 0) return false;
    choose(options()[active()]!.mention);
    return true;
  };
  /** Next to the caret while typing; under the button when it opened the list or no caret was reported yet. */
  const anchor = () => (typing() && !explicit() ? caret() ?? atButton : atButton);

  return (
    <Composer
      placeholder={activeRoom() ? `Message #${activeRoom()!.name}` : "Select a room"}
      agentHint="Return sends · Shift+Return adds a line · @agent or Ask agent asks this Space's agent; @name only notifies them."
      disabled={!activeRoom()}
      onSubmitIntercept={intercept}
      onCaretMove={(e) => caret.set(e.pageBounds)}
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
          <If when={mentions().length > 0}>
            <RecipientBar testID="mention-chips">
              <Each of={mentions()} key={(m) => (m.kind === "agent" ? "agent" : `user:${m.id}`)}>
                {(m) => (
                  <RecipientChip
                    testID={m().kind === "agent" ? "mention-chip:agent" : `mention-chip:${(m() as { id: string }).id}`}
                    label={chipLabel(m())}
                    agent={m().kind === "agent"}
                    accessibilityLabel={`Remove mention ${mentionLabel(m())}`}
                    onRemove={() => removeMention(m())}
                  />
                )}
              </Each>
            </RecipientBar>
          </If>
          <If when={open()}>
            <AnchoredPopover
              anchor={anchor()}
              onClose={close}
              placement="top"
              align="start"
              role="listbox"
              testID="mention-picker"
              accessibilityLabel={`Mention in #${activeRoom()?.name ?? ""}`}
            >
              <PopoverNote text={`Mention in #${activeRoom()?.name ?? ""} · members of ${activeSpace()?.name ?? "this Space"} only · ↑↓ then Return`} />
              <SuggestionList accessibilityLabel="Mention suggestions">
                <Each of={options()} key={(o) => o.key}>
                  {(o, index) => (
                    <SuggestionOption
                      testID={o().testID}
                      title={o().title}
                      detail={o().detail}
                      note={o().note}
                      active={index() === active()}
                      accessibilityLabel={o().label}
                      leading={
                        o().mention.kind === "agent" ? (
                          <AuthorGlyph kind="agent" tone="agent" size="sm" />
                        ) : (
                          <PersonAvatar userId={(o().mention as { id: string }).id} name={o().title} size="sm" />
                        )
                      }
                      ref={(node) => {
                        // The button moves the focus into the list; typing keeps it in the field.
                        if (index() === 0 && explicit() && !typing()) {
                          focusInList = true;
                          node.focus();
                        }
                      }}
                      onPress={() => choose(o().mention)}
                    />
                  )}
                </Each>
              </SuggestionList>
              <If when={membersError()}>{(message) => <PopoverNote tone="danger" text={message()} />}</If>
              <If when={members() !== null && options().length === 0}>
                <PopoverNote testID="mention-none" text={`No member matches “${query() ?? ""}”.`} />
              </If>
            </AnchoredPopover>
          </If>
        </>
      }
    />
  );
}

const mentionLabel = (m: Mention) => mentionText(m, me());
/** A chosen person by display name, else their id; the agent as "@agent". */
const chipLabel = (m: Mention) => (m.kind === "user" ? `@${profileName(m.id) ?? m.id}` : mentionLabel(m));
