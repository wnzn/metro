# Metro desktop

The Metro desktop client, written in TSX on [Native Shell (NSH)](https://nsh.dev).
NSH renders it with each platform's own views: GTK 4 on Linux, AppKit on macOS,
and Win32 on Windows. No npm or Node is needed to build or run it.

## What it does

- **Connect:** you enter a server URL (default `http://127.0.0.1:8787`) and a
  bearer token. Plain `http://` is accepted only for loopback (`localhost`,
  `127.0.0.0/8`, `::1`). Remote servers need `https://`, so a token never
  crosses a network in clear text. Redirects are refused.
- **Chat:** your personal thread with the agent, outside every Space. Only
  you see it. Its agent can't see your Spaces, and its only tool is
  acknowledging with a reaction. Next to it, the Chat shows active and recent
  **agent runs** in your Spaces, with three actions:
  Open (go to the room or thread), Follow up, and Stop. You can also send a
  task to the agent of a Space and room that you pick explicitly. The
  recipient is spelled out before you send ("To: agent in Acme › #lobby").
- **Spaces:** a Space has its own rooms (starting with its `lobby`), members,
  and optional paired runner. A new account has no Spaces. It sees how to
  create one, or the user ID to give an owner. There is no shared lobby.
- **Rooms and threads:** threads that an agent delegated
  (`parent_room_id`) appear under their parent room. A thread links back to
  its parent.
- **Mentions:** type `@` or press **@** to mention `@agent` or a Space member
  by their stable user ID. Picked mentions show as chips. A chip goes away
  when its text is deleted, and all chips are cleared when you change room or
  Space. Mentioning a person never starts agent work. Only `@agent` or
  **Ask agent** does. Messages show their audience and recipients, and
  `@you` is highlighted.
- **Reactions:** 👍 👀 ✅ 🙏 ❤️ 🎉 on human and agent messages, in rooms,
  threads and the Chat. Your own reaction is highlighted. The agent's
  acknowledgement is labelled "agent", and screen readers hear who reacted.
  Reactions are folded from `reaction` events and are never shown as
  transcript lines.
- **Run demo:** runs the deterministic `workspace.list` on this Space's
  paired runner. It is disabled when the Space has no runner. Paired means a
  token was issued. It does not mean the runner is online.
- **Manage (owners only):** list, add, and remove members. Issue, rotate, or
  revoke the Space's runner token. A newly issued token is shown once. It is
  cleared when you hide it, leave the view, switch rooms or Spaces, or
  disconnect.

## Requirements

- **NSH 0.6.x.** Install the official release with
  `curl -fsSL https://nsh.dev/install.sh | sh`, which installs to `~/.nsh` and
  links `~/.local/bin/nsh`, or extract
  `nsh-linux-x86_64.tar.gz` from <https://github.com/longbridge/nsh/releases>.
  `nsh.json` pins `>=0.6, <0.7`.
- **Linux window (`nsh run` / `--target linux`):** glibc ≥ 2.39, GTK ≥ 4.14,
  and **WebKitGTK 6.0** (`libwebkitgtk-6.0-4`). The NSH Linux host links
  WebKitGTK even though Metro shows no web views. GStreamer is optional.
- **Headless (`nsh test`, `nsh check`, `nsh build`):** only `nsh`. These run
  anywhere, CI included.
- macOS 14+ and Windows 10 1809+ follow NSH's platform support. They have not
  been tried with Metro.

## Commands

Run these from `apps/desktop`.

```sh
nsh check                 # TypeScript 7 type check (tsc downloaded once by nsh)
nsh test                  # headless tests, hermetic: network stubbed, no server needed
nsh run App.tsx           # open the app in a native window (needs the Linux requirements above)
nsh package App.tsx --target linux   # .nsh/out/Metro/ and .nsh/out/Metro.tar.gz (NSH's prebuilt release runner)
```

To work on the UI without the Rust server, use the dev double. It is the
same in-memory fake the tests use. It has no agent, runner or persistence,
so never use it as evidence:

```sh
node dev/mock-server.mjs       # Node >= 22.18; users alice, bob, carol (fake tokens in test/fakeServer.ts)
```

### Screenshots (native Linux, isolated)

`dev/screenshots/` runs the real `metro-server` and `metro-runner` on loopback
with a temp database, workspace and tokens. It drives the app in NSH's GTK
host on a private Xvfb display inside a container. The screenshot steps:

1. Bob @mentions Alice from his app.
2. Alice's app captures the connect screen before any secret is entered.
3. Alice reacts, posts, runs Demo, and opens Manage.

Tokens live only in mode-600 files inside the temp directory, which is
removed on exit.

```sh
docker build -t metro-desktop-shots:trixie dev/screenshots
dev/screenshots/capture.sh ../../docs/screenshots      # NSH_DIST=… if nsh isn't on PATH; METRO_BIN=… for other binaries
```

The agent shots (`chat-runs.png`, `thread.png`) need a model. By default
the script starts `scripted_model.py`, a deterministic local OpenAI-compatible
stand-in for the model only. The server, Rig, the tool call, the runner and
its listing are all real, and the stand-in's replies say "Scripted test
model". To use a real model instead, set `MODEL_ENV_FILE` to a mode-600 file
of `METRO_MODEL*` lines. To skip the agent shots, set `SCRIPTED_MODEL=0`.

## Layout

```text
App.tsx                 entry: the connect screen or the signed-in shell
nsh.json                app id, NSH version, window, theme
theme/theme.json        design tokens (light and dark): colors, tones, spacing, type, radius
src/ui/                 the replaceable styled layer: tokens.ts maps semantic names to
                        theme classes, primitives.tsx holds the components features compose
src/api/                HTTP contract (client.ts) and the server-URL guard (serverUrl.ts)
src/store.ts            session, sync loop, caches, actions: no UI
src/features/           screens: ConnectScreen, Shell, Sidebar, SpaceView, ChatView,
                        ManageSpace, Conversation (transcript and composer), eventKinds
test/fakeServer.ts      in-memory server double for the tests and the dev mock
App.test.tsx, src/**/*.test.ts   headless tests
dev/                    mock server and screenshot tooling
```

**Restyling.** Feature components pass only semantic props (`variant`,
`tone`, `gap`) and never name a color, size or class. Change
`theme/theme.json` to re-theme the app, or replace `src/ui/` behind the same
exports to swap the component set.

## Sync and state

- **Polling:** one loop per connection. It polls the active room, or the
  Chat, every 1.5 s with `?after=<cursor>`. It refreshes the Space and room
  lists every 10 s, and runs every 3 s while the Chat is open. Sending,
  reacting, or switching polls immediately.
- **Scoped caches:** events, cursors, reactions, members and in-flight
  requests are keyed by Space + room. Every lobby is called `lobby`, so the
  Space is always part of the key. Each request checks on return that its
  session is current and its Space hasn't been purged since, using a
  per-Space epoch. A late answer never lands in another Space or after access
  is lost.
- **Deduplication:** events at or below a room's cursor are dropped, so
  overlapping or repeated responses never duplicate rows.
- **Errors and retries:**
  - 401 ends the session and the connect screen says why.
  - 403 or 404 on a Space reloads the Space list. A Space you lost is
    purged, and you go to the Chat, never silently to another Space.
  - Network errors, timeouts, 5xx and 429 back off from 1 s up to 30 s,
    with a "Retry now" banner. The transcript stays on screen.
- **Secrets:** the access token is held in memory only. It never reaches
  storage, logs or the UI. Only the server URL and the last Space are
  remembered.

## Stable test IDs

These IDs are used by the tests and screenshot steps; keep them stable:

- **Connect:** `server-url`, `access-token`, `connect`, `connect-error`,
  `connect-screen`
- **Shell:** `workspace`, `sidebar`, `nav-chat`, `space:<sid>`,
  `room:<rid>`, `room-list`, `new-space-toggle`, `new-space-name`,
  `create-space`, `new-room-toggle`, `new-room-name`, `create-room`, `my-id`,
  `copy-id`, `disconnect`, `connection-state`, `server-origin`
- **Room:** `space-view`, `room-title`, `open-parent`, `runner-state`,
  `run-demo`, `manage-space`, `transcript`, `event:<id>`, `composer`, `send`,
  `ask-agent`, `mention`, `mention-picker`, `pick:agent`, `pick:<user>`,
  `mention-chips`, `mention-chip:agent`, `mention-chip:<user>`,
  `recipient:<event>:<agent|user>`, `reactions:<id>`, `react:<id>`,
  `palette:<id>`, `pick-reaction:<id>:<emoji>`, `reaction:<id>:<emoji>`,
  `notice`, `action-error`, `connection-banner`, `retry-now`
- **Chat:** `chat-view`, `onboarding`, `onboarding-id`,
  `onboarding-space-name`, `onboarding-create-space`, `runs-aside`,
  `runs-list`, `runs-error`, `run:<id>`, `run-open:<id>`, `run-follow:<id>`,
  `run-stop:<id>`, `dispatch`, `dispatch-space:<sid>`,
  `dispatch-room:<rid>`, `dispatch-text`, `dispatch-recipient`,
  `dispatch-send`, `dispatch-error`, `dispatch-notice`
- **Manage:** `manage-space`, `close-manage`, `members-card`,
  `members-list`, `member:<user>`, `remove-member:<user>`, `add-member-id`,
  `add-member`, `runner-card`, `runner-paired`, `issue-runner-token`,
  `runner-token-reveal`, `runner-token-value`, `copy-runner-token`,
  `hide-runner-token`, `revoke-runner-token`, `confirm-revoke`,
  `manage-error`, `manage-notice`

## Limits

- Polling, not push. New events appear within about 1.5 s.
- Each room keeps the last 2,000 events in memory. Older history isn't
  paged back yet.
- Mentions use user IDs as written. There are no display names until the
  server provides them.
- The WebKitGTK 6.0 requirement on Linux comes from NSH's host, not from
  Metro.
