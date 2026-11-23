# AGENTS.md

Guidance for people and coding agents who work on Metro.

## Authorship policy (build process)

This is how the work is split while building Metro. It is **not** runtime
behaviour. Metro does not route requests to these models; at runtime it uses
whatever model `METRO_MODEL` names.

| Role | Owner |
| --- | --- |
| Project manager and orchestrator: decides architecture, scope and trade-offs; assigns and directs subagents; accepts results only once they are verified | **Astra 6.2** |
| Writing (README, AGENTS.md, docs/), frontend (`apps/desktop`), complex code | **Opus 5.5**, only through OpenChamber subagents; **Astra 6.2** may directly edit and test `apps/desktop` for UX Round 2 only. |
| Mid-level code (assigned to either, at Astra's discretion) | **Sol 6.2** or **Sonnet 5.5**, both through OpenChamber subagents |

Opus 5.5 never edits directly in the root session, and nobody calls the
Claude CLI directly. Opus contributes only as an OpenChamber subagent. These
are build assignments only. They don't affect which model Metro calls at
runtime.

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

- Stay inside the files you were assigned. Don't edit another lane's files;
  report what needs to change instead.
- Never commit secrets. Generate tokens locally, and keep `.env` out of git.
- Access is per **Space**. Only members see a Space or its rooms. Only the
  owner manages members and the runner token. Outsiders and unknown ids get
  404, non-owners get 403, and nobody can add themselves. Users are identified
  by their stable identity-provider id (`sub`, or the dev username), never by
  display name.
- A server has one organization. Org admins are the stable user ids in
  `METRO_ORG_ADMINS`, a JSON array. There's no other admin role.
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
- A registered runner never runs check or MCP commands as unsandboxed host
  processes, because they could read sibling Spaces' roots. By default it
  offers only list and read. Check and MCP need `METRO_SANDBOX=docker`, which
  mounts only the claiming Space's root. Dedicated runners keep their
  existing behaviour.
- Model connections are personal by default. Unless something below says
  otherwise, a run uses the default chat connection of the person who
  started it, and so do the child threads that run starts. Nothing ever
  falls back silently to another person's connection or to `METRO_MODEL`
  after a connection fails. There are two kinds of connection, and docs must
  keep them apart:
  - **Device connections** (`device-*` providers) run in the user's own
    `metro-connector` on their machine, with their own vendor sign-in or
    key. The server stores only the connection id and a hash of its device
    token, and relays inference requests and results. It never receives the
    vendor credentials. A Space runner is never used for model auth. If the
    device is offline, the run fails with a clear error.
  - **Server-stored API profiles** are optional. Their keys are write-only
    through the API, encrypted at rest with the operator's master key, and
    never logged or returned. Custom base URLs must match an entry in
    `METRO_PROVIDER_ALLOWED_BASE_URLS` exactly.
- The providers shown to users are only those the build implements.
  Server-side adapters come from the compiled Rig catalog. Don't claim a
  provider, a vendor sign-in flow or plan limits that nobody has verified, and
  never tell anyone to extract tokens or cookies from a vendor app.
- Never print, log or screenshot a token. Docs and scripts write tokens to
  mode-600 files and read them from there.
- Each connection serves one **capability**: `chat` (answers and tool turns),
  `system_one` (typed System One decisions) or `embeddings`. A connection is
  never used for a capability it wasn't set up for, and each capability has
  its own default.
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
- **Explicit recipients always decide who a message is addressed to.** A
  message falls into exactly one of three cases:
  - **People only** (`@bob`, with no `@agent` and not sent with Ask agent).
    It never starts agent work, and it's never sent to System One for an
    addressing or routing decision.
  - **To the agent** (`@agent` or Ask agent, including mixed `@bob @agent`).
    It starts agent work, with no "was this for the agent?" decision. The
    same routing below still applies to choose the answering model.
  - **Unaddressed** (no mention, and not sent with Ask agent). By default it's
    ordinary conversation. Only the sender's opt-in classification (below)
    can start the agent from it.

  The agent's history shows each message's author and recipients.
- **Opt-in classification** decides only one thing: whether an unaddressed
  outgoing message is asking the agent. A person can turn it on for their own
  messages, in rooms or in their personal Chat. A message it starts runs as
  that person. It's never used on people-only or to-the-agent messages, or on
  other people's messages. With it off, which is the default, unaddressed
  text is ordinary conversation.
- **Use-case routing** is a separate opt-in, and it decides only which of a
  person's usable chat connections answers. It can run on any message that
  starts agent work for that person: explicit `@agent`, Ask agent, or one
  started by classification. An explicit model chosen for that turn
  overrides it. With routing off, the person's default chat connection
  answers. It follows the same consent, membership and sharing checks.
- **System One is the typed TypeSafe API** (`POST /v1/systemone`, models such
  as `jev-latest`, question types `noul`, `choice`, `score`). Spell it
  "System One" and "Noul". Never present a chat model's free-text guess as a
  System One answer.
- **Semantic search** covers only what the searcher can already read: their
  own personal Chat, and the rooms and threads of Spaces they're a member of
  now. It retrieves with embeddings, then reranks a bounded shortlist with a
  real System One call. Results are never labelled reranked unless a System
  One call ranked them. Search is the person's own feature. It doesn't widen
  the personal Chat agent's tools. Message search stays as it is. The
  Documents & media library below is a separate, additional corpus.
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
  explicitly asking the agent.
- Reactions never start agent work and never imply that work is complete.
- Threads stay one level deep and bounded, and inherit their Space's members
  and runner. The personal Chat agent's only tool is the scoped
  `react_to_message` acknowledgment. It has no runner, workspace or thread
  tools, and can't read Space transcripts. Cross-Space run controls are for
  the human, and they check membership.
- The database is pinned to one identity namespace. Never assign legacy or
  foreign history to anyone.
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
  - The current final local result is the Metro 0.2.0 local release,
    prepublication (`docs/acceptance.md`): 570 Rust tests, all seven e2e
    scripts, 6 packaging tests, and desktop `nsh test` 159/159. The
    Documents & media gate (497 Rust tests, the 83 hardened media-service
    tests, `nsh test` 106/106) and the earlier 403-test gate are historical
    milestones.
  - Don't claim a real commercial provider, production auth, a published
    release, pretrained `google/embeddinggemma-2` ranking quality, or
    native audio and video playback, seeking or the native file picker for
    library uploads, until a run is recorded.
- **User and operator docs use prebuilt files only:** the desktop app
  (Windows, Linux), the server and runner images, and the Linux runner
  binary from https://github.com/wnzn/metro. They never ask anyone to
  compile, `cargo install`, build an image locally, or run a terminal
  sidecar next to the desktop app. Source builds live only in
  [docs/development.md](docs/development.md). Until a release is published,
  docs give the verified target names (`ghcr.io/wnzn/metro-server`,
  `ghcr.io/wnzn/metro-runner`, `Metro-<version>-windows-x64.zip`,
  `Metro-<version>-linux-x64.tar.gz`,
  `Metro-runner-<version>-linux-x64.tar.gz`, each with `.sha256`) and say
  they aren't published yet. The desktop app's **Use locally**
  runs a bundled local server with no token, and its owner-only, per-Space,
  read-only local tools (`workspace.list`/`workspace.read`) never stand in
  for an operator's separately installed runner. It and the other features
  checked in the 0.2.0 Linux candidate are described as verified in that
  local candidate, never as released. Anything still unchecked (native
  Windows, real vendor accounts and sign-ins) is labelled as such. A
  deployment's own address (such as a dev LAN host) is never a documented
  default. Over plain HTTP on a LAN, the admin page is a landing
  page only; tokens go over loopback, an SSH port forward or HTTPS.
- The docs must not claim features that aren't implemented, or results that
  nobody recorded. When code and docs disagree, fix the docs. Record each
  verification run in `docs/acceptance.md`.
