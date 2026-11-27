// Threads of the personal Chat: listed under Chat in the navigation, each
// its own conversation with Metro. Only their owner sees them. Renaming and
// deleting go through the dialogs here; deleting is permanent.

import { signal } from "nsh";
import {
  activeChatThreadId,
  chatThreadBusy,
  chatThreads,
  chatThreadsSupported,
  createChatThread,
  deleteChatThread,
  mainPlace,
  openChatThread,
  renameChatThread,
} from "../../store";
import type { ChatThread } from "../../store";
import { Action, ActionBar, Column, Field, Label, Modal, NavGroup, NavItem, Row } from "../../ui";
import type { ActionsMenuItem } from "../../ui";

/** The longest thread title the server takes. */
export const MAX_THREAD_TITLE = 80;

/** The thread being renamed, and the one waiting for a confirmed Delete. */
const renaming = signal<ChatThread | null>(null);
const renameTitle = signal("");
const deleting = signal<ChatThread | null>(null);

export const openRenameThread = (thread: ChatThread) => {
  renameTitle.set(thread.title);
  renaming.set(thread);
};
export const openDeleteThread = (thread: ChatThread) => deleting.set(thread);

/** A thread's own actions, for its row and the Chat's header. */
export function threadMenuItems(thread: ChatThread, prefix: string): ActionsMenuItem[] {
  return [
    { id: `${prefix}-rename:${thread.id}`, label: "Rename", onPress: () => openRenameThread(thread) },
    { id: `${prefix}-delete:${thread.id}`, label: "Delete", destructive: true, onPress: () => openDeleteThread(thread) },
  ];
}

/** The Chat's threads under its navigation row, while the Chat is open. */
export function ChatThreadsNav() {
  const adding = signal(false);
  const title = signal("");
  const submit = async () => {
    if (await createChatThread(title())) {
      title.set("");
      adding.set(false);
    }
  };
  return (
    <If when={mainPlace() === "chat" && chatThreadsSupported()}>
      <NavGroup testID="chat-threads">
        <Each of={chatThreads()} key={(t) => t.id}>
          {(t) => (
            <NavItem
              testID={`chat-thread:${t().id}`}
              label={t().title}
              icon="thread"
              depth={1}
              selected={activeChatThreadId() === t().id}
              onPress={() => openChatThread(t().id)}
              menu={{ testID: `chat-thread-menu:${t().id}`, accessibilityLabel: `More for ${t().title}`, items: threadMenuItems(t(), "chat-thread") }}
            />
          )}
        </Each>
        <If
          when={adding()}
          else={<NavItem testID="new-chat-thread" label="New thread" icon="plus" depth={1} selected={false} onPress={() => adding.set(true)} />}
        >
          <Column padX={2} padY={1} gap={2}>
            <Field testID="new-chat-thread-title" accessibilityLabel="New thread title" value={title()} onChange={title.set}
              placeholder="Thread title" autoFocus maxLength={MAX_THREAD_TITLE} onSubmit={() => void submit()} onCancel={() => adding.set(false)} />
            <Row gap={2}>
              <Action testID="create-chat-thread" label="Create thread" busyLabel="Creating…" busy={chatThreadBusy()}
                disabled={!title().trim()} variant="primary" onPress={() => void submit()} />
              <Action label="Cancel" variant="ghost" onPress={() => adding.set(false)} />
            </Row>
          </Column>
        </If>
      </NavGroup>
    </If>
  );
}

/** Rename and Delete, wherever they were opened from. */
export function ChatThreadDialogs() {
  const title = renameTitle;
  return (
    <>
      <If when={renaming()}>
        {(t) => {
          const save = async () => {
            if (await renameChatThread(t().id, title())) renaming.set(null);
          };
          return (
            <Modal open size="compact" title="Rename thread" testID="rename-chat-thread" onClose={() => renaming.set(null)}>
              <Column pad={4} gap={3}>
                <Label heading variant="title">Rename thread</Label>
                <Field testID="rename-chat-thread-title" accessibilityLabel="Thread title" value={title()} onChange={title.set}
                  autoFocus maxLength={MAX_THREAD_TITLE} onSubmit={() => void save()} />
                <ActionBar>
                  <Action label="Cancel" testID="rename-chat-thread-cancel" onPress={() => renaming.set(null)} />
                  <Action label="Rename" testID="rename-chat-thread-save" variant="primary" busy={chatThreadBusy()} busyLabel="Saving…"
                    disabled={!title().trim() || title().trim() === t().title} onPress={() => void save()} />
                </ActionBar>
              </Column>
            </Modal>
          );
        }}
      </If>
      <If when={deleting()}>
        {(t) => (
          <Modal open size="compact" title="Delete thread" testID="delete-chat-thread" onClose={() => deleting.set(null)}>
            <Column pad={4} gap={3}>
              <Label heading variant="title">{`Delete "${t().title}"?`}</Label>
              <Label>Its messages, replies and summaries are removed, and Metro won't see them again. This can't be undone.</Label>
              <ActionBar>
                <Action label="Cancel" testID="delete-chat-thread-cancel" onPress={() => deleting.set(null)} />
                <Action label="Delete thread" testID="delete-chat-thread-confirm" variant="destructive" busy={chatThreadBusy()} busyLabel="Deleting…"
                  onPress={() => void deleteChatThread(t().id).then(done => { if (done) deleting.set(null); })} />
              </ActionBar>
            </Column>
          </Modal>
        )}
      </If>
    </>
  );
}
