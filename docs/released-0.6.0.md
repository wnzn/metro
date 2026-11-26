# New in Metro 0.6.0

These changes are included in the published
[Metro 0.6.0 release](https://github.com/wnzn/metro/releases/tag/v0.6.0).
See [Acceptance](acceptance.md#published-metro-060-2026-11-26-github-release-v060)
for the release files and image digests, and [New in Metro 0.5.1](released-0.5.1.md)
and [New in Metro 0.5.0](released-0.5.0.md) for earlier features.

## Metro in Chat

**Settings → Metro in Chat → Use my Spaces in Chat** is on by default
for each account. The desktop records that it showed the notice in Chat;
the tools are unavailable until that succeeds. You don't have to click
**Got it** to activate them. Turn the switch off or uncheck individual
Spaces in Settings. Metro acts as you, checking access when it uses a
tool. Leaving a Space or excluding it prevents further tool access to it;
it doesn't erase anything already sent to a model or recorded in Chat.
Space content Metro reads goes to the model answering your Chat turn.
If someone shares that connection with you, its owner can see those
requests and may pay for them. Web tools still require their own opt-in.

You can ask Metro to catch you up across Spaces, find messages, or look
for mentions and replies that may be waiting for you. “Waiting” means
you haven't posted a later message in that room or thread; it's not a
read receipt. Search can use your semantic search setup or return literal
matches. It only calls results reranked when System One ranked them.
Metro can also list, stop or retry eligible Space runs; list, create,
pause or resume your scheduled prompts; and find Pages or propose
drafts. You still review and publish or apply Page drafts yourself.
Metro can explain your settings, not change them. These tools don't read
DMs and aren't available in personal Page conversations.

To delegate, ask Metro to post in a named Space and say which room or
thread, and which agent if not the default. Metro posts directly, without
a separate confirmation, under your name with a **via Metro** label.
The Space name or ID must appear in your current prompt or a recent
message of your own in Chat (up to five available earlier messages).
Names found in tool results don't count, and a scheduled prompt Metro
creates for you may name only Spaces you named. Metro addresses exactly
one agent by ID, without classification: the agent you named, else the
agent whose message it replies to in a thread, else the Space's default
agent. Typing `@` into the post doesn't add a recipient by itself. Your
usual Space model choices apply. Metro
can wait up to two minutes and summarize the answer; it may return
sooner if the run is still going or no wait slot is available.

Expand **Metro sent a message to …** in your Chat to see the exact message,
destination and addressed agent. This receipt is private to your Chat;
the actual message is visible to members of the destination Space.
The receipt isn't search or model history; **Open** needs current access.
Space tool activity shows summaries, not the retrieved message bodies.

A Chat turn can make up to 16 Metro tool calls, including at most three
sends and three searches, and wait for one delegated run at a time.
Metro caps tool output at 96 KiB total per turn and 64 KiB per result
with the default limits.
**Stop** in the Chat composer stops that turn, not any separate Space
run it started. In **Agent runs**, **Retry** starts a new run on the same
message with your current routing, without sending the message again.
Only the initiator can retry eligible failed or cancelled runs; the
original message and agent must still exist. The initiator or Space owner
can stop a Space run. The runs list is a bounded, in-memory view, empty
after a server restart; room transcripts persist.

## Space agents and messages

- [Space agent behaviour](spaces.md#space-agent-behaviour) covers default
  instructions, the context block, and replies, reactions and no reply.
- The [opt-in classifier](system-one.md#opt-in-classification-of-unaddressed-messages)
  leans toward a useful agent run. Unaddressed classification uses brief
  room context and a threshold that defaults to 35%, down from 85%.
- [Messages](messages.md) covers Markdown, replies that address agents,
  forwarding, the server-wide directory and DMs.

## Conversation summaries

Compaction summarizes older messages for the model without changing
the saved transcript. It works in Chat and separately in each Space room
or thread, never in DMs or Page conversations. A reply thread's anchor
stays verbatim in its model history. Later runs use the latest applicable
summary as model-written background, not instructions, alongside more
recent messages. Turning off auto summaries doesn't discard existing ones.

Under **Settings → Model choices → Conversation summaries**, **Summarize
automatically** is on by default for runs you start. The threshold is 70%
of the estimated input budget, adjustable from 40% to 90%. Auto compaction
can also run when the number of messages exceeds the history window.
The estimate uses the model's context size minus an output reserve, not
an exact provider token count. Owners can set a model's **Context size…**
under [Model connections](settings.md#each-connections-model-list).
Unknown models use a 32,000-token estimate.

Type `/compact` or `/compact open decisions` at the start of a draft and
submit it. The optional focus is limited to 200 printable characters.
This writes a summary, not a chat message, and doesn't start an agent run.
You need at least six new nonempty conversation events since the last
summary and no active reply or summary in that conversation. Commands
can't include files. An unknown command keeps your draft; start with `//`
to send a literal leading slash instead. In DMs, `/` is ordinary text.

Expand **Earlier messages summarized** to read and copy the summary and
see who triggered it, when, and any focus text. Anyone who can read that
conversation can see the summary and focus, so don't enter private focus
text in a Space. The text is model-written and may be incomplete or wrong.
Markers aren't searchable, and you can't reply to, forward or react to them.

Automatic compaction makes a separate, tool-free call on the run's
resolved model. Manual compaction uses your default Chat route in Chat
or your current route for the Space's default agent in a Space. A shared
connection's owner can see the summarization request. Metro doesn't switch
models if the chosen connection fails; the normal server-model option can
still be selected initially if you have no default Chat connection. An
automatic failure leaves the run answering from its available history:
the Space sees a generic notice, with the detail visible only to the
initiator; in private Chat you see the detail there. A manual failure
returns to you and doesn't save a marker.

## New API routes and fields

Use the signed-in person's bearer token. For directory, DMs, forwarding
and reply threads, see the [messaging API](messages.md#messaging-api).
The classifier and its threshold use the existing
[model-routing API](system-one.md#settings-get--put-v1model-routing).

Metro's model tools aren't public HTTP endpoints: `spaces_list`,
`space_read`, `spaces_catch_up`, `spaces_search`, `waiting_on_me`,
`space_send`, `runs_list`, `run_cancel`, `run_retry`, `schedules_list`,
`schedule_create`, `schedule_set_enabled`, `pages_find`, `page_propose`
and `settings_explain`.

| Request | Result |
| --- | --- |
| `GET /v1/chat/spaces-access` | `{enabled, notice_shown, excluded_space_ids}`; initially `true`, `false`, `[]`. |
| `PUT /v1/chat/spaces-access` | Returns the same shape. All fields are optional. Send `notice_shown: true` only after showing the disclosure; `false` clears it. `excluded_space_ids` replaces the list (up to 500); a non-member or unknown Space ID gives `404`. |
| `POST /v1/chat/turns/{id}/stop` with `{}` | `{turn_id, status: "cancelled"}` for your running turn; `404` otherwise. Use the turn's activity ID, not a message event ID. |
| `POST /v1/runs/{id}/retry` with `{}` | `201` with the new run. Unknown run or non-member: `404`; not the initiator: `403`; ineligible or cannot start: `409`. This isn't idempotent. |
| `GET /v1/runs` and `GET /v1/runs/{id}` | Run views include `kind: "room"`, `started_at` (RFC 3339 UTC), and nullable `agent_id` and `trigger_event_id`. Private `failure` and `compaction_failure` details are initiator-only. |
| `GET /v1/compaction` | `{auto, at_percent, min_percent, max_percent, default_context_tokens}`. |
| `PUT /v1/compaction` | Accepts optional `auto` and `at_percent` (integer 40–90) and returns the same view; unknown fields and out-of-range values are refused. |
| `POST /v1/chat/compact` or `POST /v1/spaces/{sid}/rooms/{rid}/compact` | Body `{focus?}` or empty. Returns `201` with the `compaction` event. Space access required (`404` otherwise). `409` codes: `reply_running`, `nothing_new`, `compaction_running`. `502 summary_failed` gives the caller the failure reason. |
| `GET`, `POST`, `PATCH /v1/providers/{id}/model-list` | Model rows include `context_tokens`. Owners can set it on add or update, or clear it with `null` on update. See [Model list](settings.md#each-connections-model-list) for the range. |

The transcript APIs use `kind: "space_post"` in Chat for a private
receipt. `text` is the sent message and `activity.space_post` names the
destination (`space_id`, `space_name`, `room_id`, `room_name`, optionally
`thread_name` and `parent_room_id`), `event_id`, `agent` (`id`, `handle`,
`name`) and optional `run_id`. Room run completion activity can carry
`outcome: "reply"`, `"reaction"` or `"no_reply"`.

A `kind: "compaction"` event has the summary in `text` and a top-level
`compaction` object: `from_event_id`, `through_event_id`, `events`,
`trigger` (`"auto"` or `"manual"`), `by` (user ID), `by_name`, and optional
`focus` and `previous` (an older summary event ID). A failed automatic
summary can set `compaction_failure: {message}` on the initiator's run view.

Later changes are [in the source, not released](next-source.md).
