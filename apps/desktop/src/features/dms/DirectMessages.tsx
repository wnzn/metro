import { cleanup, signal, Storage } from "nsh";
import type { ViewRef } from "nsh";
import type { DirectoryPerson } from "../../api/directory";
import type { DirectMessage } from "../../api/dms";
import { ApiError, connectedTo, me, sessionGuard, settingsOpen } from "../../store";
import {
  Action, Banner, Column, compactWidth, ConfirmInline, Label, NavItem, navigationPanel, Row, SectionHeader,
} from "../../ui";
import { PersonAvatar } from "../profile/PersonAvatar";
import { DirectorySearch } from "./DirectorySearch";
import { activeDm, dmHiding, dmListError, dms, dmStarting, hideDm, openDm, refreshDmList, startDm } from "./state";

const noticed = new Set<string>();
function NewDmPicker(props: { onClose: (opened?: boolean) => void }) {
  const notice = signal(false);
  const problem = signal("");
  let live = true;
  cleanup(() => { live = false; });
  const valid = sessionGuard();
  const key = `metro.directory-notice:${JSON.stringify([connectedTo(), me()])}`;
  if (!noticed.has(key)) {
    void Storage.getItem(key).catch(() => null).then(shown => {
      if (!live || !valid() || shown || noticed.has(key)) return;
      noticed.add(key);
      notice.set(true);
      void Storage.setItem(key, "shown").catch(() => {});
    });
  }
  const pick = async (person: DirectoryPerson) => {
    problem.set("");
    try {
      let opened = false;
      await startDm(person.id, () => (opened = live && !settingsOpen()));
      if (opened) {
        if (live) props.onClose(true);
        if (compactWidth()) navigationPanel.collapse();
      }
    } catch (error) {
      if (live && valid()) problem.set(error instanceof ApiError && error.status === 404
        ? "This person is no longer available." : "Couldn't open the conversation. Try again.");
    }
  };
  const close = () => props.onClose();
  return <Column gap={2} testID="new-dm-picker">
    <If when={notice()}>
      <Banner tone="neutral" variant="inline" testID="directory-notice"
        message="Everyone on this Metro server can find you by your display name and picture, and send you messages." />
    </If>
    <DirectorySearch placeholder="Search people by name" onPick={person => void pick(person)} onCancel={close} autoFocus disabled={dmStarting()} />
    <If when={problem()}><Banner tone="warning" variant="inline" message={problem()} /></If>
    <Row><Action compact variant="ghost" label="Cancel" testID="cancel-new-dm" onPress={close} onEscape={close} /></Row>
  </Column>;
}

export function DirectMessagesSection(props: { visible: boolean; inbox?: boolean }) {
  const adding = signal(false);
  let addButton: ViewRef | undefined;
  const finish = (opened?: boolean) => {
    adding.set(false);
    if (!opened) queueMicrotask(() => addButton?.focus());
  };
  return <Column gap={1} padY={2} testID={props.inbox ? "dm-inbox-list" : "dm-section"}>
    <SectionHeader label="Direct messages" action={{
      icon: adding() ? "close" : "plus", label: adding() ? "Cancel new message" : "New message",
      testID: props.inbox ? "inbox-new-dm" : "new-dm", shown: true,
      ref: node => { addButton = node; },
      onPress: () => adding() ? finish() : adding.set(true),
    }} />
    <If when={adding() && props.visible}>
      <Column padX={2}><NewDmPicker onClose={finish} /></Column>
    </If>
    <If when={dmListError() && props.inbox}>
      <Banner tone="warning" variant="inline" message={dmListError()}>
        <Row><Action compact variant="ghost" label="Retry" onPress={refreshDmList} /></Row>
      </Banner>
    </If>
    <Each of={dms()} key={dm => dm.id}>
      {dm => <DmItem dm={dm()} inbox={props.inbox} fallback={() => addButton}
        onOpen={() => {
          finish(true);
          openDm(dm());
          if (compactWidth()) navigationPanel.collapse();
        }} />}
    </Each>
  </Column>;
}

function DmItem(props: { dm: DirectMessage; inbox?: boolean; fallback: () => ViewRef | undefined; onOpen: () => void }) {
  const confirming = signal(false);
  const failure = signal("");
  const name = () => props.dm.peer.name;
  const id = () => props.dm.id;
  const busy = () => dmHiding() === id();
  const prefix = props.inbox ? "inbox-dm" : "dm";
  let menu: ViewRef | undefined;
  return <Column gap={1}>
    <NavItem testID={`${prefix}:${id()}`} label={name()}
      avatar={<PersonAvatar userId={props.dm.peer.id} name={name()} verifiedName size="sm" />}
      accessibilityLabel={props.dm.unread ? `${name()}, ${props.dm.unread} unread` : name()}
      trailing={props.dm.unread ? String(props.dm.unread) : undefined}
      selected={activeDm()?.id === id()} onPress={props.onOpen} menu={{
        accessibilityLabel: `More for ${name()}`, testID: `${prefix}-menu:${id()}`,
        ref: node => { menu = node; },
        items: [{ id: `${prefix}-hide:${id()}`, label: "Hide conversation", onPress: () => { failure.set(""); confirming.set(true); } }],
      }} />
    <If when={confirming()}>
      <ConfirmInline open={confirming()} onOpenChange={confirming.set} testID={`${prefix}-confirm-hide:${id()}`}
        returnFocus={() => menu} scopeKey={id()} confirmLabel="Hide conversation" title="Hide this conversation?"
        message={`${name()} keeps their copy. It comes back if either of you sends a message.`}
        busy={busy()} busyLabel="Hiding…" disabled={!!dmHiding() && !busy()}
        onConfirm={() => {
          failure.set("");
          void hideDm(id()).then(hidden => {
            if (hidden) requestAnimationFrame(() => props.fallback()?.focus());
          }).catch(() => failure.set("Couldn't hide the conversation. Try again."));
        }} />
      <If when={failure()}><Label color="warning" variant="caption">{failure()}</Label></If>
    </If>
  </Column>;
}
