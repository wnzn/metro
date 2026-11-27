# AGENTS.md

Guidance for people and coding agents who work on Metro.

## Authorship policy (build process)

This is how the work is split while building Metro. It is **not** runtime
behaviour. Metro does not route requests to these models; at runtime it uses
whatever model `METRO_MODEL` names.

| Role | Owner |
| --- | --- |
| Orchestrator: decides scope, architecture and trade-offs; assigns and directs subagents; accepts results only once they're verified. Assigns the work in the other rows to subagents and doesn't do it itself | Any model; no specific model is required |
| UI/UX decisions: what to build, and how it should look and behave | **Opus 5.5** |
| UI/UX implementation (`apps/desktop`, including `theme/` and `src/ui/`) | By complexity: **Opus 5.5** for complex work, **Sonnet 5.5** for medium, **Sol 6.2** for the most routine tasks |
| Complex code in any layer (for example security-sensitive or tricky Rust) | **Opus 5.5** |
| General tasks (routine code, tests, fixtures, chores) | **Sol 6.2** |
| Writing (README, AGENTS.md, docs/) | **Opus 5.5** |
| Reviews | **Opus 5.5** first, then **Astra 6.2**. Work is accepted only after both reviews pass |

Every role works through OpenChamber subagents running the named model.
Nobody calls the Claude CLI directly. These are build assignments only. They
don't affect which model Metro calls at runtime.

## Layout

- `crates/`: Rust server (Axum, SQLite, Rig agent), auth, and outbound runner
- `tests/e2e/`: `smoke.py` and `threads.py`, run against the real binaries
  with a local scripted model
