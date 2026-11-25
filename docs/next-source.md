# In the source, not released

This guide describes the Metro source at `d82c07f`. None of the changes
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
  still exists. Use **Open Runner** to check the assignment before retrying.
- When the installed runner is online and you own Spaces with no runner,
  the sidebar can offer to attach it. **Choose Spaces…** opens
  **Settings → Runners → Spaces without a runner**; **Not now** dismisses
  the offer for this account and server without changing any Space.
  Review the checked Spaces and press **Attach** to make the change.
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

## Space send protocol (client developers)

The existing [room-message endpoint](spaces.md#messages-and-addressing)
accepts `infer_text_mentions: false` to turn off legacy text inference for
that send. Explicit structured `mentions` and **Ask agent** then address
the message; the sender's opt-in Space classification can still apply to
an otherwise unaddressed message. Optional `spans` entries have `start`
and `end` UTF-8 byte offsets (end exclusive) in the untrimmed decoded `text`
and an `index` into the submitted `mentions`. The server validates and
normalizes the ranges against the stored message and recipients before
returning them as event `spans`.
Legacy clients can omit both fields.

A failed classification of unaddressed text starts nothing. The POST
response can carry an `addressing_note` with `kind: "classification_failed"`
and a reason. This is returned only to the sender, isn't stored as an
event, and isn't sent on subsequent reads. An explicit generic `@agent`
failure still produces a room-level error and doesn't cancel separately
named agents. This checkout has server support and pure draft-resolution
code, **not** desktop composer wiring for name resolution, sent mention
chips, or sender-only notices.

[release]: https://github.com/wnzn/metro/releases/tag/v0.4.0
