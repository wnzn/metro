# In the source, not released

This guide describes the Metro source at `b444379`. None of the changes
below are in the published [Metro 0.4.0][release]. The screens and API
changes are a preview, not instructions for 0.4.0; wait for release files
that include them. For the current downloads, see [Install](install.md).

## Personal Chat

**Send** (or Return) asks your Chat agent directly, even when the message
contains only attachments. No `@agent`, **Ask agent**, or System One
addressing decision is needed. The reply uses your chosen model or your
existing model routing and defaults. A direct send does not give the
agent access to the attached files: Metro saves them to your personal
Library and shows them on the message.

Choose **Save note** to keep the message or attachments in Chat without
starting a run, even if you type `@agent`. Notes aren't a separate library
or a way to hide a message from later Chat history. The Chat agent still
has no runner, workspace or thread tools and cannot read Space transcripts.
Its web tools require a separate [opt-in](web-search.md).

For API clients, `POST /v1/chat/messages` defaults to a direct send.
Use `asset_ids` to attach files from your personal Library.
Set `save_note: true` for a note; it overrides `invoke_agent` and any
mention in the text, and cannot be combined with a per-turn model choice.
Legacy `invoke_agent: false` still saves a note unless the text expressly
mentions `@agent`. `/v1/model-routing` still accepts `infer_addressing` for
compatibility, but it no longer controls Chat dispatch. The one-time
migration of the old preference to Space preferences remains.

## Settings

| Group | Pages |
| --- | --- |
| **You** | Profile, Model connections, Model choices, Runners |
| **This computer** | Web search, Appearance |
| **Metro** | Monitoring, About & updates |

**Model connections** manages connections and their models. **Model
choices** holds use-case routing and search model choices; it no longer
has a Personal Chat classifier switch. Space classification still belongs
to each person's preferences in that Space. Provider groups in model
pickers can collapse: Metro remembers the groups, keeps the selected
model's group open, and expands matches during search. Appearance has
theme controls but no preview gallery.

On a server-stored connection that supports custom endpoints, its owner
can open **Endpoint → Change… → Test and save**. The new URL must exactly
match a URL approved by the operator in `METRO_PROVIDER_ALLOWED_BASE_URLS`.
**Test and save** sends a test request to that candidate URL **before**
saving, using the connection's configured API key if it has one. Use
only a destination you trust with that key. A failed test or save can
leave the old endpoint in place, but it can't take back a key the
candidate already received. Metro refuses a stale edit instead of
overwriting a newer change. A failed model-list refresh after a
successful save doesn't undo the endpoint change.
Changing a shared connection changes where borrowers' requests go, without
giving them access to the endpoint URL or credentials. This is not a change
to device-connection or web-search provider authentication.

See [Web search](web-search.md) for its switches and privacy implications,
[Updates](updates.md) for **About & updates**, and [Pages](pages.md) for
Page setup and organization limits. Organization administration is still
in the server's `/admin` panel, not the desktop's personal Settings.

## Space settings

Open **Space settings** from the room header or the Space's sidebar
menu. It opens over the current conversation without replacing its
draft. Every member gets these pages:

| Page | What you can do |
| --- | --- |
| **General** | See the name and description. The owner can edit them or delete the Space; other members can leave. |
| **Members** | See the roster. Only the owner can add or remove members. The owner's room-header **Members** shortcut and the old **Manage** entry open this page. |
| **Runner** | See the current assignment and use **Test runner** from an open room in the Space. The test lists files without calling a model. Only the owner can change the assignment. |
| **Agents** | See the Space's agents, instructions and tools. Only the owner can edit them or change the default. |
| **Your preferences** | Choose your own model and routing preferences, or enable classification for this Space with a usable System One connection. |

**Test runner** posts the file listing as **Runner activity** in the
open Space room. People who can read that room can see it; it isn't
private to the person who opened Settings.

