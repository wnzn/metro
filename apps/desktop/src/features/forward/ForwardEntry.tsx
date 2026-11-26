import { announce, effect, signal, untracked } from "nsh";
import type { ViewRef } from "nsh";
import type { DirectoryPerson } from "../../api/directory";
import type { ForwardDestination } from "../../api/dms";
import { ApiError, apiClient, readHostedAccess, spaces } from "../../store";
import type { Room } from "../../store";
import { Action, Banner, Column, Field, FormRow, Label, Modal, Picker, Row, ScrollArea, SegmentedControl } from "../../ui";
import { sidePanelFits } from "../../ui/activity";
import { DirectorySearch } from "../dms/DirectorySearch";
import { refreshDmList } from "../dms/state";
import { MentionedBody } from "../mentions/MentionedBody";
import { closeForward, forwardSelection } from "./state";

export function ForwardEntry() {
  effect(() => {
    const selection = forwardSelection();
    if (selection && !selection.current()) forwardSelection.set(null);
  });
  return (
    <Modal testID="forward-dialog" title="Forward message" open={!!forwardSelection()?.current()}
      onClose={closeForward} size={sidePanelFits() ? "form" : "full-height"}>
      <If when={forwardSelection()?.current() ? forwardSelection() : null}>
        {selection => <ForwardForm selection={selection()} />}
      </If>
    </Modal>
  );
}

type Selection = NonNullable<ReturnType<typeof forwardSelection>>;
const groups = [
  { value: "rooms", label: "Rooms" },
  { value: "threads", label: "Threads" },
  { value: "people", label: "People" },
] as const;
const fileCount = (n: number) => `${n} ${n === 1 ? "file" : "files"}`;