- `apps/desktop`: TSX desktop app on [Native Shell (NSH)](https://nsh.dev).
  Feature components use only semantic props. Styling lives in
  `theme/theme.json` and `src/ui/`, which must stay replaceable.
- `docs/`: user and operator docs
- `compose.yaml`, `Dockerfile`, `.github/workflows/ci.yml`: local stack, CI,
  and tagged GHCR releases

## Rules

The rules below describe the current source. For the published changes,
see [New in Metro 0.6.0](docs/released-0.6.0.md); for later changes, see
[In the source, not released](docs/next-source.md).

- Stay inside the files you were assigned. Don't edit another lane's files;
  report what needs to change instead.
- Never commit secrets. Generate tokens locally, and keep `.env` out of git.
- Access is per **Space**. Only members see a Space or its rooms. Only the
  owner manages members and the runner token, renames and describes the
  Space, and deletes it. Outsiders and unknown ids get 404, non-owners get
  403, and nobody can add themselves. Users are identified by their stable
  identity-provider id (`sub`, or the dev username), never by display name.
- Deleting a Space is permanent, owner-only and needs the Space's exact name
  typed as confirmation. It removes everything of the Space in one
  transaction (see [docs/spaces.md](docs/spaces.md#spaces-and-members)),
  including published Pages, their versions, recently deleted Pages and
  drafts. There is no Trash recovery for a deleted Space. It revokes its
  dedicated token, detaches its runner and stops its runs. Any
  member except the owner can leave. Leaving is the same as being removed,
  including the purge of that person's index rows and preferences for the
  Space. The owner can't leave.
- A Space has one or more **agents** (at most 16), each with a name, a
  unique `@handle`, a required one-line description, instructions, a
  picture and a tool allowlist. Exactly one is the **default**. The handle
  `agent` is reserved: only each Space's original agent may keep it, and
  `@agent` always means the generic agent, that is, the default agent or
  the classifier's pick. Instructions are appended after Metro's
  built-in preamble and can never replace it. The allowlist is a subset of
  the tools the run would get anyway; it never adds one. Only the owner
  manages agents, and every member can read everything about them,
  instructions included. Each run records its agent; threads keep the agent
  of the run that created them; scheduled prompts in a Space use its
  default agent.
- A server has one organization. Org admins are the stable user ids in
  `METRO_ORG_ADMINS`, a JSON array. There's no other admin role.
- The **directory** lets signed-in people find others who have signed in
  to this server, by display name or exact stable id. It exposes id, display
  name and avatar; bios are for the person and their Space or DM peers.
  **DMs** are one-to-one and participant-only, with no admin bypass,
  agents, attachments, mentions, search or Chat-agent access. Hiding a DM
  hides history for the caller; when both hide all events Metro purges
  the conversation.
- There are two kinds of runner, and they never stand in for each other:
  - A **dedicated** runner uses a Space's own runner token. That token is
    issued by the Space owner, shown once, stored only as a hash, rotated on
    reissue and revocable. It serves that one Space and nothing else.
  - A **registered** runner is named and scoped. A personal runner belongs to
    its owner. An organization runner belongs to the org and is managed by its
    admins. Its credential (`metro_rt_v2_`) follows the same rules: shown
    once, hash only, rotated on reissue, revocable.
- One runner can be attached to several Spaces, but each Space uses at most
  one runner. Only the runner's owner attaches a personal runner, and only to
  Spaces they own. Any Space owner may attach an organization runner to their
  own Space. A registered runner gets a job only for a Space it's attached
  to, and it keeps each Space in its own `spaces/<sid>/` root.
- So the old rule still holds in this form: a dedicated token never serves a
  second Space, and no job goes to a runner its Space didn't explicitly
  choose. Don't add a server-wide runner token, a fallback Space or runner, or
  any implicit cross-Space assignment.
- The desktop app has a personal registered runner built in (no separate
  install, binary or terminal). It exists only after an explicit Set up
  for the signed-in server and account (or, on the app's own local
  workspace only, the installer's marker), keeps its credential in one
  mode-600 file, starts and stops with the app, updates with it, and
  serves list and read only. Reconnect is explicit; deleting it turns it
  off locally and keeps its folders.
- The desktop may offer its own eligible personal runner when creating a
  Space or for owner-selected existing Spaces. Submitting the checked
  Create Space choice or pressing Attach is the explicit action. These
  paths must use an atomic attach-if-empty check, never replace or detach
  assignments (including dedicated tokens or offline runners), and never
  fall back to a replacing route. Reconnect rotates the existing
  registration's token without changing attachments; a new one starts
  unattached.
- **Space settings** combines General, Members, Runner, Agents and Your
  preferences. Members can read and change their own preferences;
  preserve owner-only management and the old Manage entry to Members.
  Runner attachment and dedicated token issue require an empty slot.
  Detach, revoke and rotate require the reviewed
  assignment revision; never retry unguarded. Don't show a dedicated
  token from an unconfirmed or superseded assignment. Rotation
  invalidates claimed work under the old credential and can preserve
  waiting work for the same runner; removing or replacing an assignment
  drops old work rather than transferring it. **Test runner** posts its
  listing as Runner activity in the open Space room, not privately in
  Settings, and never calls a model.
- A registered runner never runs check or MCP commands as unsandboxed host
  processes, because they could read sibling Spaces' roots. By default it
  offers only list and read. Check and MCP need `METRO_SANDBOX=docker`, which
  mounts only the claiming Space's root. Dedicated runners keep their
  existing behaviour.
- Model connections are personal by default. A run uses the sender's own
  usable connections (or ones shared with them there), and so do the child
  threads that run starts, in this order: the model explicitly chosen for
  that turn, the sender's model for that agent in that Space, the sender's
  default model in that Space, then the sender's use-case routing (below)
  or default chat connection. Every step is checked when saved and again
  when used; a choice that can't be used fails the run visibly. Nothing ever
  falls back silently to the next step, to another person's connection or
  to `METRO_MODEL` after a connection fails. `METRO_MODEL` is shown as
  "Server model (operator)", and only to someone with no default chat
  connection. There are two kinds of connection, and docs must keep them
  apart:
  - **Device connections** (`device-*` providers) run in the user's own
    `metro-connector` on their machine, with their own vendor sign-in or
    key. The server stores only the connection id and a hash of its device
    token, and relays inference requests and results. It never receives the
    vendor credentials. A Space runner is never used for model auth. If the
    device is offline, the run fails with a clear error.
  - **Server-stored API profiles** are optional. Their keys are write-only
    through the API, encrypted at rest with the operator's master key, and
    never logged or returned. Custom base URLs must match an entry in
    `METRO_PROVIDER_ALLOWED_BASE_URLS` exactly. Only the owner can change
    a supported endpoint. Testing sends the configured key, if any, to
    the approved candidate before the save. A rejected save cannot undo
    that disclosure. Don't overwrite a concurrent change or expose the
    endpoint to borrowers.
- The providers shown to users are only those the build implements.
  Server-side adapters come from the compiled Rig catalog. Don't claim a
  provider, a vendor sign-in flow or plan limits that nobody has verified, and
  never tell anyone to extract tokens or cookies from a vendor app.
- Never print, log or screenshot a token. Docs and scripts write tokens to
  mode-600 files and read them from there.
- Each connection serves one **capability**: `chat` (answers and tool turns),
  `system_one` (typed System One decisions) or `embeddings`. A connection is
  never used for a capability it wasn't set up for, and each capability has
  its own default. A person's first connection of a capability with a model
  becomes that default automatically, and deleting the default promotes
  their oldest remaining one.
- Each chat connection has its own **model list**, managed only by its
  owner. Models a provider lists are added enabled (opt-out); a refresh
  adds new ones and marks vanished ones unavailable, never deleting entries
  or the owner's names. Custom and self-hosted endpoints take manual models
  with owner-set thinking options (effort values plus the wire: `none`,
  `reasoning_effort` or `reasoning.effort`). A borrower of a shared
  connection may pick any model its owner left enabled; the owner controls
  that by disabling models.
- **Sharing is explicit and grants use, never credentials.** Only a
  connection's owner can share it, either with Spaces they select or with
  the whole organization, and can revoke the share. A Space share works
  only while both the owner and the borrower are members of that Space.
  Every use rechecks the grant, and work that's no longer allowed is
  refused. Nobody else ever sees its base URL, key, sign-in or device token,
  or gets any management rights. Docs and UI must say that the owner of
  the device or endpoint can see the requests that go through it and may pay
  for them. Subscription sign-ins tied to one person's account
  (`device-openai-oauth`, Sign in with ChatGPT) can't be shared. Their owner
  can share a bring-your-own preset instead, which holds no credential, and
  each person then signs in with their own account. A preset must never
  switch quietly to the borrower's paid API profile. Accounts are never
  shared.
- **In Spaces, explicit recipients always decide who a message is
  addressed to.** A Space message falls into exactly one of these cases:
  - **People only** (`@bob`, without an agent recipient from any source).
    It never starts agent work, and it's never sent to System One for an
    addressing or routing decision.
  - **To a named agent** (its `@handle`, Ask agent with that agent chosen,
    or a text reply to that agent in a thread unless the sender opts out).
    That recipient bypasses classification. Deduplicate it with other
    explicit recipients. A separate generic mention still resolves normally.
    A deleted reply agent is not replaced; the sender gets a private notice.
  - **To the generic agent** (`@agent` or Ask agent without an agent,
    including mixed `@bob @agent`). It starts agent work, with no "was this
    for an agent?" decision. The default agent runs, unless the sender opted
    in to classification in that Space; then one System One Choice among the
    agents (with no "no agent" option) picks it. An unsure answer runs the
    default agent. A failed decision shows a visible error, and no agent
    runs for `@agent`.
  - **Unaddressed** (no mention, Ask agent or agent reply target). By
    default it's ordinary conversation. Only the sender's opt-in
    classification (below) can start an agent from it.

  The same routing below still applies to choose each run's answering
  model. The agent's history shows each message's author and recipients.
- **Steering.** A message that would start agent A for sender S in a
  Space room or thread where A is running a run S started from S's own
  message (or, in Chat, a steer while S's turn runs in the same Chat
  thread or the main Chat, never another) is delivered into
  that run at its next model call instead of starting another. Space
  messages always steer this way unless they carry an explicit model for
  the turn; another person's message or one for another agent starts its
  own run, and people-only messages never reach a run. Delivery rechecks
  in one transaction that it's still S's message, S may still read the
  room and (Chat) it wasn't stopped, and writes a `steered` marker (never
  a message, history or indexed). Anything not delivered when the run
  ends starts as it would have. Inboxes are memory only.
- The desktop sends verified Space mentions as stable IDs and validated
  text ranges. Never silently retarget an ambiguous, changed or stale
  mention; keep the original draft when send is refused. Manual Dispatch
  from Chat is an explicit generic request to the verified room or thread,
  with no implicit recipients from its text. Follow up checks the exact
  destination and doesn't silently change it. Generic `@agent` isn't an
  identity card. Don't fall back to legacy text inference on a server
  refusal.
- **Opt-in classification** decides only which agent, if any, an outgoing
  message is for. A person turns it on for their own messages **per
  Space**, only while they have a System One connection usable in that
  Space. It's one typed System One Choice over the Space's agents plus
  "No agent" for an unaddressed message (without "No agent" for the
  generic `@agent`). The options are keyed internally, so model text
  never becomes an agent id. For unaddressed text, send the new text,
  Space and room names, Space description, the display names of the
  sender and the Space's other members (never ids), and up to eight
  preceding human or agent excerpts (400 characters each, 2,400 in
  total, newest kept), each with its author's name, whether a person or
  an agent, and the names it explicitly addressed, to the sender's
  System One connection. In a reply thread the anchor (unless deleted)
  is the first excerpt, within those bounds and always kept. Lean strongly toward a useful agent; "No agent"
  is for text directed at or about another person, or that says no
  agent should answer. Below the directedness threshold or on failure,
  start nothing; at or above it, "No agent" uses the strongest agent
  option. Default is 0.35 (range 0.2–1); migrate stored 0.85 values once.
  Failure on unaddressed text returns an unstored `addressing_note` to
  the sender only, not a room event. The selected run acts as the sender.
  With `defer_decisions` (what the desktop sends), the message is stored
  and shown first and the decision follows; the pick is run activity and
  is never written into the stored recipients (in no path: the POST says
  `classified`). Nothing labels it "Picked by classifier" or "detected by
  System One"; a weak pick's fallback marker names no guess or score. The
  pending decision is a `pending_decisions` row written with the message
  (with any explicit model choice) and claimed by the storing process;
  once made it becomes one `pending_targets` row per run, each claimed in
  the transaction that registers that run and writes its `started`
  marker, which also checks the message still exists. Startup claims
  everything left (`UPDATE ... RETURNING`) and enqueues it before
  accepting requests; only the claimant dispatches. Work rereads the live
  message after each wait and before each System One call, so a deleted
  message reaches no model. Classification context is only events before
  the message. Deferred System One calls are bounded per server and per
  person and wait rather than drop; a slot is held only for the call.
  Failed pending work (any error path or panic, via a drop guard; a Chat
  turn's guard disarms only once its `started` marker is written) is
  dropped with a visible room or Chat note (Chat notes carry the
  `turn_id`); work older than an hour at recovery is noted as
  unanswered, never run. For unaddressed messages both drops are silent
  in the room (a note would reveal the opt-in). After graceful shutdown
  begins, guards drop and note nothing; the rows wait for the next start.
  Deleting a Space deletes its pending rows. A resumed choice's refusal is
  a generic room note. One `metro-server` process per database (no lock:
  the dev supervisor and desktop hand over a listening socket and overlap
  briefly); work an overlapping older process stored is recovered at the
  next restart. Never classify people-only or explicitly named recipients.
  With classification off (the default), unaddressed text is ordinary
  conversation.
- Sender-only notice details are POST metadata, never a polled event,
  search result or model history. Either or both fields may be present.
  Deferred decisions hand them out once, in memory only (lost on
  restart, while the decision itself still runs), through the
  sender-only `POST .../messages/{eid}/decisions`, which rechecks
  membership and that the message still exists as the sender's message.
  Bind them to the originating authenticated sender, destination and
  matching transcript row. Don't replay them after dismissal, session
  reset or access loss, or expose private model details in a shared
  room. Direct Personal Chat failures
  in this server appear in the private Chat, not as addressing notices.
- **Personal Chat is direct.** Send starts the Chat agent, including for
  attachment-only messages. No addressing classifier or marker is needed.
  **Save note** stores the message without starting a run and overrides
  text mentions and the legacy `invoke_agent` flag. `infer_addressing`
  remains accepted for compatibility and the existing one-time Space
  migration, but doesn't control Chat dispatch. Use-case routing remains
  separate and can choose the answering connection. The POST returns
  first, with the turn's `turn_id`; the turn is saved as a pending
  decision, registered before it routes, started in storage order, and
  never started if its message was deleted. Stop works before it starts:
  it's saved in the pending row before it's acknowledged, so a restart
  doesn't start it. A person's turns run one at a time per conversation
  (the main Chat and each Chat thread have their own line; **Queue**, the
  default): the next starts when the running one there ends, a turn in
  one never holds up another, and Stop ends only the running one. With
  `delivery: "steer"` the message goes into the turn running in the same
  conversation at its next model call instead; if that turn ends first,
  it is queued like any other (see Steering below).
- **Metro in Chat** can read and act as the user in their eligible
  Spaces. The per-account switch is on by default but gated on showing
  the notice, with per-Space exclusions and live access checks. Space
  content read goes to the model answering Chat, visible to a shared
  connection's owner. Posts go directly as the user, marked `via Metro`,
  only to a Space named in the user's own text. Address exactly one
  agent: the named one, else the agent whose message the post replies
  to, else the default agent. Don't infer recipients from text. A
  schedule Metro creates may name only Spaces the user named. The
  receipt is Chat-only, never indexed or model history. Metro doesn't
  change settings or apply Page drafts. Chat stop doesn't stop delegated
  runs; only their initiators can retry eligible failed or cancelled
  runs, with fresh routing. The runs registry is memory-only.
- **Use-case routing** is a separate opt-in, and it decides only which of a
  person's usable chat connections answers. It can run on any run that
  starts for that person: a named agent, `@agent`, Ask agent, or one
  started by classification. An explicit model chosen for that turn and
  the person's per-agent and per-Space models override it, and each person
  can turn it off per Space. With routing off, the person's default chat
  connection answers. It follows the same consent, membership and sharing
  checks.
- **System One is the typed TypeSafe API** (`POST /v1/systemone`, models such
  as `jev-latest`, question types `noul`, `choice`, `score`). Spell it
  "System One" and "Noul". Never present a chat model's free-text guess as a
  System One answer.
- **Semantic search** covers only what the searcher can already read: their
  own personal Chat, and the rooms and threads of Spaces they're a member of
  now. It retrieves with embeddings, then reranks a bounded shortlist with a
  real System One call. Results are never labelled reranked unless a System
  One call ranked them. Chat's Space search also checks the Metro in Chat
  grant. The Documents & media library below is a separate corpus.
- **Documents & media** ([docs/library.md](docs/library.md)) holds only files
  a person **explicitly uploads**, either to their personal library (only
  the uploader sees it, with no admin bypass) or to a Space they're a member
  of (only its current members see it). Metro never scans runner roots,
  workspaces or anyone's disk to fill it.
  - Every list, read, preview, download, search, index and outgoing model
    call rechecks access at that moment. So revoking someone's membership
    takes effect on their next request, and results are filtered again
    before they're returned. Removing a member doesn't delete the Space's
    assets. It purges only that person's index rows for the Space, in the
    same transaction. Deleting a file cascades to everyone's index rows.
  - Indexing is manual, and each person has their own index. The person
    chooses the assets and the media-capable connection. Uploading, listing
    and searching never send an unindexed file anywhere. That connection
    follows the same personal, sharing and no-fallback rules as any other
    connection. Docs must say that whoever runs the endpoint, including a
    shared device's owner, sees the segments and bytes that are sent.
  - A text-only `/v1/embeddings` connection never stands in for media
    embedding. Neither does a filename, caption or base64-as-text. Don't
    tell anyone to point the media adapter at an unverified vLLM, llama.cpp
    or other endpoint.
  - The model is `google/embeddinggemma-2` (with a hyphen; not first-gen
    EmbeddingGemma, not Gemini Embedding 2). It takes no native PDF input, so
    pages are extracted or rendered first.
  - A result with no text excerpt is never labelled reranked by System One,
    because System One is text only. Files are opened only through an
    authenticated request that runs the access check again.
- **Scheduled prompts** run once or on a recurrence, in the creator's
  personal Chat or in a Space room, and the Metro server dispatches them in an
  IANA time zone. Each occurrence is dispatched at most once, across
  restarts too. On every trigger, the server re-checks the creator's
  membership and connection. If a check fails, that occurrence fails visibly
  and no fallback is used. A scheduled prompt counts as the creator
  explicitly asking the agent (in a Space, its default agent, never
  classified).
- **Web search and fetch** run on the initiator's most recently seen
  online web device on that server, never on the server itself, a runner
  or a borrowed connection's device. The computer's
  switch is on by default with a notice. A Space owner enables each tool
  in its agent allowlist; personal Chat has a separate per-account opt-in
  for both, off by default. Provider choices and keys stay on the
  computer; keys go only to their provider, never to Metro. The server
  relays job arguments and bounded results. A contacted provider sees
  applicable network and auth data and may receive the query or URLs;
  session setup can fail first. A rejected saved key never silently
  falls back to keyless for that provider. Activity keeps accepted
  queries, keywords and non-default result limits, or normalized URLs
  and focus, plus bounded summaries and attempt outcomes. Snippets and
  page text are relayed as current-run tool output, not stored or
  replayed as tool history; replies may quote them. Metro doesn't
  control vendor or model retention. If no paired device is online at
  call time, report the tool failure. A scheduled prompt may answer
  without web data if its model is usable.
- **Fetch checks provenance, not DNS.** Require an HTTP(S) URL from a
  person's message available to the run or a structured search result
  in that run. Reject URL userinfo, blocked IP literals and recognized
  local hostnames. Metro doesn't check target DNS or control provider
  redirects; don't describe these checks as proof a URL is public.
  Warn against submitting private or credential-bearing links.
- **Pages** are validated Page TSX, not executable user code. Reads are
  as the viewer; Space pages see only their own Space, and personal
  pages have no admin bypass. Actions run as the clicker. Personal-page
  runs are page-only with no web tools and cannot target Chat or a Space;
  Space actions are limited to that Space's top-level rooms. Consent
  from one button covers the entire page version, not just that button.
  Each click still checks the current version, access and destination;
  schedules also check the reviewed task revision as applicable.
  Space Page tools are opt-in per agent, run
  as the initiator, and return only room-safe data. Proposal cards are
  visible in the room; the draft is private to the initiator and applies
  only after they act with current edit rights. The organization limits
  are live through the token-authenticated `/v1/admin/settings` API used
  by `/admin`; they don't grant access to page data.
- Reactions never start agent work and never imply that work is complete.
- **Space agents** get bounded Space, room, member and agent context;
  user-written names and descriptions are data. A run may reply, react,
  or complete with no agent message (including an empty final answer).
  The outcome stays in run activity metadata, but silence shows nothing
  in the transcript; never turn it into a chat message.
- **Messages** render bounded Markdown without live HTML or remote
  images. Forward copies text, not recipients, attachments, reactions
  or replies; verify both ends and never dispatch an agent from a forward
  or its comment.
- **Deleting a message** is permanent: anyone may delete their own
  messages; a Space owner any message or agent reply in their Space; a
  personal Chat's owner anything in it; DMs only your own. No admin
  bypass; unreadable or unknown is 404, not allowed is 403, not a message
  or agent reply is 409. One transaction removes its reactions,
  attachment links, everyone's search vectors and covering summaries, so
  it never reaches a model again. A thread anchor (even of an empty
  thread) stays as a `deleted` tombstone, kept out of model history and
  the thread preamble. A `message_deleted` event tells readers. Deleting
  never starts or cancels a started run; a run not yet registered for it
  never starts. Run task previews are resolved from the live trigger
  when read (runs API, Pages, Metro in Chat): a deleted trigger shows no
  text, status kept.
- **Compaction** keeps the saved transcript intact. Chat, Space rooms
  and threads can use a model-written summary as untrusted history
  (never DMs or Page conversations). Auto is per initiator, on by default
  at 70% of estimated input budget, and uses the run's resolved model.
  Manual `/compact` uses the caller's default Chat route or route for
  the Space's default agent. No fallback after model failure. Summary
  and focus are visible to readers of that conversation, not indexed.
  A slash command starts at the beginning of a draft and is not a message.
- A `reasoning_summary` event carries only the reasoning summary text a
  provider returned (bounded), never encrypted, raw or redacted reasoning
  and never text Metro made up. It's never a message, never model history
  and never indexed.
- Threads stay one level deep and inherit Space membership and runner.
  Agent-created threads keep their original agent and connection.
  Anchored reply threads have their own limit, include the anchor in
  agent history and do not mirror to the parent. Personal Chat has no
  direct runner, workspace or agent-thread tools; reactions and web
  tools keep their separate rules.
- **Chat threads** belong to the Chat's owner alone (no admin bypass;
  unknown is 404): a new thread, or a reply thread anchored to a
  main-Chat message or reply, one level deep. Each is its own Chat-agent
  conversation with its own history, summaries, turns, Stop and
  Queue/Steer line and inbox; a reply
  thread's history starts with its anchor unless deleted. Only the owner
  renames one. Deleting a thread is permanent and in one transaction
  removes its events, reactions, attachment links, search vectors,
  summaries and pending decisions, and stops its turns (their later
  events are dropped, not logged as errors); a deleted anchor
  stays as a tombstone while its thread exists. Scheduled prompts
  target the main Chat only.
- The database is pinned to one identity namespace. Never assign legacy or
  foreign history to anyone.
- Desktop startup can show the Shell after verifying identity and
  membership, before the remembered room finishes loading. Don't show
  an old transcript before verification or let late startup work
  override a newer navigation, session or authoritative access change.
  Only reuse in-flight model reads for the same session and scope.
- *Demo* must stay deterministic and must never call a model.
- Sandboxing is opt-in, and `docker` mode fails closed. Check and MCP commands
  are operator-defined JSON argv and are never run through a shell.
- Status: [docs/system-one.md](docs/system-one.md) tracks each part.
  - Every part is implemented: scheduled prompts
    ([docs/schedules.md](docs/schedules.md)), capabilities and sharing
    ([docs/settings.md](docs/settings.md)), classification, use-case routing
    and semantic search. All of them are demonstrated end to end with
    loopback fixtures and scripted models.
  - The classification and routing opt-ins are off by default. With them
    off, the default behaviour above is the only behaviour.
  - A historical final local result is the Metro 0.2.0 local release,
    prepublication (`docs/acceptance.md`): 570 Rust tests, all seven e2e
    scripts, 6 packaging tests, and desktop `nsh test` 159/159. The
    Documents & media gate (497 Rust tests, the 83 hardened media-service
    tests, `nsh test` 106/106) and the earlier 403-test gate are historical
    milestones.
  - **Metro 0.6.0 is published**: stable GitHub release `v0.6.0`
    (2026-11-26T03:13:02Z), tag on `430c835` (PR #19), made by CI run
    45737693549, with 18 assets, Sigstore-signed `SHA256SUMS`, unsigned
    Windows files and signed GHCR images (see [release record](docs/acceptance.md#published-metro-060-2026-11-26-github-release-v060)).
    Native Windows by a person, real vendor accounts and real sign-ins
    remain unverified. [0.6.0 work](docs/released-0.6.0.md) is
    released; [0.6.1 work](docs/next-source.md) is not.
  - The previous release was [Metro 0.5.1](docs/acceptance.md#published-metro-051-2026-11-26-github-release-v051)
    (`v0.5.1` on `5120c52`, CI run 45725172627), and before it
    [Metro 0.5.0](docs/acceptance.md#published-metro-050-2026-11-25-github-release-v050).
    Before them, Metro 0.4.0: GitHub release `v0.4.0` (2026-11-25),
    tag on `1a743e5`, made by CI run 45560122825, with Sigstore-signed
    `SHA256SUMS`, unsigned Windows files and GHCR images signed by the release workflow
    (`docs/acceptance.md`). Metro 0.3.2 (`v0.3.2` on `d943797`, CI run
    45418420759) was the release before it. Native Windows by a person,
    real vendor accounts and real sign-ins are still unverified.
  - The 0.4.0 staged gate (4175475, several agents per Space, per-connection
    model lists, the desktop redesign) ran on local builds of source that
    is now released in 0.4.0: 686 Rust tests, eight e2e scripts (not
    `library_reference.py`) and desktop `nsh test` 400/400, with no
    packaging tests, real vendor run or native Windows check. The 0.3.x
    releases don't contain it.
  - Don't claim a real commercial provider, production auth, a release
    other than the recorded ones, pretrained `google/embeddinggemma-2` ranking quality, or
    native audio and video playback, seeking or the native file picker for
    library uploads, until a run is recorded.
- **User and operator docs use prebuilt files only:** the desktop app
  (Windows, Linux), the server and runner images, and the Linux runner
  binary from https://github.com/wnzn/metro. They never ask anyone to
  compile, `cargo install`, build an image locally, or run a terminal
  sidecar next to the desktop app. Source builds live only in
  [docs/development.md](docs/development.md). Docs point to the published
  release by the names and URLs recorded in `docs/acceptance.md`: the
  current one is **Metro 0.6.0**
  (https://github.com/wnzn/metro/releases/tag/v0.6.0, tag `v0.6.0` on
  `430c835`), with `Metro-0.6.0-linux-x64.tar.gz`,
  `Metro-0.6.0-windows-x64.zip`,
  `Metro-0.6.0-windows-x64.unsigned-preview.msi`,
  `Metro-runner-0.6.0-linux-x64.tar.gz` (each with `.sha256`), the
  installer scripts, `SHA256SUMS` with its Sigstore bundle, `IMAGES.txt`,
  and the images `ghcr.io/wnzn/metro-server:v0.6.0`,
  `ghcr.io/wnzn/metro-runner:v0.6.0` and
  `ghcr.io/wnzn/metro-embeddinggemma2:v0.6.0` (pinned by digest). Windows
  files are **unsigned** (no Authenticode). Docs never present source
  that came after a release's commit as part of that release; label it
  "in the source, not released". A newer release is named in docs only
  once its publication is recorded in `docs/acceptance.md`. The desktop app's **Use locally**
  runs a bundled local server with no token, and its owner-only, per-Space,
  read-only local tools (`workspace.list`/`workspace.read`) never stand in
  for an operator's separately installed runner or for the app's built-in
  runner. It and the other features
  checked in the 0.2.0 Linux candidate are described as verified in that
  local candidate, never as released. Keep unattended native Windows CI
  checks separate from earlier Wine attempts and the still-unverified
  native GUI by a person. Real vendor accounts and sign-ins remain
  unverified. A deployment's own address (such as a dev LAN host) is
  never a documented default.
- **Metro server transport** in 0.5.0: prefer HTTPS or SSH
  forwarding. The desktop and `/admin` require informed consent before
  sending a token over ordinary plain HTTP; runner and CLI clients warn
  rather than prompt. Direct loopback and recognized Tailscale hosts can
  skip the question. The desktop also asks when the route to one of those
  hosts is proxied or unknown; don't silently exempt tailnet hosts from a
  user's desktop proxy. Runner and connector server clients reach loopback
  and Tailscale directly; the admin panel can't detect browser proxies.
  This doesn't relax provider URL rules.
- **Updates** require a verified published release and an explicit
  install action. Source and preview builds don't offer updates. Only
  eligible managed Linux desktop installs update in-app; portable and
  Windows copies are manual, and servers and Docker runners get notices
  only. Standalone runner self-update verifies independently; the server
  sends a version, never a URL or code. If recovery is uncertain, keep
  journals, locks and database copies intact; document the verified
  installer's read-only diagnostic, never an improvised repair.
- The docs must not claim features that aren't implemented, or results that
  nobody recorded. When code and docs disagree, fix the docs. Record each
  verification run in `docs/acceptance.md`.