Under **General → Danger zone**, **Delete Space…** warns that deletion
is permanent and includes Space Pages, their history and drafts. Only
the owner can delete the Space, and they must type its exact name.
Unlike [deleting an individual Page](pages.md#deleting-pages-and-spaces),
there is no Trash recovery.

### Guarded runner changes

On **Space settings → Runner**, assignment and online status are separate.
Wait for a confirmed assignment before changing it. **Attach** requires
an empty Space. To switch runners, first detach the registered runner
or revoke the dedicated token. Detaching leaves the runner's other
Spaces and on-disk files alone. Find dedicated token controls and **Local
tools** under **Advanced: dedicated token and local tools**; manage a
registered runner's own token under **Settings → Runners**.
The older **Local tools** pairing is separate from the installer's
registered runner. It won't replace an existing assignment. Turning it
off won't revoke a newer or different pairing.

- **Issue dedicated token** requires an empty Space. **Rotate…**,
  **Revoke…**, and **Detach…** act only on the assignment that you
  reviewed. If it changed, reload and review it before trying again.
  A server that doesn't support a guarded change doesn't get an
  unconditional fallback request.
- Rotation invalidates the old credential immediately. Already claimed
  jobs under the old credential fail; unclaimed jobs can still go to
  the same runner after it reconnects with the new token. Detaching,
  revoking or replacing an assignment drops the old queued and claimed
  work; it doesn't pass that work to a different runner.
- A new dedicated token is shown once, masked until revealed, only
  after Metro confirms the same committed assignment and while you
  stay on its Runner page. Save it privately. If a response was lost or
  confirmation failed, the change may already have happened. Check
  current status before retrying; you can't retrieve an issued token.

For API clients, the guarded `POST` paths under `/v1/spaces/{sid}` are
`/runner/attach-if-empty` (with `runner_id`) and
`/runner-token/issue-if-empty` (with `{}`), plus
`/runner/detach-if-current`, `/runner-token/rotate-if-current`, and
`/runner-token/revoke-if-current`. The last three take the owner-visible
`assignment_revision` from `GET /v1/spaces/{sid}/runner` as a decimal
string. A stale revision or occupied slot is a conflict, not permission
to use an older write route. Keep the [0.4.0 pairing instructions](spaces.md#runner-pairing)
for published 0.4.0 clients.

## HTTP, Tailscale, and proxies

Prefer HTTPS for a remote server, or use an SSH port forward. Plain HTTP
on an ordinary network does not encrypt tokens, messages, or files. These
source changes do not alter the 0.4.0 [deployment instructions](deploy.md).

- **Desktop:** Direct loopback (`localhost`, `127.0.0.0/8`, `::1`) and
  direct Tailscale (`100.64.0.0/10`, `fd7a:115c:a1e0::/48`, or a
  `*.ts.net` MagicDNS name) don't prompt. Other plain-HTTP addresses require
  confirmation before Metro sends a token. A loopback or Tailscale address
  also requires confirmation if the app cannot tell that the route is
  direct. The choice is for that exact server address and is remembered
  with a saved server; forgetting the server removes the choice.
  On the startup connection screen, the form scrolls to reveal the
  warning and both actions in short windows. Focus moves to **Cancel**.
  **Cancel** or Escape returns focus to the field or button that opened
  the warning. **Connect anyway** explicitly proceeds.
- **Proxies:** The desktop keeps your proxy settings and only adds exact
  loopback entries to its exemptions. It does not silently add tailnet
  hosts or bypass your proxy for ordinary hosts. If your environment uses
  a proxy, route a trusted tailnet server directly by putting its exact
  host in `NO_PROXY` (`[IPv6]` for the desktop launcher). Don't rely on a
  Tailscale-looking name or address to encrypt a path outside your tailnet.
- **Web admin:** Open `/admin` directly in a top-level browser tab.
  Outside loopback and the Tailscale host forms above, plain HTTP requires
  confirmation before sign-in. The browser remembers the choice per
  origin where storage is available; the panel lets you forget it. The
  panel can't inspect the browser's proxy route. Use HTTPS or verify the
  connection goes directly over a trusted tailnet before entering an admin
  token. The token is held in the tab's memory, not in that stored choice.
  Framed pages and origins that aren't HTTP or HTTPS still can't sign in.
- **Runner, connector and `metro-tui`:** They warn on other plain-HTTP
  server addresses rather than prompting. The runner still accepts
  `METRO_ALLOW_INSECURE_HTTP` for compatibility but no longer needs it.
  Runner and connector server requests go directly to loopback or
  Tailscale and otherwise respect their configured proxy. `metro-tui`
  doesn't use proxies. These rules are for the **Metro server URL**:
  they do not relax device model endpoint rules or provider allowlists.

## This computer's runner

The installer's personal runner is for the bundled local workspace. It
only lists and reads files under each Space's own folder; it doesn't run
shell commands or MCP servers. It is distinct from the **Local tools**
switch. **Settings → Runners** shows its status and attached folders,
along with the personal and organization runner lists.

- **Create Space** (including the first-Space form) shows **Use this
  computer's runner**, checked initially, only when Metro has verified an
  eligible local runner for the current account. Uncheck it to create the
  Space without a runner. Even with it checked, creation and attachment
  are separate: if attachment fails or cannot be confirmed, the Space
  still exists. **Open Runner** opens that Space's **Runner** settings
  so you can check the assignment before retrying.
- When the installed runner is online and you own Spaces with no runner,
  the sidebar can offer to attach it. **Choose Spaces…** opens
  **Settings → Runners → Spaces without a runner**; **Not now** dismisses
  the offer for this account and server without changing any Space.
  The first verified list shown in a Runners visit starts checked.
  Newly eligible Spaces appear unchecked. If a Space loses eligibility,
  it loses its check and won't be checked automatically if it returns.
  Changing the runner or session clears the selection. Review the
  checked Spaces before pressing **Attach to N Spaces**.
  You can still use Settings later. A confirmed new-Space attachment
  also dismisses the sidebar offer.
- This attach path only fills an empty assignment. The server rechecks
  ownership and emptiness when attaching, including any dedicated token
  or registered runner assigned in the meantime. Offline is not the same
  as unassigned. A server without the guarded attach route is not given a
  fallback request that could replace an assignment.
- Reconnecting an existing personal registration rotates its token and
  keeps its attachments. A new registration starts unattached. See
  [Updates](updates.md#runner-and-server-updates) for how each runner type
  is updated.

## Mentions in rooms and threads

Type `@` to look up a member by name or stable ID, or an agent by name,
handle or ID. Use the arrow keys and **Tab** or **Return** to select a
suggestion; this inserts a mention, not a message. If the text matches
multiple people or agents, pick the intended recipient before sending.
An unmatched mention stays text. If a previously bound recipient is
no longer available or a typed match has changed, Metro asks you to
review it rather than silently addressing someone else.

An addressed person alone doesn't start an agent. A named agent starts
directly; `@agent` is still the generic request. **Ask agent** can add
an agent request to explicit mentions. The per-Space classification
preference still applies to unaddressed text. **Send** never adds an
explicit agent beyond the draft's verified mentions.

Sent mentions with verified ranges display as chips, using current
names for people and named agents and a stronger highlight for
mentions of you. Open a chip that identifies a current person or named
agent to see their card. Generic `@agent` is not a specific identity
and doesn't open a card. Metro uses stored recipient IDs, not display
names, as identity. Older messages without mention ranges can still
show their recorded recipients; unavailable mentions aren't clickable.

## Recipient-safe Dispatch and exact-destination Follow up

Personal Chat's **Agent runs** panel lists runs in Spaces you belong
to, including threads, not runs in Personal Chat itself. The Dispatch
form (**New task** and **Follow up**) already exists in 0.4.0. The
changes here are how it addresses a task and verifies its destination:

- **New task** and **Follow up** ask the generic agent in the selected
  Space room or thread. Dispatch is not a mention composer: text such
  as `@name` doesn't add recipients.
- If **Follow up** would change the destination of a nonempty task
  draft, **Keep draft** leaves it where it is. **Move draft** changes
  the destination only after Metro verifies the new room or thread.
  Neither choice sends anything or changes the draft text.
- **Follow up** checks the run's exact room or thread even if it's
  already selected or in the room list. A cached entry isn't enough
  to enable **Send to agent** while the check is pending or after it
  fails. Try **Follow up** again or choose another destination yourself;
  Metro doesn't quietly substitute a different room. Check the **To:**
  line before sending.

## Page consent and Web Activity

- **Approve version and run** grants approval for the whole Page
  version but runs only the action you clicked. The prompt shows the
  clicked action and lists up to four other available agent or schedule
  button labels, with a count if more are available. The list is not
  the scope of the approval. See [Page actions](pages.md#who-can-see-edit-and-run-things)
  for the checks that still apply to each click.
- Web Activity labels provider attempts **saved key** or **no key**;
  a skipped provider may say it **requires an API key**. If all attempts
  fail to produce usable output, Activity says **No provider returned a
  usable result.** Look at the individual outcomes rather than treating
  that summary as a rate-limit diagnosis. A key does not guarantee
  better limits. See [Web search](web-search.md#what-metro-keeps-in-conversation-activity).

## Private sender notices

After a message appears, its sender may see **Only you see this** on
that message. It can carry an addressing notice, a routing or model
notice, or both on the same message. For unaddressed Space text that
the classifier couldn't process, no agent starts. **Ask agent** prepares
a generic mention in an empty draft or focuses the existing draft. It
doesn't resend anything. A failed generic
`@agent` choice still has a public room error and doesn't cancel
separately named agents. A Space routing notice describes one refused
turn; it doesn't mean every agent named in the message failed.

Notices are attached only after the desktop matches the POST response
to the sender's actual transcript row. They belong to the original
room or thread (including Dispatch sends) or Personal Chat, never to the
view you happen to open next. **Settings** on the notice opens the
relevant Space preferences, Model choices or Model connections. Other
room members may see a general error, not the sender's private
connection details.

The desktop can read routing-only POST metadata for direct Personal
Chat sends, but this server reports Chat turn-start failures as errors
in the private Chat itself; its Chat POST supplies neither notice.
**Save note** does not start a turn or produce one of these notices.
This notice path doesn't apply to automated schedules or Page actions.

The details are sent only in the originating POST response. They aren't
stored as events, returned by transcript reads, or included in search or
model history. The app holds them in the sending session only. A notice
can reappear on returning to its room in that session while the
matching message remains loaded. Dismissal, reload, sign-out, an
account or workspace switch, and loss of access clear it as
applicable. A transcript read alone never creates a notice.

## Space send protocol (client developers)

The existing [room-message endpoint](spaces.md#messages-and-addressing)
accepts `infer_text_mentions: false` to turn off legacy text inference
for that send. The desktop uses it for room, thread and Dispatch sends
and doesn't retry with legacy text inference. Explicit structured
`mentions` and `invoke_agent` then address
the message; the sender's opt-in Space classification can still apply to
an otherwise unaddressed message. Optional `spans` entries have `start`
and `end` UTF-8 byte offsets (end exclusive) in the untrimmed decoded `text`
and an `index` into the submitted `mentions`. The server validates and
normalizes the ranges against the stored message and recipients before
returning them as event `spans`.
Legacy clients can omit both fields. Modern sends can return an empty
`spans` array to distinguish “no verified ranges” from a legacy event
with no `spans` field.

Successful Space message POSTs can return either or both sender-only
fields alongside the posted event: `addressing_note`
(`classification_failed` or `generic_agent_unavailable`) and
`routing_note` (`routing_failed` or `model_unavailable`). Handle these
as response metadata, not as fields of a polled event. If an older server
rejects safe addressing, use matching prebuilt client and server files
when available. If a reply was lost, check the room before resending.

## Starting the desktop

For a saved remote server, Metro can show the sidebar after verifying
your account and Space list while the remembered room still loads.
It doesn't show a cached transcript before verification, and a new
navigation choice wins over the late startup response. The local
launcher and desktop also reuse some overlapping startup reads; they
don't share model results across accounts or Spaces. A remote workspace
still requires an available server; this doesn't add offline reading.
See [startup fixture evidence](acceptance.md#source-only-interim-gate-and-startup-evidence)
for what was measured.

[release]: https://github.com/wnzn/metro/releases/tag/v0.4.0
