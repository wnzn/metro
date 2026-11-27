# New in Metro 0.6.1

These changes are included in the published
[Metro 0.6.1 release](https://github.com/wnzn/metro/releases/tag/v0.6.1).
See [Acceptance](acceptance.md#published-metro-061-2026-11-27-github-release-v061)
for the release files and image digests, and [New in Metro 0.6.0](released-0.6.0.md)
for earlier features.

## Changes

- **Delete messages.** Delete your own messages, any message or agent
  reply in a Space you own, anything in your personal Chat, and your own
  DM messages. It's permanent and removes reactions, attachment links,
  search entries and covering summaries; a message with a reply thread
  leaves a **Message deleted** line and its thread. See [Delete](messages.md#delete).
- **Messages appear before System One decides.** The desktop stores and
  shows a Space message at once, then runs opt-in classification, the
  `@agent` pick and use-case routing in the background. In local tests
  with a scripted System One that takes 1,000 ms, the message was visible
  to another member 1–2 ms after the POST instead of about 1,022 ms.
  Private notices still reach only the sender. Pending decisions, and the
  runs a decision planned, are saved and claimed, so a restart still
  makes and starts them, once; a message deleted meanwhile reaches no
  model and starts nothing. A turn with a chosen model defers too. A classifier's pick is no longer added
  to the message's recipients or labelled *Picked by classifier* or
  *detected by System One*; the run shows as that agent's. Personal Chat
  now routes and starts its turn after the message is stored too, in send
  order, and **Stop** works while it waits, across a restart.
  See [classification](system-one.md#decisions-after-posting).
- **Quieter transcript.** The composer no longer shows *Markdown
  supported*. A run that ends without an agent message shows nothing in
  the transcript.
- **Runner wording.** Runner presence reads **Connected** or
  **Disconnected** in Settings.
- **Runner tests.** The runner's install-lock tests no longer flake. A
  new server test drives the desktop's **Use locally** paths end to end
  (per-Space local tools and an installed runner answering **Test
  runner**, before and after a restart); it passed on Linux and as a
  Windows build under Wine, and in CI on native Windows (`windows-2025`)
  in the package smoke job ([record](acceptance.md#source-only-metro-061-lanes-2026-11-27-local)).
  A **Test runner** timeout reported on one Windows computer still
  hasn't been reproduced: native CI with a fresh profile passes, so it
  remains environment-specific and unverified, and this doesn't claim to
  fix it. Such failures now report a reason (below).
- **Local runner failures say why.** If the desktop's local tools or
  this computer's runner can't start (for example, its folder can't be
  opened or a pairing can't be read), Space settings shows the reason
  instead of nothing, and Metro tries again at the next sign-in. A
  worker that stops unexpectedly restarts after 1 second, doubling up to
  a minute.
- **"Not picked up" says why.** When no runner takes a job, the room
  says whether the Space has no runner, its runner isn't connected (with
  when it last checked in), or it's connected but didn't take the job in
  time.

## New API routes and fields

Use the signed-in person's bearer token.

| Request | Result |
| --- | --- |
| `DELETE /v1/spaces/{sid}/rooms/{rid}/events/{eid}`, `DELETE /v1/chat/events/{eid}`, `DELETE /v1/dms/{id}/events/{eid}` | `200` with `{deleted: true, events}`; `404`, `403` or `409`. See the [delete API](messages.md#deleting-messages). |
| `POST /v1/spaces/{sid}/rooms/{rid}/messages` with `defer_decisions: true` | Returns the stored message at once, with `decisions_pending: true` when a System One decision follows. |
| `POST /v1/spaces/{sid}/rooms/{rid}/messages/{eid}/decisions` with `{}` | Sender only. `200` with the event and any `addressing_note` or `routing_note`, once; `202 {decisions_pending: true}` if still deciding after 90 seconds; `404` otherwise. See [decisions after posting](system-one.md#decisions-after-posting). |
| `GET /v1/dms/{id}/events?after=&deletions=1` | Also returns `message_deleted` events. |
| `POST /v1/chat/messages` | The response adds `turn_id` when the message starts a turn. |
| `POST /v1/chat/turns/{turn_id}/stop` | Also stops a turn that hasn't started yet; the stop is saved first. |
| Space message POST responses | `classified: true` when the classifier picked an agent; its pick is never in `mentions`. |

Events can also be `kind: "message_deleted"` (JSON `text`
`{target_event_id, tombstone}`) and `kind: "deleted"` (a tombstone with
no text, recipients or metadata that anchors a reply thread). Room run
completion activity still carries `outcome: "no_reply"`; the desktop
shows nothing for it. A run whose message was deleted keeps its status
in `/v1/runs` with an empty `task`.

Later changes are [in the source, not released](next-source.md).
