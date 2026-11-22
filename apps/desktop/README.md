# Metro desktop

The Metro desktop client, written in TSX on [Native Shell (NSH)](https://nsh.dev).
NSH renders it with each platform's own views: GTK 4 on Linux, AppKit on macOS,
and Win32 on Windows. No npm or Node is needed to build or run it.

## Install (users)

Download the build for your system from
[Releases](https://github.com/wnzn/metro/releases). **No release has been
published yet.** The 0.2.0 files have been built and verified locally
([evidence](../../docs/acceptance.md#final-metro-020-local-release-prepublication-2026-11-23-project-manager-local)), but there are no files to download yet. You never need NSH, a
checkout or a terminal to use the app; the build commands further down are
for contributors.

- **Linux x64:** `Metro-<version>-linux-x64.tar.gz`. Extract it and start
  its launcher. Needs glibc ≥ 2.39, GTK ≥ 4.14 and WebKitGTK 6.0.
- **Windows x64:** `Metro-<version>-windows-x64.zip`, a portable ZIP. See
  [Windows portable package](#windows-portable-package).

Check each download against the `.sha256` file published beside it. The
0.2.0 names are verified targets but aren't published yet.

The first screen, **Connect to Metro**, offers two choices:

- **On this computer → Use locally:** a private workspace that the app
  runs on this computer, just for you. It needs no server address or
  token, and its data stays on this computer, separate from any server you
  connect to.
- **Or connect to a server:** the URL and token your admin gave you
  ([README](../../README.md#install-and-use-prebuilt-only)).

### Verified in the 0.2.0 Linux candidate

QA ran the packaged candidate in Debian 13 under Xvfb, as a non-root user,
from a read-only install, with Docker `--network none`
([acceptance](../../docs/acceptance.md)). It isn't a release.

- **Local workspace:** the local button (labelled *Open local workspace* in
  that candidate; **Use locally** in the final wording) started the bundled server
  with no token or remote service. QA created a Space and posted a message,
  which showed its time. After quitting, the app's listeners and child
  processes had all ended. On reopening, the same room, the message and the
  saved profile and avatar were all still there
  ([screenshot](../../docs/screenshots/local-workspace-relaunch.png)).
- **Settings** opens as a window over the app, with **Profile**,
  **Runners**, **Model providers**, **Model routing** and **Appearance**.
- **Profile:** the display name and a picture chosen through the native
  Linux file picker were saved, and the avatar showed next to the message
  ([screenshot](../../docs/screenshots/settings-profile.png)). Your
  **User ID** is shown here, not in the sidebar.
- **Sidebar footer:** shows where you're connected and your id. It says
  *On this device* only when the session is positively identified as this
  app's own hosted local workspace. Any other server, including one you
  reach through a manual localhost port forward, shows its actual URL. The final
  0.2.0 source checks for the hosted local workspace itself
  (`onLocalWorkspace()`), and a test covers a manual port forward. A
  UI-validation build cut from older source showed the plain loopback
  address in local mode. The final archive hasn't been re-checked in the
  GUI.
- **Search** is a centered box at the top of the window.
- **Provider picker:** one searchable dropdown grouped by vendor. Each
  vendor shows how many ways it can be connected and its capabilities. For
  example, OpenAI is one row with three ways to connect.
  Each vendor's sign-in and key options are grouped under its name. Vendor
  marks are 21 monochrome SVGs bundled with the app, from
  `@lobehub/icons-static-svg` (MIT). They're tinted with the theme, shown
  only beside the vendor's own name, and never fetched over the network.
  Other providers get a lettermark.
- **In-app device bridge:** a separate packaged smoke of the app's device
  bridge with the bundled server passed.

### Also verified

These were checked in a packaged Linux UI-validation build, with no CLI,
separate runner or real vendor
([acceptance](../../docs/acceptance.md)):

- **Device connection in the app:** **Settings → Model providers →
  Connect this device** asked for consent before pairing, and no device
  token was ever shown or entered. A test endpoint offered two models, and
  only the approved one was listed by **Test connection** and used. The
  agent answered through it, and kept answering after signing out and
  after a full restart.
- **Local tools:** turned on separately for two Spaces from **Manage**.
  Each Space's listing showed only its own folder, and no model was called.
  Turning one Space off left the other on.

Checked by tests:

- **Local times:** message times are shown in your local time zone. Tests
  with `Intl` time zones verify the conversion from the server's UTC
  `created_at` ([spaces.md](../../docs/spaces.md#message-times)).

### Local tools on this computer

In the app's own local workspace, a Space's owner can open **Manage → Local
tools on this computer → Turn on local tools**. The agents in that Space can
then list and read files (`workspace.list` and `workspace.read`) in one
private folder on this computer, per Space, while Metro is open.

- They can't change files, run commands or programs, or see anything else
  on your disk. Links out of the folder are refused.
- It's per Space and owner only. It's offered only when the Space has no
  other runner, and it never replaces one. Turning it off revokes only its
  own pairing.
- It's only for this computer's own local workspace, never for a remote
  server, and never for personal Chat or model access.
- Checks, MCP and commands, and tools for a shared server, need a separately
  installed runner run by an operator ([runners.md](../../docs/runners.md)).

### Not verified yet

- **Device connections with a real vendor:** the in-app flow was verified
  only against a local test endpoint ([above](#also-verified)). No real
  vendor account or vendor sign-in has been tested. Vendor software such as
  the Claude Code CLI stays separate, and you install it yourself.
- **Status badges in the GUI:** in an earlier tested build, a **Starting…**
  badge could stay shown until the view was reopened. The final 0.2.0
  source fixes this: badges now move to *Serving from here* or *On* in the
  open view, which automated tests cover. The final archive hasn't been
  re-checked in the GUI.
- **Windows:** the candidate opened a Metro window and its server answered
  `/healthz` under Wine. Nobody has checked that it draws or takes input,
  or run it on native Windows.
- App-wide consistency of icons and type: icons appear in the sidebar and
  Settings, but no app-wide check was recorded.
- **Local tools:** `workspace.read` hasn't been exercised in the app, and
  neither have arbitrary paths. In the UI-validation build, QA also saw
  the general runner token controls and *Runner attached* under the local
  tools card.
- Documents & media: the native file picker was verified only for the
  profile picture. Library uploads, and audio and video playback and
  seeking, haven't been verified. In a headless GTK run of the source
  after 0.2.0, the audio player loaded a file's real duration (0:10) but
  showed no playback progress.

## What it does

- **Connect:** open the local workspace, or enter a server URL (default
  `http://127.0.0.1:8787`) and a bearer token. Plain `http://` is accepted only for loopback (`localhost`,
  `127.0.0.0/8`, `::1`). Remote servers need `https://`, so a token never
  crosses a network in clear text. Redirects are refused.
- **Chat:** your personal thread with the agent, outside every Space. Only
  you see it. Its agent can't see your Spaces, and its only tool is
  acknowledging with a reaction. Next to it, the Chat shows active and recent
  **agent runs** in your Spaces, with three actions:
  Open (go to the room or thread), Follow up, and Stop. **New task** there
  sends a task right away, once, to the agent of a Space and room or thread
  that you pick explicitly. The recipient is spelled out before **Send to
  agent** ("To: agent in Acme › #lobby"). Follow up opens the same form, and
  Close folds it while keeping your draft. This is separate from
  **Scheduled tasks › New task**, which creates a schedule.
- **Spaces:** a Space has its own rooms (starting with its `lobby`), members,
  and optional paired runner. A new account has no Spaces. It sees how to
  create one. Your user ID, to give an owner, is under **Settings →
  Profile**. There is no shared lobby.
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
  runner. It is disabled when the Space has no runner. Attached or paired
  means jobs go to that runner. It does not mean the runner is online. The
  call and its listing show as one collapsed **Runner activity** group. Demo
  never calls a model, so it never says "thinking".
- **Agent activity:** each agent turn shows as **one** collapsed group, not a
  row per tool. The turn is identified by the event's `activity` metadata
  (`id`, `name`, `source`, and a lifecycle `phase` on `kind: "activity"`
  events). While the turn runs, the group reads **`<name> is thinking…`**
  (`Metro` when the server sends no name), even before its first tool. Once
  it ends, it reads **`<name>’s activity`**. A failed, cancelled or interrupted
  turn, or a failed tool, carries a badge that stays visible while collapsed.
  One press (or Space/Return) opens every step in order: the calls with their
  arguments, the results, and any summaries the model provider shared. Very
  long output still folds. Metro shows no chain of thought of its own. The
  group stays where its first event appeared, and later steps join it
  without moving other messages or closing it on the next poll. People's
  messages, the agent's replies, errors and reactions stay in the transcript
  as before. Older events without metadata each stay a separate collapsed
  group, because which run they belong to is unknown.
- **Which model answered:** a turn's `started` marker carries the route the
  server recorded (`activity.route`). When it's worth saying (a use case, a
  model chosen for that message, a start by System One or a scheduled
  prompt), the collapsed group shows one compact badge, such as **Routed:
  Incident review · 91%** (or **Routed: chosen model**). Opening the group
  adds one line: where the model came from, its provider and model ID, and
  the server's short reason. The raw probability (`p 0.91`) is never
  shown. Connection IDs are
  never shown, and the route is not counted as a step. A turn that answered
  with no steps keeps a row only when it has such a badge. A message that
  System One judged to be for the agent is labelled **agent · detected by
  System One**, never as an `@agent` its sender typed. The app never sends
  `addressed_by`; only the server sets it.
- **Search** (the box at the top of the window; <kbd>Ctrl</kbd>/<kbd>⌘</kbd>+<kbd>K</kbd>
  focuses it): typing sends nothing, and **Return** runs the search. The
  Search page has two modes, **Conversations** (messages you can read) and
  **Documents & media** (your indexed files), and no second query field.
  A result opens your Chat, or the room or thread it came from, if you're
  still a member. Results show no raw scores.
- **Library** (sidebar): your personal library and the files of Spaces
  you're in. **Upload**, then **Open** on each row. The row's **More** menu
  holds **Index** (or **Index again**), and **Delete…** for the uploader or
  the Space owner, which asks first. On this page, the top search box searches
  documents and media. The media model is chosen in **Settings › Model
  providers › Documents & media**. See [docs/library.md](../../docs/library.md).
- **Scheduled tasks** (sidebar): **New task** creates a schedule. Each card
  has **Edit**, and **History**, **Pause**/**Resume** and **Delete…** in
  its **More** menu. See [docs/schedules.md](../../docs/schedules.md).
- **Account row** (sidebar footer): your picture, name and host. Its menu
  offers **Copy server URL**, **Copy ID** and **Sign out**.

Chat, Search, Library and Scheduled tasks are your own pages, outside every
Space, and work with or without any Space. The entries above describe the
current source after 0.2.0, which isn't released. Return-only search with
its scope and type filters, folded zero-similarity results, the Library
page, the media model in Settings, scheduled-task cards with the History
menu, the routing badge's percentage, the account row, and Chat's **New
task** form were checked in a native Linux GTK run, in dark and light themes,
of the unreleased source after 0.2.0. At 860 px wide with the runs shown,
Chat's composer and header currently overlap the runs. A fix is in
progress. The account menu's items and the Ctrl/⌘+K shortcut are covered
by automated tests. Windows and macOS weren't checked.
- **Manage (owners only):** list, add, and remove members. Pick which runner
  serves the Space: attach one of your personal runners or an organization
  runner from the same list as Settings › Runners, or detach it. A Space that
  still has a dedicated (single-Space) token can rotate or revoke it, and an
  owner can still issue one on purpose. A Space uses at most one runner, so
  attaching needs the dedicated token revoked first.
- **Settings** (sidebar, available with or without any Space):
  - **Runners:** add a named runner, either personal or (for org admins)
    organization-wide. Its token is shown once, masked until you press Show,
    next to copyable steps for the prebuilt runner of this app's version:
    either download `Metro-runner-<version>-linux-x64.tar.gz` from Metro
    Releases, check its `.sha256` and start its `run-runner` with the token in
    a mode-600 file, or run `ghcr.io/wnzn/metro-runner:v<version>` with
    `docker run --env-file ./runner.env` and a named volume at `/workspace`.
    Each Space gets `spaces/<space id>/`. Metro never asks you to build the
    runner.
    No command or template contains the token. Rotate and delete are offered
    only to whoever manages the runner (its owner, or org admins). Both are
    confirmed first, with the Spaces you can see that use the runner.
  - **Model providers:** the providers come from the server's
    `/v1/provider-catalog`, grouped by vendor in a searchable dropdown.
    **On this device** connections run inside the packaged app: **Connect
    this device** pairs it without showing a token, and **Manage
    connection** follows once paired. Metro has no vendor key field for
    them. The connection shows as paired, online or offline, and you can
    unpair it with a confirmation. (A copy of the app run without its
    launcher, such as a development build, has no device helper and instead
    shows `metro-connector` commands for a connector an operator already
    installed on a headless computer.) **Server-stored API profiles**
    take an API key that is sent once and never read back. Custom endpoints
    must match one of the operator's approved URLs exactly. **Save and fetch
    models** lists models, or you enter a model ID by hand when the endpoint
    can't list them. You can also make a connection your default and replace
    or delete it.
    Each connection serves one **capability**, fixed by its provider: **Chat**
    (answers), **System One** (typed decisions) or **Embeddings** (search).
    The list filters by capability, each connection shows its capability,
    and each capability has its own default: making a System One connection
    the default never changes your chat default. A provider that can't list
    its models needs a model ID before it's saved, and a custom-endpoint
    provider with no approved endpoint can't be saved at all.
    **Sharing…** (owner only) sets who else may use a connection: **Only me**
    (the default), **Selected Spaces** (only Spaces you're in, and only while
    both you and they are members) or the **Organization**. Saving **Only
    me** revokes it. The panel says what sharing means: others can send
    requests through it and use your credits, plan or device, you can see
    what they send and may pay for it, and they never see its base URL, key,
    sign-in or device token. A connection needs a model before it can be
    shared. A Sign in with ChatGPT connection shares only a bring-your-own
    preset: each person signs in with their own account.
    In the current source after 0.2.0, this page also holds **Documents &
    media**: **Index and search with** picks your media model (only
    media-capable connections), and **Where your files go** says what Index
    and search send, and to whom.
  - **Model routing:** all off by default, and only ever about messages you
    write. Pick a **System One classifier**, an **embeddings model** for
    search, and a **fallback answer model**. Each picker offers only models of
    its capability that you own or that are shared with you, grouped by whose
    they are; one with no model, or a preset you can't run yet, can't be
    picked. **Use-case routing** has editable named use cases (name,
    description, answering chat model, up to 16) and a minimum confidence.
    **Classify my unaddressed messages** is a separate opt-in with its own
    threshold (0.5 to 1). **Where your text goes** says, from what is chosen,
    which endpoint receives which text, whose it is, and what is never sent.
    The page saves the whole draft at once; leaving it drops unsaved edits.
  - **Appearance:** Dark (the default), Light or System.
  - Every token or key shown once is wiped from memory when you press Done,
    change page, Space or room, or disconnect. None of them is stored.

## Runtime requirements

- **Linux x64:** glibc ≥ 2.39, GTK ≥ 4.14 and **WebKitGTK 6.0**
  (`libwebkitgtk-6.0-4` on Debian and Ubuntu). The app's native host links
  WebKitGTK even though Metro shows no web views. GStreamer is optional.
- **Windows x64:** 64-bit Windows 10 1809 or later, or Windows 11 (see
  [below](#windows-portable-package)). Not yet tested on native Windows.
- **macOS:** not tried.

Building, testing and capturing screenshots of the app from source are for
contributors only, in
[docs/development.md](../../docs/development.md#the-desktop-app).

## Windows portable package

The release file will be `Metro-<version>-windows-x64.zip`, a portable build
with no installer. The package recorded so far was built locally as
`Metro-windows-x64.zip`. It isn't signed or published yet, and it hasn't been started on native Windows. The 0.2.0 candidate has only been started under Wine. Its
checks are in [acceptance](../../docs/acceptance.md#verified-windows-x64-portable-package-built-on-linux-2026-11-22-packaging-local).

1. Check the ZIP against its `.sha256` file, for example with
   `Get-FileHash Metro-0.2.0-windows-x64.zip -Algorithm SHA256` in PowerShell.
2. Extract the **whole** ZIP. Don't run the app from inside the archive.
3. Run `Metro\Metro.exe`. Keep the `nsh` folder next to it, because it holds
   the app bundle.

It needs 64-bit Windows 10 1809 or later, or Windows 11. If Windows reports
that `VCRUNTIME140.dll` or `VCRUNTIME140_1.dll` is missing, install the
[Microsoft Visual C++ v14 Redistributable (x64)](https://learn.microsoft.com/cpp/windows/latest-supported-vc-redist).
Windows N editions may also need the Media Feature Pack.

The package holds no credentials and no server. Point it at your Metro
server, and use HTTPS when the server is remote.

## Layout (contributors)

```text
App.tsx                 entry: the connect screen or the signed-in shell
nsh.json                app id, NSH version, window, theme
theme/theme.json        design tokens (light and dark): colors, tones, spacing, type, radius
src/ui/                 the replaceable styled layer: tokens.ts maps semantic names to
                        theme classes, primitives.tsx holds the components features compose
src/api/                HTTP contract (client.ts) and the server-URL guard (serverUrl.ts)
src/store.ts            session, sync loop, caches, actions: no UI
src/settings.ts         Settings state: runners, Space runner, provider catalog and connections,
                        the one-time token reveal; hooks into store.ts's session lifecycle
src/routing.ts          Model routing draft and its rules, the models you may pick, and
                        connection sharing; same session and race rules as settings.ts
src/features/           screens: ConnectScreen, Shell, Sidebar, SpaceView, ChatView,
                        ManageSpace, Conversation (transcript and composer), eventKinds,
                        activity/ (one collapsed group per agent turn), SettingsView, RunnersSettings,
                        ProvidersSettings, RoutingSettings, AppearanceSettings, SearchView,
                        ScheduledTasksView, TokenReveal (one-time token panel),
                        setupTemplates (non-secret Docker and metro-connector steps)
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
- **Shell:** `workspace`, `sidebar`, `nav-chat`, `nav-search`,
  `nav-schedules`, `space:<sid>`,
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
- **Activity:** per group, where `<key>` is `<source>:<activity id>` (for
  example `agent:run_…`, `demo:demo_…`) or `event:<event id>` for an older
  uncorrelated event: `activity:<key>`, `activity-toggle:<key>`,
  `activity-title:<key>` (no steps yet), `activity-outcome:<key>`,
  `activity-details:<key>`, `activity-route-badge:<key>`,
  `activity-route:<key>` (opened), and `activity-step:<event id>` for each step
- **Manage:** `manage-space`, `close-manage`, `members-card`,
  `members-list`, `member:<user>`, `remove-member:<user>`, `add-member-id`,
  `add-member`, `runner-card`, `runner-paired`, `attached-runner`,
  `detach-runner`, `confirm-detach`, `dedicated-runner`,
  `issue-runner-token`, `revoke-runner-token`, `confirm-revoke`,
  `attach-runner-list`, `attachable:<runner>`, `attach-runner:<runner>`,
  `open-runner-settings`, `manage-error`, `manage-notice`
- **One-time token panel** (Settings and Manage): `token-reveal`,
  `token-reveal-title`, `runner-token-value` (masked until shown),
  `reveal-toggle`, `copy-runner-token`, `hide-runner-token` (Done),
  `setup-build`, `setup-envfile`, `setup-env`, `setup-docker` (each with
  `-copy`), `connector-step-<n>`, `reveal-test-connection`
- **Settings:** `nav-settings`,
  `nav-settings:<runners|providers|routing|appearance>`, `settings`,
  `runners-settings`, `providers-settings`, `routing-settings`,
  `appearance-settings`
- **Runners:** `add-runner-group`, `runner-name`,
  `runner-visibility:<personal|organization>`, `add-runner`, `runner-list`,
  `runner:<id>`, `rotate-runner:<id>`, `delete-runner:<id>`,
  `runner-confirm:<id>`, `confirm-rotate-runner`, `confirm-delete-runner`,
  `cancel-runner-action`, `runners-error`, `runners-notice`
- **Model providers:** `provider-search`, `device-providers`,
  `server-providers`, `provider:<provider id>`, `device-form`,
  `server-form`, `provider-name`, `provider-url:<url>`,
  `provider-base-url`, `provider-key`, `provider-model`, `provider-save`,
  `provider-save-fetch`, `provider-create-device`, `provider-form-error`,
  `connection-list`, `connection:<id>`, `connection-default:<id>`,
  `connection-make-default:<id>`, `connection-models:<id>`,
  `connection-token:<id>`, `connection-unpair:<id>`,
  `connection-confirm-unpair:<id>`, `connection-confirm-unpair-now:<id>`,
  `device-status:<id>`, `device-status-text:<id>`,
  `connection-replace-key:<id>`, `connection-key-input:<id>`,
  `connection-key-save:<id>`, `model:<id>:<model>`,
  `connection-model-list:<id>`, `connection-model-input:<id>`,
  `connection-model-save:<id>`, `listing-ok:<id>`, `listing-error:<id>`,
  `connection-delete:<id>`, `connection-confirm-delete:<id>`,
  `connection-confirm-delete-now:<id>`, `catalog-error`, `providers-error`,
  `providers-notice`, `provider-capability` (and
  `provider-capability:<all|chat|system_one|embeddings>`),
  `connection-capability:<id>`, `connection-shared:<id>`
- **Sharing:** `connection-sharing:<id>`, `sharing:<id>`,
  `sharing-visibility:<id>` (and `:<personal|spaces|organization>`),
  `sharing-spaces:<id>`, `sharing-space:<id>:<sid>`, `sharing-warning:<id>`,
  `sharing-preset:<id>`, `sharing-needs-model:<id>`, `sharing-save:<id>`,
  `sharing-error:<id>`, `sharing-notice:<id>`
- **Model routing:** `routing-classifier`, `routing-embedding`,
  `routing-default-chat` (each with `-missing`), `routing-enabled`,
  `routing-infer`, `routing-min-route`, `routing-min-directed`,
  `routing-add-case`, `case:<n>`, `case-label:<n>`, `case-description:<n>`,
  `case-model:<n>`, `case-remove:<n>`, `case-missing:<n>`,
  `routing-dataflow`, `dataflow-<routing|addressing|search|never>`,
  `routing-save`, `routing-revert`, `routing-error`, `routing-notice`,
  `routing-unsupported`, `available-error`,
  `routing-no-<classifier|chat|embedding>`
- **Search:** `top-search`, `search-view`, `search-surface`, `search-form`,
  `search-scope`, `search-results`, `search-less-relevant`, `search-result:…`, `search-open:…`,
  `search-summary`, `search-ranking`, `search-ranking-explain`,
  `search-partial`, `search-notice`, `search-empty`, `search-hidden`,
  `search-error`, `search-open-error`, `search-open-providers`,
  `search-index-progress`, `search-index-more`, `search-index-notice`,
  `search-index-error`
- **Scheduled tasks:** `schedules-view`, `schedule-list`, `schedule-new`,
  `schedules-reload`, `schedules-error`, `schedules-notice`, `schedule-form`,
  `schedule-name`, `schedule-prompt`, `schedule-destination` (and
  `schedule-destination:…`), `schedule-room`, `schedule-rooms-state`,
  `schedule-recipient`, `schedule-when`, `schedule-once-date`,
  `schedule-once-time`, `schedule-once-zone`, `schedule-once-preview`,
  `schedule-repeat`, `schedule-repeat-time`, `schedule-weekday`,
  `schedule-cron`, `schedule-cron-preview`, `schedule-zone`,
  `schedule-timezone`, `schedule-enabled`,
  `schedule-save`, `schedule-cancel`, `schedule-form-error`, and per task
  (`…:<id>`): `schedule:`, `schedule-summary:`, `schedule-state:`,
  `schedule-next:`, `schedule-exact:`, `schedule-last:`, `schedule-edit:`,
  `schedule-more:` (its items `schedule-history:`, `schedule-pause:`, `schedule-resume:`,
  `schedule-delete:`), `schedule-busy:`, `schedule-run:`, `schedule-confirm:`,
  `schedule-confirm-delete:`, `schedule-cancel-delete`, `schedule-legend:`,
  `schedule-history-refresh:`, `schedule-runs:`, `schedule-runs-empty:`,
  `schedule-runs-error:`

## Limits

- Polling, not push. New events appear within about 1.5 s.
- Each room keeps the last 2,000 events in memory. Older history isn't
  paged back yet.
- Mentions use user IDs as written. Messages show the author's display name,
  as the server recorded it when the message was written.
- The WebKitGTK 6.0 requirement on Linux comes from NSH's host, not from
  Metro.
