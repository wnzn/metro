# Settings: model connections, appearance and org admins

The desktop app's **Settings** opens as a window over the app, with five
sections in this order (as verified in the 0.2.0 Linux candidate):

| Section | What it's for |
| --- | --- |
| **Profile** | Your picture, display name and bio, and your **User ID**; see [spaces.md](spaces.md#profiles) |
| **Runners** | Your registered runners; see [runners.md](runners.md) |
| **Model providers** | Your model connections, described on this page |
| **Model routing** | Opt-in use-case routing and classification; see [system-one.md](system-one.md#how-a-message-reaches-the-agent) |
| **Appearance** | Color scheme, [below](#appearance) |

In the current source after 0.2.0, which isn't released, **Settings** is still a
sidebar entry with these sections. **Model providers** also holds
**Documents & media**, where you choose the media model that indexes and
searches your files ([library.md](library.md#in-the-desktop-app)). That
choice was checked in a native Linux GTK run, in dark and light themes, of the unreleased source after 0.2.0. Your account is in the sidebar's account row.
Its menu offers **Copy server URL**, **Copy ID** and **Sign out**. The
account row was checked in the same native GTK review, and its menu items
are covered by automated tests.

This page also covers the server settings that operators set behind them.

## Adding a connection in Settings → Model providers

As seen in the 0.2.0 Linux candidate:

- **Your connections** comes first, with each one's capability, whether
  it's your default, whether it's shared, and whether it runs on this
  device or uses a saved key.
- **Add a connection** then asks:
  - **For:** All, Chat, System One or Embeddings, which narrows the list.
  - **Provider:** one dropdown of what this server supports, grouped by
    vendor and searchable (**Search providers**). Each vendor is one row
    showing how many ways it can be connected and its capabilities. For
    example, OpenAI is one row with three ways to connect, covering Chat
    and Embeddings. Vendor marks are bundled with the app (monochrome SVGs
    from `@lobehub/icons-static-svg`, MIT) and never fetched over the
    network.
  - **Connect <vendor>:** the ways to connect that vendor, each with its
    capability and a **Choose** button. Ways on your device (your own
    sign-in or key stays on your computer) are preferred. Ways stored on the
    server use a key encrypted there.

The ways map to the provider ids below: the `device-*` kinds run on your
device, and the others are [server-stored API profiles](#server-stored-api-profiles).
The [older screenshot](screenshots/settings-providers.png) is from a
development build, before the vendor dropdown.

## Which model a run uses

Model connections are **personal**. Each person can save up to 20, and mark
one as their **default**. A connection needs a model before it can be the
default.

- Every agent run uses the default connection of **the person who started
  it**. That holds in personal Chat, in Space rooms, and in the child threads
  that run delegates to. It never falls back to another member's
  connection. A shared connection is used only when someone picks it
  ([sharing](#sharing-a-connection)).
- If that person has no default connection, the run uses the operator's
  `METRO_MODEL` environment model, as before. If neither exists, Ask agent
  reports that no model is configured.
- If the default connection can't be used (a device that's offline, a key
  that can't be decrypted, a revoked device token), the run **fails with a
  clear error**. It never quietly switches to `METRO_MODEL` or to anyone
  else's connection, so the operator's model is never billed for it.
- The first connection you save with a model becomes your default if you
  don't have one yet. You can change the default at any time.
- **Per-message choice in personal Chat.** A message to the agent in your own
  Chat can name one of your other connections for that one reply
  (`provider_connection_id` on `POST /v1/chat/messages`). It doesn't change
  your saved default.
  - The choice only works on a message addressed to the agent (`@agent` or
    `invoke_agent`). Otherwise it's a `400`.
  - Someone else's connection, or one that doesn't exist, gets `404`, and
    nothing is posted.
  - If the chosen connection can't be used, that reply fails. It doesn't fall
    back to your default or to `METRO_MODEL`.
  - Space rooms and threads always use the initiator's default.
- *Demo* never calls a model.

Connections are private unless their owner [shares](#sharing-a-connection)
one. Your defaults are always your own connections.

## Capabilities

Every connection has one fixed **capability**, set by its provider. You get
**one default per capability**.

| Capability | Provider ids | Used for |
| --- | --- | --- |
| `chat` | The chat adapters above, `custom`, `device-openai-oauth`, `device-claude-code`, `device-openai-compatible` | Agent replies and tool turns |
| `system_one` | `systemone` (TypeSafe's hosted API, `https://api.typesafe.ai/v1`, your own TypeSafe key), `custom-systemone` (the System One wire on an operator-approved endpoint, key optional), `device-systemone` (on your own machine through `metro-connector`) | Typed decisions, such as the [search rerank](system-one.md#semantic-search) |
| `embeddings` | `openai-embeddings` (OpenAI's `POST /v1/embeddings`, your own key), `custom-embeddings` (OpenAI-compatible embeddings on an operator-approved endpoint), `device-embeddings` (on your own machine) | [Semantic search](system-one.md#semantic-search) |

- A connection is never used for a capability it doesn't have. A System One
  or embeddings connection never answers a chat turn.
- On a device, the connector runs only the operation that matches the
  connection's kind.
- `custom-*` base URLs must match `METRO_PROVIDER_ALLOWED_BASE_URLS` exactly,
  like `custom`.
- `GET /v1/providers` shows each connection's `capability`.

## Sharing a connection

A connection is **private** until its owner shares it. Only the owner can
view or change the share. Organization admins can't change other people's
connections.

| Visibility | Who can use it |
| --- | --- |
| `personal` (the default) | Only you |
| `spaces` | Members of the Spaces you select (1 to 32, each one a Space you're in), inside those Spaces only, and only while **both you and they** are still members |
| `organization` | Every signed-in user of this server, anywhere, including their personal Chat |

**What a borrower gets.**

- **Use only.** Borrowers never see the base URL, key, sign-in or pairing
  token, and they can't manage, share or delete the connection.
- **The owner sees and pays.** A borrowed connection runs on the owner's
  endpoint, key or device. The owner can see those requests, and may be
  billed for them. The model list marks these entries `runs_on:
  "owner_connection"` and `billing: "owner"`.
- **Rechecked on every use.** Revoking a share, removing a member or
  deleting the connection stops the next request. On the server and on
  devices, pending and claimed work that's no longer allowed is refused, and
  its results aren't accepted. A connection someone isn't allowed to use
  answers exactly like one that doesn't exist (`404`).

**Sign in with ChatGPT is never shared as an account.** Sharing a
`device-openai-oauth` connection publishes only a **bring-your-own preset**:
the model name, with no credential (`mode: "preset"`,
`runs_on: "your_own_sign_in"`, `billing: "you"`).

- **It runs only on your own Sign in with ChatGPT connection.** If you have
  one, it runs there, preferring one already set to the preset's model, on
  your own device and your own plan. It's never run on the owner's account.
- **It never falls back to an API key.** If you don't have Sign in with
  ChatGPT, the preset fails before any call, even if you have a working
  OpenAI API-key connection. In the model list it shows `ready: false`. The
  error asks you to add Sign in with ChatGPT, or to pick your own OpenAI
  connection yourself if you want to use your API key.
- **Revoking stops it at once.** After the owner revokes the share, the next
  request never reaches your device, and an answer your device was still
  producing is withheld.

| Request | Result |
| --- | --- |
| `GET /v1/providers/{id}/sharing` | Owner only: `{"visibility", "space_ids", "mode": "owner_connection"\|"preset", "capability"}` |
| `PUT /v1/providers/{id}/sharing` `{"visibility": "personal"\|"spaces"\|"organization", "space_ids"?}` | Owner only. This replaces the share. `space_ids` is needed only for `spaces`. Sharing needs a chosen model first (`409`), and a Space you aren't in gets `400`. |
| `GET /v1/models/available` | The models you can use in your personal Chat: your own, plus organization shares |
| `GET /v1/models/available?space_id=SID` | The models you can use in that Space (`404` if you aren't a member) |
| `GET /v1/models/available?scope=any` | For pickers only: everything you could use in any of your Spaces. Each run still checks its own scope. |

Each model entry includes `connection_id`, `name`, `model`, `provider_id`,
`capability`, `visibility`, `owned`, `owner_id`, `execution`, `runs_on`,
`billing`, `is_default`, and `ready` for presets. An entry never includes a
key, base URL or token.

**After a share is revoked or changed**, a request that has already gone to
the provider can't be unsent. Its answer, or its vectors for embeddings,
are withheld instead. This applies to server-stored connections, System One
and embeddings included, and to device connections.

**What's been tested.**

- Capabilities, per-capability defaults and sharing pass component tests
  over real HTTP
  ([acceptance.md](acceptance.md#model-capabilities-and-sharing-component-tests-2026-11-22-providers-lane-local)).
- `tests/e2e/intelligence.py` passed with real server and connector
  processes, using shared local and device chat and embeddings connections
  across users, revocation included
  ([acceptance.md](acceptance.md#demonstrated-classification-routing-sharing-and-search-end-to-end-2026-11-22-integrator-local)).

Both used loopback fixtures. No real commercial provider has been tested:
not TypeSafe, not OpenAI embeddings, and not a shared real account.

There are two kinds of connection, and they handle credentials very
differently.

| | Device connection | Server-stored API profile |
| --- | --- | --- |
| Provider ids | `device-openai-oauth`, `device-claude-code`, `device-openai-compatible` | The adapters in this server's build, from `GET /v1/provider-catalog` |
| Who calls the model | The on-device integration on **your** computer (in this build, the separate `metro-connector` process) | The Metro server |
| Where the vendor credential lives | Your computer only. Metro never receives it. | The server's database, encrypted with the operator's master key |
| What the server stores | The connection's id and name, the chosen model, and a hash of its device token | The same, plus the encrypted key and the base URL |
| Needs your computer online | Yes | No |

A Space runner is never used for model access. Runners only serve workspace
tools.

## Device connections

With a device connection, inference runs on your own machine (a laptop, or an
always-on personal machine), using your own vendor sign-in or your own API
key. The server relays each request to your machine and the answer back.

> **Status.** Ordinary users only install the desktop app. The packaged
> desktop app includes a device bridge that serves device connections while
> it's open, so there's no separate program or terminal. A packaged smoke of
> that bridge with the bundled server passed. In the packaged app you pair
> with **Connect this device**, and later use **Manage connection**.
> Verified in a packaged Linux build against a local test endpoint
> ([acceptance](acceptance.md)): you consent before pairing, you never see
> or enter a device token, only the models you approve are offered, and
> replies kept working after signing out and after a restart. No real
> vendor account or vendor sign-in (such as Sign in with ChatGPT) has been
> tested, and nor has native Windows. The standalone `metro-connector` CLI remains
> contributor tooling
> ([development.md](development.md#metro-connector-developer-cli)).
>
> Vendor software stays separate. Metro never installs, updates or signs in
> to tools such as the Claude Code CLI for you.

**What Metro sees.** The server never receives your vendor credential, the
vendor's base URL or your list of approved models. It does see the content of
each request and response it relays: the conversation, tool definitions and
the model's reply. That content isn't end-to-end encrypted, and it's held in
memory only while the job is open. Tool calls in a reply are run by Metro's
agent loop, on the Space's runner as usual, and never on your device.

### Create the connection and its device token

In **Settings → Model providers**, add a connection with a way to connect
that runs on your device (a `device-*` provider). In the packaged app,
click **Connect this device**; the app pairs itself and never shows you the
token. Development builds without the launcher instead offer **New device
token**. The token starts with `metro_pc_`. It's shown **once**, masked
unless you click **Show**, and the server keeps only a hash. Only the
connection's owner can create, replace or revoke it. Creating it again
replaces the old one, and revoking it or deleting the connection fails that
connection's pending jobs.

Once paired, the button becomes **Manage connection**. Earlier development
builds showed a **Set up metro-connector** guide instead
([screenshot](screenshots/settings-device-onboarding.png)). The app never
signs in to a vendor CLI for you. **Test
connection** lists models once the device is online. Don't paste the token
into a command line or a chat.

Over the API, the same steps are `POST /v1/providers` with
`{"name", "provider_id": "device-…"}`, then
`POST /v1/providers/{id}/device-token` `{}` (`201 {"token"}`). A device
connection takes no `base_url` or `api_key`.

**You approve the models.** Metro can only use the model ids approved on
your machine. It can't add a model, change the base URL or choose the
endpoint. A request for an unapproved model is refused on your machine.

### When the device is offline

The server treats a connector that hasn't polled for 45 seconds as offline.
`GET /v1/providers/{id}/device-token` shows `paired`, `online` and
`last_seen_secs`. A run on an offline, busy or unpaired device fails at once
with an error that says so, for example *"your device connector is offline;
start metro-connector on your computer and try again"* (the wording of this
build). A request the device
picks up but doesn't answer within the 300-second job limit fails too.
Nothing is retried on another machine or another person's connection.

### Kind: `device-openai-oauth` (Sign in with ChatGPT)

This kind uses OpenAI's **direct** Sign in with ChatGPT flow for ChatGPT plan
usage by open-source, locally run apps
([overview](https://developers.openai.com/siwc/token-sharing-open-source)).
It doesn't use Codex app-server, never reads `~/.codex`, and never runs a
model CLI.

**Eligibility, read this first.**

- OpenAI describes this capability as a **preview** for open-source apps that
  run locally on the signed-in user's own device.
- Paid or remotely hosted apps should complete OpenAI's
  [interest form](https://openai.com/form/sign-in-with-chatgpt-interest/).
  Nothing in Metro means your deployment is pre-approved.
- Metro doesn't claim that your deployment qualifies, or that Metro is
  published under an open-source license. Decide that yourself against
  OpenAI's terms. A shared Metro server relays the requests, even though the
  sign-in stays on your machine.
- Your plan or workspace policy can still refuse it
  (`subscription_sharing_user_not_eligible`).
- Usage counts against **your ChatGPT plan's own limits**, which OpenAI sets.
  Metro doesn't promise any amount of usage. You can check yours in
  [ChatGPT usage settings](https://chatgpt.com/settings/usage).
- When a limit is hit, the run fails. It never moves to another billing
  path.

**Sign in.** In this build, the connector's setup starts sign-in
([commands](development.md#metro-connector-developer-cli)).

1. The connector prints a one-time `http://127.0.0.1:<port>/auth/start?…`
   link and tries to open your browser. It tries port 1455 first, then any
   free port.
2. The link redirects to OpenAI. You sign in to ChatGPT and approve the
   agent, which is registered as "Metro". You can rename it on first sign-in.
3. OpenAI sends the browser back to the connector's page on `127.0.0.1`. The
   connector waits up to 10 minutes.

The browser must run **on the same machine as the connector**, because the
callback only reaches `127.0.0.1`. Signing in a remote or headless machine
isn't supported.

**What it checks.**

- It uses dynamic client registration (a public client with no secret),
  PKCE (S256), and a fresh `state` and `nonce` on every attempt.
- It verifies the ID token's signature and claims.
- A sign-in that didn't grant plan usage is reported. Nothing is sent to
  inference until a later sign-in grants it.
- If you decline consent, sign-in fails with an error.
- One profile holds one ChatGPT account. Signing in to a different account
  is refused, so add a new connection for it instead.

**Tokens on your machine.** The tokens are kept in the profile's `vendor/`
directory (mode 700, files 600). The access token is refreshed shortly before
it expires, and the rotating refresh token is replaced each time. If the
refresh token is rejected, the tokens are cleared and you're asked to sign in
again. Signing out on the device asks OpenAI to revoke the refresh token and
deletes the local tokens. If OpenAI doesn't confirm the revocation, the
connector says so. Then disconnect Metro in ChatGPT's settings.

**Models and requests.**

- Listing models asks OpenAI for your account's models.
- Requests go to the Responses API with `store: false`, and only a completed
  response counts as success. Tool calls come back to Metro, and the
  connector runs no tools itself.
- Request options that this route doesn't support are **rejected**, never
  silently dropped. These include `temperature`, `top_p`, `max_tokens`,
  `stop`, `seed`, the penalties and `n` > 1.

### Kind: `device-claude-code` (Claude, local CLI)

This kind runs **your own installed Claude Code CLI** (version 2.1.286 or
later, unmodified) with **your own Anthropic Console API key**. Usage is
billed to that Console account at API rates.

Metro doesn't offer Claude.ai, Pro or Max sign-in, and doesn't route requests
through subscription credentials. Anthropic doesn't allow third-party apps to
do that ([Agent SDK overview](https://code.claude.com/docs/en/agent-sdk/overview),
[legal and compliance](https://code.claude.com/docs/en/legal-and-compliance)).
The connector never runs `claude auth login` or `claude setup-token`, never
reads the CLI's saved credentials or your keychain, and refuses
`sk-ant-oat` subscription tokens.

Claude Code is vendor software. Install and update it yourself from
Anthropic; Metro never installs it.

**Set up.** Create a key in the [Claude Console](https://platform.claude.com)
under Settings → API keys. In this build you enter it in the connector's
setup ([commands](development.md#metro-connector-developer-cli)).

- Enter the key at the hidden prompt, or set `METRO_CLAUDE_CODE_KEY_FILE` to
  an owner-only file, or to `-` for stdin.
- The connector finds `claude` on `PATH`, or uses the absolute path in
  `METRO_CLAUDE_CODE_BIN`, and checks its version.
- It checks the key with Anthropic's model list, which runs no model.
- The key is saved as a mode-600 file in the profile's `vendor/` directory.
  `logout` deletes it, and never touches your own `claude` login or
  `~/.claude`.

**How a request runs.**

- Each completion starts the CLI for inference only, with `--bare` and
  `--restricted`.
- There are no built-in tools, MCP servers, skills, hooks, saved sessions or
  permission prompts.
- The CLI runs in an empty working directory, with a private `HOME` and
  config directory, and a cleared environment that holds only this key. The
  conversation goes in on stdin.
- When Metro offers tools, the CLI is asked for structured output: either a
  text reply, or a list of calls to the offered functions. The connector
  checks every name and argument against the offered schemas, then hands the
  calls back to Metro, which runs them. The CLI never executes them.

### Kind: `device-openai-compatible`

Your own OpenAI-compatible endpoint, such as a local inference server or a
gateway. Use it with your own key, or `--no-api-key`. The base URL is set
on your machine only.

### What has been tested

Only local fixtures have been tested, never a real vendor account.

- The full device path passed end to end with real binaries:
  `metro-server`, `metro-connector`, and a loopback OpenAI-compatible
  vendor. That covered pairing, online status, a Chat turn with a tool
  round trip, model listing, token rotation, another user's device being
  refused, and the vendor key never reaching Metro's database or logs. See
  [acceptance.md](acceptance.md).
- The OpenAI flow passed 15 tests against fake OIDC, token, model and
  streaming endpoints. Those included a two-tool round trip in the request
  shape that Metro's Rig agent sends, which the Sign in with ChatGPT
  translation accepts.
- The Claude adapter passed 14 tests against a stub CLI and a stub Anthropic
  API.
- The connector passed nine checks against loopback Metro fixtures.

No live ChatGPT sign-in, real Claude Code run or paid inference has been
recorded. Nobody has checked with real accounts how the account email and
`sub` stored in the OpenAI profile, or a real Console key, are handled.
Treat these kinds as unverified with real accounts until
[acceptance.md](acceptance.md) says otherwise.

## Server-stored API profiles

These are optional. The server calls the provider, using an API key you
enter once.

- The providers you can pick are the adapters compiled into this server's
  Rig build that Metro can drive with an API key and its agent tools, as
  listed by `GET /v1/provider-catalog`:
  - the OpenAI family, the Anthropic Messages variants, and native
    `gcp.gemini`;
  - `custom` (OpenAI-compatible Chat Completions);
  - adapters for local or private endpoints, such as Ollama, llama.cpp and
    Azure OpenAI. These appear only when the operator has set
    `METRO_PROVIDER_ALLOWED_BASE_URLS`;
  - the three `device-*` kinds.

  The response also lists `allowed_base_urls`. The catalog is built from
  compiled tables only. Reading it never contacts a vendor or a device, and
  Metro lists nothing it can't run. Being listed doesn't mean an adapter has
  been tested against that vendor with a real account.
- Keys are **write-only**. The API never returns them, only `has_key`, and
  they're never logged. At rest they're encrypted (XChaCha20-Poly1305) and
  bound to the owner and the connection, so a key copied to another row or
  another user won't decrypt.
- A base URL is either the provider's own public HTTPS endpoint, or an exact
  entry of the operator's `METRO_PROVIDER_ALLOWED_BASE_URLS`. Requests never
  follow redirects. That's how a self-hosted OpenAI-compatible endpoint is
  allowed.
- `POST /v1/providers/{id}/models` asks that connection's provider (or your
  connector, for a device connection) for its current models. The list comes
  back sorted, without duplicates, and capped at 1000 ids. If listing isn't
  available or fails, type the model id yourself.

| Request | Result |
| --- | --- |
| `GET /v1/provider-catalog` | What this server can connect to |
| `GET /v1/providers` | Your connections: `id`, `name`, `provider_id`, `base_url`, `model`, `is_default`, `has_key`, `execution` |
| `POST /v1/providers` `{"name", "provider_id", "base_url"?, "api_key"?, "model"?, "is_default"?}` | `201` with the connection. With a model, and no default yet, it becomes your default. |
| `PATCH` / `DELETE /v1/providers/{id}` | Change or delete your connection |
| `POST /v1/providers/{id}/models` | List the provider's models: server-side for API profiles, through your connector for device connections |
| `POST` / `GET` / `DELETE /v1/providers/{id}/device-token` | Device connections only: create or replace, show pairing and online state, revoke |

Only the owner can see or manage a connection. Other people's ids get `404`.
A share lets someone else *use* it ([sharing](#sharing-a-connection)), but
never see or manage it.

## Operator settings

| Variable | Meaning |
| --- | --- |
| `METRO_PROVIDER_MASTER_KEY` | Base64 (standard, padded) of exactly 32 random bytes. It encrypts stored API keys. |
| `METRO_PROVIDER_KEY_FILE` | The same value, read from a file. Set only one of the two. The server warns if the file is readable by others, or if it sits beside the database. |
| `METRO_PROVIDER_ALLOWED_BASE_URLS` | JSON array of the exact custom base URLs people may use, for example `'["https://llm.internal.example/v1"]'` |
| `METRO_ORG_ADMINS` | JSON array of stable user ids, see [below](#organization-admins) |
| `METRO_MODEL`, `METRO_MODEL_BASE_URL`, `METRO_MODEL_API_KEY` | The fallback model for people with no default connection |

**Without a master key**, nobody can save an API key (`409`), and keyless and
device connections still work. **If the master key changes or is lost**, saved
keys can't be decrypted. Runs on those connections fail with *"cannot be
decrypted with this server's master key; enter the key again"*, and each
owner has to re-enter their key. Back the key up separately from the database
volume, so that a copy of the data doesn't carry its own key.

Create a key file without printing it:

```sh
mkdir -p "$HOME/.config/metro" && chmod 700 "$HOME/.config/metro"
( umask 077; openssl rand -base64 32 > "$HOME/.config/metro/provider-master.key" )
```

With Compose, use the override that mounts it as a secret. The file must be
readable by the server's UID 10001, for example after `chown 10001` and
`chmod 400`.

```sh
METRO_PROVIDER_MASTER_KEY_HOST_FILE="$HOME/.config/metro/provider-master.key" \
  docker compose -f compose.yaml -f compose.provider-key-file.yaml up -d server
```

Existing databases upgrade in place. People start with no connections, so
their runs keep using `METRO_MODEL` until they choose a default.

## Organization admins

A server has one organization. `METRO_ORG_ADMINS` lists its admins by
**stable user id**: the dev username, or the OIDC `sub`, never a display
name. Duplicates are ignored. Org admins create and manage organization
runners ([runners.md](runners.md)). Being an admin gives no access to
anyone's Spaces or connections. `GET /v1/me` returns `is_org_admin`.

## Appearance

**Settings → Appearance → Color scheme** offers **Dark** (the default),
**Light** and **System**, which follows the operating system. A change applies
at once, without a restart, and is remembered on that device
([screenshot](screenshots/settings-appearance.png)). The setting
isn't secret and never leaves the device.

**Agent activity starts collapsed.**

- Each agent turn shows in the transcript as **one** collapsed group, not as
  a row per tool.
  - While the turn runs, the group reads *Metro is thinking…*, with a step
    count ([screenshot](screenshots/agent-thinking.png)). Once the turn ends,
    it reads *Metro’s activity*.
  - One click opens every step in order: each tool call with its arguments
    and result, for example *Tool · List workspace · complete*, and any
    thinking or progress summary that the model provider shared
    ([screenshot](screenshots/agent-activity-expanded.png)).
  - A failed, cancelled or interrupted turn, or a failed tool, keeps a badge
    that you can see while the group is collapsed.
  - Groups are built only from the public activity metadata on events.
    Metro shows no chain of
    thought of its own. A step counts as thinking only when the server or
    provider labels it that way.
  - A routed turn's group can show its route, for example *detected by
    System One* and the use case. It never shows connection ids, prompts or
    credentials ([screenshot](screenshots/route-attribution.png)).
- Demo never calls a model. Its call and listing show as one collapsed
  *Runner activity* group.
- People's messages, the agent's replies, errors and reactions always stay
  visible, including messages posted while a turn is running.

**Changing the look.** The design lives in one semantic layer, not in each
screen:

- `apps/desktop/theme/theme.json` holds the tokens: a `dark` and a `light`
  palette (`background`, `foreground`, `primary`, `muted`, `border`,
  `sidebar-*` and so on, plus the status tones `tone-agent`, `tone-runner`,
  `tone-success`, `tone-warning`, `tone-danger`, `tone-system` and
  `tone-neutral`), and the shared spacing, type sizes, radii and widths.
  Edit a token once, and every screen follows, in both schemes.
- `apps/desktop/src/ui/` holds the primitives that screens compose. Feature
  components pass only semantic props, such as `tone="agent"` or
  `variant="primary"`, never colors. Replacing `src/ui/` behind the same props
  re-skins the whole app.

So a redesign is mostly token edits in one file, plus changes to the
primitives if the shapes change. Screens don't need to be touched one by one.

Contributors: see [apps/desktop/README.md](../apps/desktop/README.md) for
building the app.
