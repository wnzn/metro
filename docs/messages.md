# Messages

Markdown, Reply, Forward, the directory and DMs are in the published
[Metro 0.6.0](released-0.6.0.md). **Delete** is in the source, not
released, planned for 0.6.1 ([source overview](next-source.md)).

## Markdown

The desktop renders messages from people and agents in Chat, Space rooms
and threads, and DMs as Markdown. The composer is still plain text: use
`**bold**`, lists, inline code or fenced code blocks. Copy keeps the
original Markdown. The renderer doesn't execute HTML or load remote
images; it shows image alt text instead. Only HTTPS links without
credentials are clickable. Verified Space mentions still show as chips
in formatted text, and as literal text inside code. Rendering is bounded
to 32 KiB and 800 nodes per message.

## Reply

In a top-level Space room, **Reply** opens a thread anchored to that
message. The same message always opens the same thread. Use the reply
count under the message to return to it. In a thread, **Reply** quotes a
message there without creating another thread. Anchored reply threads
inherit membership and the runner from the Space and don't post results
back to the parent room. They are separate from agent-created threads.

A text reply to an agent's message explicitly asks that agent even if
classification is off. Remove the **Replying to** recipient chip to keep
the quote without automatically asking the agent. Other explicit mentions
still apply; mentioning the same agent doesn't start it twice. A generic
`@agent` mention can resolve separately by the usual rules. Older replies
without agent attribution use the default agent. If the original agent
was deleted, only the sender gets a notice, and no agent substitutes for
it. Normal addressing rules still apply to other explicit recipients.

In a DM, Reply quotes a message inline. Personal Chat has Forward but
no Reply control.

## Forward

Choose **Forward** on a person's message or an agent reply. Pick another
Space room or search for a person, preview the message, and optionally
add a comment. You're sending a copy under your own name. The copy
shows the original message's stored author label; it links back only when
forwarded within the same Space. Check the destination before sending:
recipients can read the copied text even if they can't read the original.

Forward copies the text, not its mentions, attachments, reactions or
replies. Metro shows how many files were omitted. The comment goes first
as a separate message. Neither message asks an agent or runs classification.
The server rechecks source and destination access together.

## Delete

**In the source, not released (0.6.1).** Choose **Delete** in a
message's toolbar and confirm. Deleting is permanent, and there's no
Trash. You can delete:

- your own messages anywhere you can post;
- any message or agent reply in a Space you own;
- anything in your personal Chat, including Metro's replies;
- in a DM, only your own messages.

There is no admin bypass. Deleting removes the message for everyone, with
its reactions, attachment links (the Library file stays), everyone's
search index entries for it and any conversation summary that covers
it, so no later run sends it to a model. A message that anchors a reply
thread, even an empty one, leaves a **Message deleted** line so the thread
keeps its place; the deleted message is no longer part of that thread's
agent history. Runs it already started keep going; a run that hadn't
started yet (for example while System One was still deciding) never
starts. Deleting never starts or stops agent work. Copies already
forwarded, and anything already sent to a model, aren't affected.

## Direct messages and the directory

Use **Direct messages → New message** to find someone who has signed in to
the same Metro server at least once. You don't need to share a Space.
The directory shows stable IDs, display names and avatars; names aren't
used as recipient IDs. People who have never signed in and server actors
aren't listed. Email isn't exposed as a separate field, but if your sign-in
provider uses your email as your display name, that name is visible.
Your bio is visible to people who share a Space or a DM with you.

Only the two participants can read a DM through Metro; there is no admin
bypass. DMs support text, quotes, reactions and forwarding, but have no
agents, attachments or structured mentions. They are not included in
search, the Library or Metro in Chat.

**Hide conversation** hides the current history for you only. A new
message brings back the conversation, not the history you hid. Once both
people have hidden all its messages, the conversation and history are
purged. Hiding doesn't block a person from contacting you.

## Messaging API

All routes take a signed-in person's bearer token. Missing or out-of-scope
conversations and messages generally return `404`.

### Reply threads

- `POST /v1/spaces/{sid}/rooms/{rid}/events/{eid}/thread` with `{}` returns
  a `Room`: `201` if created, `200` if it already exists. `rid` must be a
  top-level room, and `eid` a `message`, `agent` or `thread` event there.
  Nested or unsupported anchors and exhausted capacity return `409`.
  The default limit is 5,000 reply threads per Space, separate from room
  and agent-thread limits.
- Reply thread rooms have `anchor_event_id` and `parent_room_id`. On the
  anchor in the parent room's event feed, `replies` gives `room_id`,
  `count`, `last_at` when available, and up to three `participant_ids`.