function ForwardForm(props: { selection: Selection }) {
  const group = signal<(typeof groups)[number]["value"]>("rooms");
  const spaceId = signal<string | null>(null);
  const roomId = signal<string | null>(null);
  const person = signal<DirectoryPerson | null>(null);
  const rooms = signal<Room[]>([]);
  const comment = signal("");
  const busy = signal(false);
  const loading = signal(false);
  const loadError = signal(false);
  const reload = signal(0);
  const error = signal("");
  const sourceLost = signal(false);
  let cancelControl: ViewRef | undefined;
  const activeSpace = () => spaces().find(s => s.id === spaceId());
  const current = (selection: Selection) => forwardSelection() === selection && selection.current();
  effect(() => {
    const section = group();
    const sid = activeSpace()?.id;
    reload();
    rooms.set([]);
    roomId.set(null);
    loadError.set(false);
    loading.set(false);
    const api = apiClient();
    if (!api || !sid || section === "people") return;
    const selection = props.selection;
    const run = new AbortController();
    const alive = () => !run.signal.aborted && current(selection);
    loading.set(true);
    void readHostedAccess(api, () => api.listRooms(sid, section === "rooms", run.signal)).then(list => {
      if (!alive()) return;
      const source = selection.source;
      rooms.set(list.filter(r => (section === "threads" ? !!r.parentRoomId : !r.parentRoomId)
        && !(source.kind === "space" && source.space_id === sid && source.room_id === r.id)));
    }).catch(() => { if (alive()) loadError.set(true); })
      .finally(() => { if (alive()) loading.set(false); });
    return () => run.abort();
  });
  const destination = (): { wire: ForwardDestination; name: string } | null => {
    if (group() === "people") {
      const selected = person();
      return selected ? { wire: { kind: "dm", user_id: selected.id }, name: selected.name } : null;
    }
    const space = activeSpace(), room = rooms().find(r => r.id === roomId());
    return space && room ? { wire: { kind: "space", space_id: space.id, room_id: room.id },
      name: `${space.name} › ${room.anchorEventId === undefined ? room.name : "Thread"}` } : null;
  };
  const submit = async () => {
    const api = apiClient(), target = destination(), selection = props.selection;
    if (!api || !target || !current(selection) || busy() || sourceLost()) return;
    busy.set(true);
    error.set("");
    try {
      const result = await readHostedAccess(api, () => api.forwardMessage(selection.source, target.wire, comment().trim() || undefined));
      if (apiClient() !== api) return;
      if (target.wire.kind === "dm") refreshDmList();
      announce(`Forwarded to ${target.name}${result.omittedAttachments ? `. ${fileCount(result.omittedAttachments)} ${
        result.omittedAttachments === 1 ? "wasn't" : "weren't"} forwarded.` : ""}`);
      if (current(selection)) closeForward();
    } catch (failure) {
      if (!current(selection)) return;
      const code = failure instanceof ApiError ? (failure.details as { code?: string } | undefined)?.code : undefined;
      if (code === "forward_source_not_found") {
        sourceLost.set(true);
        error.set("You can no longer read this message.");
      } else if (code === "forward_destination_not_found") {
        roomId.set(null);
        person.set(null);
        reload.set(untracked(reload) + 1);
        error.set("You can't post there anymore.");
      } else error.set("Couldn't forward. Try again.");
      // The button that launched the request was disabled while it worked.
      // Keep the keyboard inside the dialog even after a permanent refusal.
      queueMicrotask(() => { if (current(selection)) cancelControl?.focus(); });
    } finally {
      if (current(selection)) busy.set(false);
    }
  };
  const attachments = () => props.selection.event.attachments?.length ?? 0;
  const previewKey = `forward:${JSON.stringify(props.selection.source)}`;
  return <>
    <Column padX={4} padY={3}><Label variant="heading" heading>Forward message</Label></Column>
    <ScrollArea padX={4} padY={1} gap={3}>
      <If when={!sourceLost()}>
        <FormRow label="Preview">
          <MentionedBody event={props.selection.event} pieces={[]} stateKey={previewKey} />
          <If when={attachments()}>
            <Label color="muted" variant="caption" testID="forward-omitted">{`${fileCount(attachments())} won't be forwarded.`}</Label>
          </If>
        </FormRow>
      </If>
      <FormRow label="Destination">
        <SegmentedControl accessibilityLabel="Destination type" testID="forward-target" options={groups} value={group()}
          disabled={busy() || sourceLost()} onChange={value => {
            group.set(value);
            person.set(null);
            error.set("");
          }} />
        <If when={group() === "people"} else={
          <>
            <Picker testID="forward-space" accessibilityLabel="Destination Space" placeholder="Choose a Space"
              items={spaces().map(s => ({ value: s.id, label: s.name }))} value={activeSpace()?.id ?? null}
              disabled={busy() || sourceLost() || !spaces().length} onChange={value => { spaceId.set(value); error.set(""); }} />
            <If when={!spaces().length}><Label color="muted" variant="caption">No Spaces available</Label></If>
            <If when={activeSpace()}>
              <Picker testID="forward-room" accessibilityLabel={group() === "threads" ? "Destination thread" : "Destination room"}
                placeholder={group() === "threads" ? "Choose a thread" : "Choose a room"}
                items={rooms().map(r => ({ value: r.id, label: r.anchorEventId === undefined ? r.name : `Reply thread · ${r.anchorEventId}` }))}
                value={roomId()} disabled={busy() || sourceLost() || !rooms().length}
                onChange={value => { roomId.set(value); error.set(""); }} searchable />
              <If when={loading()}><Label color="muted" variant="caption">Loading destinations…</Label></If>
              <If when={loadError()}>
                <Label color="warning" variant="caption">Couldn't load destinations.</Label>
                <Row><Action compact variant="ghost" label="Retry rooms" onPress={() => reload.set(untracked(reload) + 1)} /></Row>
              </If>
              <If when={!rooms().length && !loading() && !loadError()}>
                <Label color="muted" variant="caption">{group() === "threads" ? "No other threads" : "No other rooms"}</Label>
              </If>
            </If>
          </>
        }>
          <DirectorySearch placeholder="Search people…" selectedId={person()?.id} disabled={busy() || sourceLost()} onPick={p => {
            person.set(p);
            error.set("");
          }} />
        </If>
      </FormRow>
      <Field testID="forward-comment" value={comment()} onChange={comment.set} placeholder="Add a comment (optional)"
        multiline disabled={busy() || sourceLost()} />
      <If when={error()}><Banner tone="warning" variant="inline" testID="forward-error" message={error()} /></If>
    </ScrollArea>
    <Column padX={4} padY={3} gap={1}>
      <If when={destination()}>{target => <Label variant="caption" color="muted" lines={1}>{`To ${target().name}`}</Label>}</If>
      <Row gap={2} wrap justify="end">
        <Action testID="forward-cancel" label={busy() ? "Close" : "Cancel"} variant="ghost" onPress={closeForward}
          onEscape={closeForward} viewRef={node => { cancelControl = node; }} />
        <Action testID="forward-submit" label="Forward" variant="primary" busyLabel="Forwarding…" busy={busy()}
          disabled={!destination() || sourceLost()} onPress={() => void submit()} />
      </Row>
    </Column>
  </>;
}
