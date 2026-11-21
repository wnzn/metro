# Spaces, Chat and the API

This page describes how Metro behaves for its users, and the HTTP API behind
it. Every human endpoint takes `Authorization: Bearer <user token>`. Errors
come back as `{"error": "..."}`.

## Identity

Your identity provider decides who you are. Metro keeps no user directory.

| Sign-in mode | Your `id` (stable, used for membership) | Your `actor` (display only) |
| --- | --- | --- |
| `METRO_DEV_USERS` | The username | The username |
| OIDC | The token's `sub` | The claim named by `METRO_OIDC_ACTOR_CLAIM` (default `sub`) |

`GET /v1/me` returns `{"id", "actor"}`. Owners add members by `id`, never by
display name. The desktop app shows your id with a **Copy ID** button.

**A database belongs to one identity provider.** On first start, the server
records the identity namespace it uses: dev users, or one specific OIDC
issuer. After that it refuses to start with a different one, because ids from
different providers must never be mixed. To change providers, start with a
new database (see [Upgrading](#upgrading-from-the-single-room-build)), or
restore the previous auth settings.

## Spaces and members

- Anyone signed in can create a Space and becomes its **owner**. Each owner can
  have up to 50 Spaces. A new Space starts with a `lobby` room.
- Only **members** see a Space, its rooms and its threads. Members can list
  the member roster, which the desktop uses for `@` autocomplete.
- Only the **owner** adds or removes members, and manages the runner token.
  Nobody can add themselves, and the owner can't be removed. Removing a member
  also stops their running agent runs in that Space.
- A Space holds at most 100 members and 200 rooms, threads included.

| Request | Who | Result |
| --- | --- | --- |
| `GET /v1/spaces` | anyone | `{"spaces": [{"id", "name", "role", "runner_paired"}]}`, only your Spaces |
| `POST /v1/spaces` `{"name"}` | anyone | `201` with the Space, `role: "owner"` |
| `GET /v1/spaces/{sid}/members` | members | `{"members": [{"user_id", "role"}]}` |
| `POST /v1/spaces/{sid}/members` `{"user_id"}` | owner | `201` when added, `200` if already a member |
| `DELETE /v1/spaces/{sid}/members` `{"user_id"}` | owner | `{"removed": true, "stopped_runs": n}` |
| `GET` / `POST /v1/spaces/{sid}/rooms` `{"name"}` | members | List rooms, or create one |

People outside a Space get **404**, the same as for a Space that doesn't
exist, so Metro never reveals whether a Space exists. Members who try an
owner-only action get **403**.

## Messages and addressing

| You write | Addressed to | Agent starts? |
| --- | --- | --- |
| `Can someone review this?` | the room | no |
| `@bob can you review this?` | Bob | no |
| `@agent summarize today` | the agent | yes |
| `@bob @agent check greet.sh` | Bob and the agent | yes |
| any text sent with **Ask agent** | the agent | yes |

Mentioning a person never starts agent work. Only `@agent` or Ask agent does.
In text, `@agent` and `@<member id>` count only as standalone handles, and
`@<member id>` only for current members of the Space. The agent's history shows each message's
author and recipients, so it can tell a question for Bob from one for itself.

- `POST /v1/spaces/{sid}/rooms/{rid}/messages`
  `{"text", "invoke_agent"?, "mentions"?}` returns `202` with the event.
  `mentions` is a list of up to 32 recipients, each `{"kind":"agent"}` or
  `{"kind":"user","id":"<member id>"}`. The desktop picker sends these, which
  also works for opaque OIDC ids that nobody would type.
- `GET /v1/spaces/{sid}/rooms/{rid}/events?after=<id>` returns
  `{"events": [...]}`. Message events carry `mentions` and an `audience`:
  `room`, `agent`, `users` or `mixed`.
- `POST /v1/spaces/{sid}/rooms/{rid}/demo` `{}` runs `workspace.list` on the
  Space's runner and never calls a model. It returns `409` if the Space has no
  paired runner.

## Reactions

Anyone who can see a message can react with one of 👍 👀 ✅ 🙏 ❤️ 🎉. This
works on human and agent messages in Chat, rooms and threads.

- `PUT .../events/{eid}/reactions` `{"emoji"}` adds your reaction, and
  `DELETE` with the same body removes it. Both return `{"changed": bool}`.
  Repeating a reaction you already made, or removing one you didn't make,
  changes nothing and records nothing.
- The path prefix is `/v1/spaces/{sid}/rooms/{rid}` for rooms and threads, and
  `/v1/chat` for Chat.
- **Reactions never start agent work**, and they never mean a task is
  complete. A conversation between people stays between people, reactions
  included.
- When the agent is addressed, it may answer with a single reaction instead of
  a redundant reply, for example acknowledging a thank-you.

## Threads

A room's agent can delegate work to **child threads** with the tools
`thread_create`, `thread_send`, `thread_status` and `thread_stop`.

- A thread is a child room with its own persistent transcript. The desktop
  shows it under its parent room (`parent_room_id`), with a link back.
- A thread inherits its Space's members and paired runner. Its results are
  posted back to the parent room as `thread` events.
- Threads go only one level deep: a thread can't create threads. A room can
  have at most 16 of them.

## Personal Chat

- Every user has a private Chat with the agent. Only that user can see it, and
  it works with zero Spaces.
- `GET /v1/chat/events?after=<id>` and `POST /v1/chat/messages`
  `{"text", "invoke_agent"?}` work like room messages. Chat has no other
  members, so only `@agent` or `invoke_agent` addresses anyone.
- The Chat agent's only tool is `react_to_message`, which acknowledges a
  message addressed to it in your Chat. It has no runner, workspace or thread
  tools, and it can't read Space transcripts. There is no personal runner, and
  Chat messages are never forwarded into a Space.
- The cross-Space controls in Chat are for you, not the agent: you can list,
  open, follow up on and stop runs (see [Runs](#runs)). From Chat, the desktop
  also lets you send a task to the agent of a Space and room you pick
  explicitly, and shows the recipient before you send. All of these check
  your membership.

## Runs

- `GET /v1/runs` returns `{"runs": [...]}`: agent runs in Spaces you belong
  to, threads included. Personal Chat runs aren't listed. A run's status is
  `running`, `completed`, `failed` or `cancelled`.
- `POST /v1/runs/{id}/stop` `{}` stops a run and returns it as `cancelled`.
  The person who started a run can stop it, and so can the Space's owner.
  Other members get 403, outsiders get 404, and a run that has already
  finished gets 409.
- Stopping halts the agent's orchestration. A tool job that a runner has
  already claimed may keep running on that machine until it ends or times
  out.
- Run history is kept in memory, bounded, and lost on restart. Room
  transcripts are durable.
- At most 4 agent runs are active per Space, and 8 per server.

## Runner pairing

- The owner issues the Space's runner token with
  `POST /v1/spaces/{sid}/runner-token` `{}`, which returns `{"token"}` with
  `Cache-Control: no-store`. The server keeps only a hash, so the token can't
  be shown again.
- Issuing a new token **rotates** it: the old token stops working, and jobs
  claimed with it fail. `DELETE` revokes the token (`204`).
- A runner is configured with only that token and `METRO_SERVER_URL`. The
  token decides which Space it serves: every room and thread in that Space,
  and nothing else. The server has no runner settings.
- `runner_paired` means a token exists. It doesn't mean a runner is online.
- Without a runner, a Space still works for chat and Ask agent; only the
  workspace tools are missing. Every member can trigger the runner's tools,
  so every member can read its workspace. See
  [deploy.md](deploy.md#trust-boundaries).

## Keeping secrets out of screenshots and logs

User tokens, runner tokens and model keys are all bearer secrets.

- Keep tokens in mode-600 files. Don't `echo` them or paste them into chat.
  The README's commands never print one.
- The desktop app holds your token in memory only. It shows a new runner token
  once and clears it when you hide it, leave the view or disconnect. Hide it
  before you screen-share.
- Tokens passed with `curl -H` are briefly visible to other local users in the
  process list. On a shared machine, use `curl --header @file` instead.
- If a token leaks, rotate it: issue a new runner token, or change the dev
  token and restart. For OIDC tokens, rely on your provider's controls and
  short lifetimes.

## Upgrading from the single-room build

The single-room build's rooms belong to no Space. Metro never assigns them to
anyone. If the database holds any of that history, the server refuses to
start and says how many rooms and events it found. It accepts an old database
only if that database holds nothing but an empty default lobby.

To upgrade with Compose without losing that history:

1. Stop the server, then copy the database out as your backup:

   ```sh
   docker compose stop server
   docker compose cp server:/data/. ./metro-single-room-backup/
   ```

2. Start the new version on a **new volume** and leave the old one untouched.
   Add a project name to `.env`, for example
   `COMPOSE_PROJECT_NAME=metro-spaces`, then run `docker compose up -d`.
   Compose creates `metro-spaces_server-data`; the old volume stays as it was.
3. Recreate your Spaces and members, and issue new runner tokens.

Outside Compose, point `METRO_DB` at a new, empty file and keep the old one.