- `POST /v1/spaces/{sid}/rooms/{rid}/messages` additionally accepts
  `reply_to` (a same-thread event ID or its anchor) and `reply_to_agent`
  (defaults to `true`). Invalid targets or `reply_to` in a top-level room
  give `400`. Events expose `reply_to` at top level. An automatically
  addressed recipient is stored as an agent mention with
  `addressed_by: "reply"`; a missing agent can set a private
  `addressing_note.kind: "reply_agent_unavailable"` on the POST response.
  Messages sent by Metro expose `via: "metro"` at top level.

### Deleting messages

**In the source, not released (0.6.1).**

| Request | Who may delete |
| --- | --- |
| `DELETE /v1/spaces/{sid}/rooms/{rid}/events/{eid}` | The sender; the Space owner for any message or agent reply in the Space |
| `DELETE /v1/chat/events/{eid}` | The Chat's owner, for any message or reply in it |
| `DELETE /v1/dms/{id}/events/{eid}` | The sender only |

Success returns `200` with `{deleted: true, events}`, the
`message_deleted` events it wrote. A conversation or message the caller
can't read, or an unknown ID, gives `404`; a reader who may not delete it
gets `403`; anything other than a `message` or `agent` event gives `409`.
A `message_deleted` event arrives through the normal `after` poll; its
`text` is JSON `{target_event_id, tombstone}`. With `tombstone: true`,
the target stays as a `kind: "deleted"` event with no text, recipients
or metadata, anchoring its reply thread. A deletion also removes the
`compaction` events that cover it in its own room and in the reply thread
anchored on it, each with its own `message_deleted` event. DM polls
include deletions only with `GET /v1/dms/{id}/events?after=&deletions=1`,
so older clients keep receiving messages only.

### Forward

`POST /v1/forward` takes `source`, `destination` and optional `comment`.
Choose the relevant fields:

| `kind` | Source fields | Destination fields |
| --- | --- | --- |
| `space` | `space_id`, `room_id`, `event_id` | `space_id`, `room_id` |
| `dm` | `dm_id`, `event_id` | `user_id` |
| `chat` | `event_id` | Not supported |

The desktop picker offers top-level rooms; the API also accepts Space
thread destinations. Forwarding to yourself or the same Space room is
refused. A DM destination gets or creates the conversation. On success
Metro returns `201` with `{events, omitted_attachments}`. The copy has a
top-level `forwarded` object with `author_name`, `author_kind` (`person`
or `agent`), `sent_at` (or `null`), and optional
`same_space: {room_id, event_id}`.
An inaccessible or unknown source or destination returns `404` with
`forward_source_not_found` or `forward_destination_not_found`, respectively.

### Directory and DMs

`GET /v1/directory?q=&cursor=&limit=` returns
`{users: [{id, display_name, avatar_version}], next_cursor?}` with
`Cache-Control: no-store`. The caller isn't listed. Search accepts the
beginning of a display name or a word in it, or an exact stable ID.
`q` accepts up to 64 printable characters; `limit` defaults to 20 and is
clamped to 1–25. Pass the returned cursor to continue the same search.
Directory requests are rate-limited per user (`429`).

`GET /v1/profiles/{id}` and `GET /v1/profiles/{id}/avatar` also allow
access to directory-listed users. The profile's `bio` is `null` for
callers who aren't the person or a current Space or DM peer.

| Request | Result |
| --- | --- |
| `GET /v1/dms` | `{dms}` with up to 200 conversations, newest first. Each has `id`, `peer` (directory card), `last` when visible and `unread` (up to 99). |
| `POST /v1/dms` with `{user_id}` | Gets (`200`) or creates (`201`) a conversation with a listed user without sending a message. Self gives `400`; an unknown or unlisted user gives `404`. |
| `GET /v1/dms/{id}/events?after=` | `{events, reactions}`; only events newer than `after` and the caller's hide point. |
| `POST /v1/dms/{id}/messages` with `{text, reply_to?}` | `201` with the message event. Optional quote target must be visible in that DM. |
| `PUT /v1/dms/{id}/read` with `{last_read}` | `204`. Advances the caller's read point to a visible event ID; older positions don't move it back. |
| `DELETE /v1/dms/{id}` | `204`; hides existing history for the caller, purging the conversation when both have hidden all events. |
| `PUT` or `DELETE /v1/dms/{id}/events/{eid}/reactions` with `{emoji}` | Adds or removes the caller's reaction on a visible event and returns `{changed}`. |
