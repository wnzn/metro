# Scheduled prompts

A scheduled prompt is a message that the Metro server sends to the agent for
you, either once or on a recurrence. It goes to your personal Chat, or to a
room or thread in a Space you belong to.

> **Status: demonstrated end to end on the server, with scripted models.**
> These runs pass:
>
> - 30 unit tests and 2 HTTP component tests;
> - the black-box `tests/e2e/schedules.py --timeout 150`, against the real
>   `metro-server` binary and the real clock;
> - its opt-in `--live-recurring` run of a real hourly cron.
>
> See [acceptance.md](acceptance.md#demonstrated-scheduled-prompts-end-to-end-2026-11-22-scheduler-lane-local).
> The desktop's **Scheduled tasks** screen has also been checked natively
> against the real backend: a prompt fired, and its history shows
> `dispatched` ([screenshot](screenshots/scheduled-tasks.png)). Both are
> part of the
> [Documents & media local gate](acceptance.md#demonstrated-final-local-gate-with-documents--media-2026-11-22-primary-workspace-local),
> along with desktop `nsh test` 106/106. The models were local scripted stubs,
> and no real vendor model has been used.

## In the desktop app

Open **Scheduled tasks** in the sidebar. Each task shows:

- its schedule in words, for example *Every day at 09:00 · Asia/Manila*;
- its next firing, in the task's zone and in UTC;
- where it goes;
- its prompt;
- its last status, with **History**, **Edit**, **Pause**/**Resume** and
  **Delete…** buttons.

A one-time task that has fired reads **No more firings**. Its outcome stays
in History, and you can still edit it with a new time, or delete it.

**New task** opens a form with these fields:

- **Name** and **Prompt.**
- **Where:** your personal Chat, or a room or thread of a Space you're a
  member of.
- **When: Once.**
  - Enter a date and a 24-hour time, at most 366 days ahead.
  - **Read the time in:** *This device* (its IANA zone) or *UTC*.
  - The app sends that exact moment, with its offset, and the server stores
    it in UTC. The form previews it both ways, for example *Fires … Asia/Manila,
    which is … UTC*.
  - The desktop can only read a one-time date in your device's zone or in
    UTC. To use another zone, use the API with an RFC 3339 offset.
- **When: Repeats.**
  - Pick *Every day*, *Weekdays* or *Weekly* (choose the day) with an
    `HH:MM` time, or *Cron* to type a five-field expression.
  - **Time zone** takes any IANA name, such as `Asia/Manila`. The **This
    device's** and **UTC** buttons fill it in for you.
  - The server checks the zone and the 15-minute minimum.
  - The next firing shown always comes from the server (`next_run_local`).
    The app never calculates it.
- **Active:** turn it off to save the task paused.

**History** lists each occurrence, with its due time, status and detail. The
app labels the statuses in plain words: *Dispatched*, *Not dispatched* (for
`error`), *Unknown*, *Missed*, *Cancelled*, *Refused: no access* (for
`revoked`) and *Interrupted*. It repeats that *Dispatched* means the agent
accepted the prompt, not that it succeeded.

## What a scheduled prompt does

- **It counts as you explicitly asking the agent.** It goes through the same
  path as an `@agent` message from you: your model connection, and the
  Space's runner when it's in a Space. It never goes to a classifier.
- **It belongs to you.** Only you can see, edit, pause or delete your tasks.
  Another person's task, and one that doesn't exist, both get `404`. Tasks are
  tied to your stable user id. The server stores no user token.
- **The Metro server runs it.** Your desktop app doesn't need to be open.
- **It's checked again when it fires.** For a Space task, you must still be a
  member and the room must still exist, or the occurrence is `revoked`
  ([below](#statuses)). When it's sent, the membership check and the posting
  of the prompt happen in one database transaction, so a member removed at
  that moment can't have a prompt posted in their name. Your model connection is checked when the prompt is
  routed. If it can't be used, the failure is visible in one of two ways.
  Either the occurrence is recorded as `error`, or it's recorded as
  `dispatched` and the turn fails with an error in the conversation, for
  example when your device connector is offline. Either way, nothing falls
  back to another room, Space, person, model or `METRO_MODEL`, and nobody
  else is billed.

## API

All endpoints take your user token, like the rest of the
[human API](spaces.md).

| Request | Result |
| --- | --- |
| `GET /v1/schedules` | `{"tasks": [...]}`: your tasks, oldest first |
| `POST /v1/schedules` | `201` with the task |
| `PATCH /v1/schedules/{id}` | The updated task. Omitted fields are kept. |
| `DELETE /v1/schedules/{id}` | `204`. The task's history is deleted too. |
| `GET /v1/schedules/{id}/history` | `{"runs": [...]}`, newest first, at most 100 |

A create body is `{"name", "prompt", "space_id"?, "room_id"?, "schedule",
"enabled"?}`. Unknown fields are rejected.

- `name`: 1 to 100 printable characters.
- `prompt`: 1 to 8,000 characters, the same as the server's message limit.
- `space_id` and `room_id`: both omitted or `null` for your personal Chat, or
  both set for a room or thread in a Space you belong to. A Space you aren't
  in, or a room that isn't in that Space, gets `404`. In a `PATCH`, setting
  `space_id` to `null` moves the task to your personal Chat and clears the
  room.
- `enabled`: defaults to `true`. Send `false` to create the task paused, or
  in a `PATCH` to pause it. Pausing never needs Space access. Anything that
  leaves the task enabled checks access again.
- `schedule` is one of:
  - `{"kind": "once", "at": "2026-11-23T09:00:00+08:00"}`: an RFC 3339 time
    with an offset. It must be in the future and at most 366 days ahead, and
    it's returned in UTC.
  - `{"kind": "cron", "expression": "0 9 * * *", "timezone": "Asia/Manila"}`:
    a [cron expression](#cron-expressions) in an IANA time zone.

A task includes `id` (`sch_…`), `owner_id`, `name`, `prompt`, `space_id`,
`room_id`, `schedule`, `enabled`, `next_run_at` (UTC), `next_run_local` (cron
tasks only, in the task's own zone), `last_status`, `last_run_at`,
`last_detail`, `created_at` and `updated_at`.

A history entry includes `id`, `due_at`, `status`, `detail`, `claimed_at` and
`finished_at`.

| Error | When |
| --- | --- |
| `400` | Invalid name, prompt, schedule or target, a one-time `at` in the past, or re-enabling a one-time task whose time has passed (give it a new `at`) |
| `404` | Not your task, or a Space or room you can't use |
| `409` | You already have 50 tasks, or the task changed during your edit (try again) |

### Example: every day at 09:00 in Manila

This uses the `api` helper and token variables from the
[contributor quickstart](development.md#two-people-one-space-from-a-shell), so
no token is printed:

```sh
api "$ALICE_TOKEN" POST /v1/schedules '{
  "name": "Morning summary",
  "prompt": "Summarize what changed in the workspace since yesterday.",
  "space_id": "'"$SPACE"'", "room_id": "<room id>",
  "schedule": {"kind": "cron", "expression": "0 9 * * *", "timezone": "Asia/Manila"}
}'
```

To send it to your personal Chat instead, leave out `space_id` and `room_id`.
To pause it, send `PATCH /v1/schedules/<id>` with `{"enabled": false}`.

## Cron expressions

Cron expressions use the standard five fields: `minute hour day-of-month
month day-of-week`. There's no seconds field.

- Fields accept `*`, lists (`1,15`), ranges (`1-5`) and steps (`*/30`,
  `0-30/10`). Month names `jan`–`dec` and weekday names `sun`–`sat` work in
  any case.
- `0` and `7` both mean Sunday.
- The shorthands `@hourly`, `@daily`/`@midnight`, `@weekly`, `@monthly` and
  `@yearly`/`@annually` are accepted.
- Days follow classic (Vixie) cron rules. If either day field is `*`, a day
  must match both. If both are restricted, a day matches when either one
  does, so `0 9 1 * mon` fires on the 1st **and** on every Monday.
- A recurring task can fire at most once every **15 minutes**. A shorter
  spacing anywhere in the expression is rejected, and so is an expression
  that would never fire.

**Time zones.** Fields are local wall-clock times in the task's IANA zone.

- **Clocks jump forward:** a local time that doesn't exist fires at the end
  of the gap, so a daily task is never skipped.
- **Clocks fall back:** a local time that occurs twice fires only the first
  time.
- **No daylight saving time:** zones such as `Asia/Manila` aren't affected.

## When it fires

- The server's scheduler wakes for the next due task, and also when a task
  is created or edited. A one-time task a few seconds ahead fires about on
  time.
- **At most once per occurrence.** An occurrence is claimed, and the task
  moved past it, in one database transaction before anything is sent. A
  second check, a second server process on the same database file, or a
  restart never sends the same occurrence twice.
- **Never late, never in a burst.** An occurrence found more than **5
  minutes** after its time, for example because the server was down, isn't
  run. A single `missed` history entry records how many occurrences were
  skipped, and the task resumes at its next future time. Resuming a paused
  task doesn't catch up either.
- **Edited, paused or deleted after being claimed:** that occurrence is
  `cancelled` and not sent, so stale content never goes out.
- **The server stops while sending:** at the next start, the occurrence is
  marked `interrupted` and not retried. Its prompt may or may not have been
  posted.

## Statuses

Each occurrence has one status. The latest also shows as the task's
`last_status`.

| Status | Meaning |
| --- | --- |
| `dispatching` | Claimed, and being sent |
| `dispatched` | The prompt was posted under your name and the agent run was accepted. This is the same in personal Chat and in Space rooms. **It doesn't mean the agent succeeded.** The answer, or a model, vendor, device or tool error, shows in the conversation. |
| `error` | Sending was refused before a run started, for example because routing found no usable model connection. `detail` says why. |
| `unknown` | The acknowledgment that the prompt was posted and accepted didn't come back within **60 seconds**. That isn't expected to happen, because it doesn't wait for the model. The submission keeps going and isn't retried, and this entry is replaced with the real outcome if one arrives later. |
| `missed` | Found more than 5 minutes late and not run. One entry counts all the skipped occurrences. |
| `cancelled` | The task was paused, edited or deleted between being claimed and being sent |
| `revoked` | You're no longer a member of the task's Space, or its room is gone. The task is **paused**, and a notice is posted to your personal Chat. |
| `interrupted` | The server stopped while sending. It isn't retried, and the prompt may or may not have been posted. |

## Limits

| Limit | Value |
| --- | --- |
| Tasks per person | 50 |
| Shortest recurrence | 15 minutes |
| How far ahead a one-time task can be | 366 days, and strictly in the future |
| History kept per task | 100 entries |
| Late threshold before `missed` | 5 minutes |
| Time before `unknown` | 60 seconds |
| Name, prompt | 100 characters, 8,000 characters |

## Operator setting

| Variable | Meaning |
| --- | --- |
| `METRO_SCHEDULE_TICK_SECS` | The longest time the scheduler sleeps between checks. The default is 30, and values are clamped to 1–300. It wakes earlier for the next due task, and when a task changes, so you rarely need to set this. |

Schedules live in the server's SQLite database, `schedules` and
`schedule_runs`, which are created on startup. They're included in your
normal database backups.
