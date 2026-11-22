import { signal } from "nsh";
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
  status,
} from "../store";
import type { Mention } from "../store";
import { Action, Badge, Chip, Column, Label, Row, Surface } from "../ui";
import { Composer, ConnectionBanner, Transcript } from "./Conversation";
import { mentionText } from "./eventKinds";

export function SpaceView() {
  return (
    <Surface variant="panel" grow testID="space-view">
      <RoomHeader />
      <ConnectionBanner />
      <Transcript emptyText="No events yet. Say hello, mention @agent or someone, or run the demo." />
      <RoomComposer />
    </Surface>
  );
}

function RoomHeader() {
  const parent = () => {
    const id = activeRoom()?.parentRoomId;
    return id ? rooms().find((r) => r.id === id) ?? null : null;
  };
  const paired = () => !!activeSpace()?.runnerPaired;
  return (
    <Surface variant="toolbar" padX={4} padY={3} gap={3}>
      <Column grow gap={1}>
        <Row align="center" gap={2}>
          <Label variant="caption" color="muted" lines={1}>
            {activeSpace()?.name ?? ""}
          </Label>
          <Label variant="caption" color="muted">
            ›
          </Label>
          <Label variant="heading" lines={1} testID="room-title">
            {activeRoom() ? `# ${activeRoom()!.name}` : "No room selected"}
          </Label>
        </Row>
        <If
          when={parent()}
          else={
            <Label variant="caption" color="muted" lines={1}>
              Members of this Space share its rooms and its agent.
            </Label>
          }
        >
          {(p) => (
            <Row align="center" gap={2}>
              <Label variant="caption" color="muted">
                Thread delegated from
              </Label>
              <Action testID="open-parent" label={`# ${p().name}`} variant="ghost" onPress={() => selectRoom(p().id)} />
            </Row>
          )}
        </If>
      </Column>
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
    </Surface>
  );
}

function RoomComposer() {
  const pickerOpen = signal(false);
  const query = () => mentionQuery(draft());
  const showPicker = () => pickerOpen() || query() !== null;
  const toggle = () => {
    const open = !pickerOpen();
    pickerOpen.set(open);
    if (open) void loadMembersForMentions();
  };
  // Typing "@" opens the picker; members load once per Space and room.
  let loadedFor = "";
  const ensureMembers = () => {
    const key = `${activeSpace()?.id}/${activeRoom()?.id}`;
    if (loadedFor !== key) {
      loadedFor = key;
      void loadMembersForMentions();
    }
  };
  const choose = (m: Mention) => {
    addMention(m);
    pickerOpen.set(false);
  };
  return (
    <Composer
      placeholder={activeRoom() ? `Message #${activeRoom()!.name} — @ to mention, Return to send` : "Select a room"}
      agentHint="Ask agent or @agent asks this Space's agent. Mentioning a person only notifies them."
      disabled={!activeRoom()}
      tools={<Action testID="mention" label="@" accessibilityLabel="Mention the agent or a member" variant="ghost" onPress={toggle} />}
      above={
        <Column gap={2}>
          <If when={mentions().length > 0}>
            <Row gap={2} wrap testID="mention-chips">
              <Label variant="caption" color="muted">
                To:
              </Label>
              <Each of={mentions()} key={(m) => (m.kind === "agent" ? "agent" : `user:${m.id}`)}>
                {(m) => (
                  <Chip
                    testID={m().kind === "agent" ? "mention-chip:agent" : `mention-chip:${(m() as { id: string }).id}`}
                    label={`${mentionLabel(m())}  ×`}
                    accessibilityLabel={`Remove mention ${mentionLabel(m())}`}
                    selected
                    onPress={() => removeMention(m())}
                  />
                )}
              </Each>
            </Row>
          </If>
          <If when={showPicker()}>
            {(() => {
              ensureMembers();
              return <MentionPicker query={query() ?? ""} onChoose={choose} />;
            })()}
          </If>
        </Column>
      }
    />
  );
}

const mentionLabel = (m: Mention) => mentionText(m, me());

function MentionPicker(props: { query: string; onChoose: (m: Mention) => void }) {
  const q = () => props.query.toLowerCase();
  const people = () => (members() ?? []).filter((m) => m.userId !== me() && m.userId.toLowerCase().includes(q()));
  const agentMatches = () => "agent".startsWith(q());
  return (
    <Surface variant="card" pad={2} gap={2} testID="mention-picker">
      <Label variant="caption" color="muted">
        Mention in #{activeRoom()?.name ?? ""} (members of {activeSpace()?.name ?? "this Space"} only)
      </Label>
      <Row gap={2} wrap>
        <If when={agentMatches()}>
          <Chip testID="pick:agent" label="@agent · starts agent work" selected={false} onPress={() => props.onChoose({ kind: "agent" })} />
        </If>
        <Each of={people()} key={(m) => m.userId}>
          {(m) => (
            <Chip
              testID={`pick:${m().userId}`}
              label={`@${m().userId}${m().role === "owner" ? " · owner" : ""}`}
              selected={false}
              onPress={() => props.onChoose({ kind: "user", id: m().userId })}
            />
          )}
        </Each>
      </Row>
      <If when={membersError()}>
        {(message) => (
          <Label variant="caption" color="danger">
            {message()}
          </Label>
        )}
      </If>
      <If when={members() !== null && people().length === 0 && !agentMatches()}>
        <Label variant="caption" color="muted">
          No member matches “{props.query}”.
        </Label>
      </If>
    </Surface>
  );
}
