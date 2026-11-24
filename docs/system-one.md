# System One, semantic search, shared connections and scheduled prompts

> **Status: implemented, and every local gate passed. Commercial vendors
> haven't been validated.** The current final local result is the
> [Metro 0.2.0 local release](acceptance.md#final-metro-020-local-release-prepublication-2026-11-23-project-manager-local) (prepublication). The
> [Documents & media gate](acceptance.md#demonstrated-final-local-gate-with-documents--media-2026-11-22-primary-workspace-local),
> with 497 Rust tests, all seven e2e scripts and desktop `nsh test` 106/106,
> is a historical milestone.
> The earlier [System One milestone gate](acceptance.md#historical-final-gate-for-the-system-one-milestone-2026-11-22-independent-reviewer-local),
> with 403 Rust tests, is kept as a record.
>
> - Every part is documented from its code.
> - `tests/e2e/intelligence.py` passed against the primary workspace's
>   locked binaries, with loopback inference fixtures
>   ([acceptance.md](acceptance.md#demonstrated-classification-routing-sharing-and-search-end-to-end-2026-11-22-integrator-local)).
> - Each System One request carries exactly one typed question.
>
> No part has been tested against a real TypeSafe account, a cloud vendor or
> production authentication.

| Part | Implemented | Verified |
| --- | --- | --- |
| [Connection capabilities](settings.md#capabilities) (`chat`, `system_one`, `embeddings`) | yes | Component tests, and `tests/e2e/intelligence.py` |
| [Sharing a connection](settings.md#sharing-a-connection) with Spaces or the organization | yes | Component tests, and `tests/e2e/intelligence.py` across users, on a local endpoint and on a device, including revocation |
| [Opt-in classification](#opt-in-classification-of-unaddressed-messages) of unaddressed messages | yes | Component tests, and `tests/e2e/intelligence.py` |
| [Use-case routing](#use-case-routing) to an answering model | yes | Component tests, and `tests/e2e/intelligence.py` |
| [Semantic search](#semantic-search) with a System One rerank | yes | 14 unit + 4 HTTP tests, `tests/e2e/intelligence.py`, and the native desktop screen ([screenshot](screenshots/semantic-search.png)) |
| [Scheduled prompts](schedules.md) | yes | 30 unit + 2 HTTP tests, `tests/e2e/schedules.py`, a live hourly cron run, and the native desktop screen ([screenshot](screenshots/scheduled-tasks.png)), all with scripted models |

## What stays the same by default

A server that never turns any of this on behaves as it does today:

- A message to people only (`@bob`, with no agent and not sent with Ask
  agent) never starts an agent.
- Only an agent's `@handle`, `@agent` or **Ask agent** starts agent work,
  including in a mixed `@bob @agent` message. `@agent` and Ask agent without
  a chosen agent start the Space's default agent. Text with no mention is
  ordinary conversation.
- A run uses the default chat connection of the person who started it, or
  `METRO_MODEL` if they have none. A connection that fails fails the run, with
  no fallback.
- Connections are private to their owner until the owner shares one.

Everything below is **opt-in** and adds to these defaults.

## Connection capabilities

Every connection has one fixed capability, `chat`, `system_one` or
`embeddings`, with one default per capability. A chat model never stands in
for System One, and an embeddings endpoint never answers a chat turn. The
provider ids are `systemone`, `custom-systemone` and `device-systemone` for
System One, and `openai-embeddings`, `custom-embeddings` and
`device-embeddings` for embeddings. Details are in
[settings.md](settings.md#capabilities).

### System One (TypeSafe)

System One is TypeSafe's typed decision API. Jev is its model family
(`jev-latest` and dated versions). Metro calls it as documented, and these
docs describe it no further than TypeSafe's own docs do:

- `POST https://api.typesafe.ai/v1/systemone`, with `Authorization: Bearer
  <API key>`. The body has `model`, `state` (the content being judged) and
  named `questions`. The answers come back under the same names, with
  `model` and `usage`
  ([API reference](https://docs.typesafe.ai/api/system-one)).
- Metro uses two question types
  ([primitives](https://docs.typesafe.ai/primitives)):
  - `noul`, the probability of yes from 0 to 1, for addressing in personal
    Chat and the search rerank;
  - `choice`, one option with `confidence` and per-option `probabilities`,
    for choosing a Space agent and for use-case routing.

  Every request Metro sends carries exactly one question, because some
  endpoints accept no more.
- `GET /v1/models` lists the model names an account may use
  ([reference](https://docs.typesafe.ai/api/list-models)).
- TypeSafe's limits: a body of at most 32 MiB (`413` above that), UTF-8 JSON,
  at most 64 levels of nesting, and finite numbers only. `429` and `529` mean
  back off and retry ([request rules](https://docs.typesafe.ai/api/errors)).

The `systemone` provider uses `https://api.typesafe.ai/v1`. Metro's System
One calls have only reached a loopback fixture. None has gone to a real
TypeSafe account.

## Sharing a connection

This is documented from the code in
[settings.md](settings.md#sharing-a-connection). In short:

- Only the owner shares a connection: with selected Spaces, while both
  people remain members, or with the whole organization.
- Borrowers get use only, never the base URL, key, pairing token or any
  management.
- The owner can see borrowed requests and may pay for them.
- Every use rechecks the grant, on the server and on devices.
- Sign in with ChatGPT is shared only as a credential-free bring-your-own
  preset. It runs only on the borrower's own Sign in with ChatGPT
  connection, and never falls back to an API-billed key.

## How a message reaches the agent

Explicit recipients always decide who a message is addressed to. System One
makes separate decisions, each behind the sender's own opt-in:

- **Addressing:** whether an unaddressed message is asking an agent, and in
  a Space which one; and which agent the generic `@agent` means. This is
  [classification](#opt-in-classification-of-unaddressed-messages).
- **Answering model:** which chat connection answers a run that has already
  started. This is [use-case routing](#use-case-routing).

| Message | Starts an agent? | Agent decision | Answering model |
| --- | --- | --- | --- |
| People only: `@bob`, no agent, not sent with Ask agent | Never | None. It's never sent to System One. | None. There's no run. |
| An agent's `@handle`, or Ask agent with that agent chosen | Yes, that agent (one run per agent mentioned) | None | See [which model a run uses](settings.md#which-model-a-run-uses) |
| `@agent`, or Ask agent without an agent, including mixed `@bob @agent` | Yes | The default agent; with the sender's classification on in that Space, a Choice among the agents (no "no agent" option). Unsure: the default agent. Failed: an error, and nothing runs. | Same |
| Unaddressed, with the sender's classification **off** (the default) | No | None | None. There's no run. |
| Unaddressed, with the sender's classification **on** | Only if System One clearly picks an agent | A Choice among the agents plus *No agent* (Spaces), or a `noul` (personal Chat) | Same |

## Opt-in classification of unaddressed messages

A person can turn on classification for **their own** outgoing messages. In
Spaces it's a setting **per Space** (`classify_unaddressed` in
[your preferences for that Space](spaces.md#your-preferences-in-a-space)).
It can be turned on only while the person has a System One connection
usable in that Space. Personal Chat keeps its own setting
(`infer_addressing`, [below](#settings-get--put-v1model-routing)). Both are
off by default.

**In a Space room**, it decides two things for the sender's messages:

- **Unaddressed** messages (no mention, and not sent with Ask agent): one
  System One `choice` over the Space's agents plus *No agent*. Each option
  shows the agent's name and description and is keyed internally
  (`agent_1`, `agent_2`, …), so nothing the model returns is ever used as an
  agent id. A pick starts that agent, as the sender, only if the probability
  that the message is for some agent is at least `min_directedness`. *No
  agent*, an unsure, malformed or failed answer, or an option that was never
  offered starts nothing, and the message stays ordinary conversation.
- **`@agent`, or Ask agent without an agent**, in a Space with more than
  one agent: the same `choice`, without *No agent*. A pick at or above
  `min_route_probability` runs that agent; a weaker one runs the default
  agent. A failed decision posts an error in the room and runs nothing for
  `@agent`.

**In personal Chat**, it decides only whether an unaddressed message is
asking the agent: a single `noul`, and only an answer at or above
`min_directedness` starts a run.

In both places:

- Only that one new message, never the room's history, ids or the agents'
  instructions, goes to the sender's System One connection (`classifier_id`),
  cut to 4,000 characters.
- The run belongs to the sender and uses the sender's connections.
- An unaddressed message that started a run this way is stored with
  `addressed_by: "classifier"`, which no client can send. The run's activity
  shows how its agent was chosen, for example *Picked by classifier: Coder*.
- It never sees a people-only message, which never starts an agent.
- It never sees a message to a named agent (`@coder`, or Ask agent with an
  agent chosen).
- It never sees other members' messages because of someone else's opt-in.

When the per-Space setting was introduced, everyone who had
`infer_addressing` on with a System One connection got
`classify_unaddressed` on in each Space they were a member of then. That
happened once; Spaces joined later start with it off.

## Use-case routing

A person can map their use cases (for example *code review*, *quick
question*, *writing*) to chat connections they can use. It's a separate
opt-in from classification (`enabled`). It decides only which connection
answers, never whether the agent is addressed.

- It can run on any run that has started for that person: from an agent's
  `@handle`, `@agent`, Ask agent (mixed messages included), or a classified
  unaddressed message.
- **A model the person chooses explicitly for that turn overrides routing**,
  and so do their own per-agent and per-Space models in that Space. In a
  Space, it applies only while their `routing` preference for that Space is
  on (the default). See
  [which model a run uses](settings.md#which-model-a-run-uses).
- System One gets one `choice` over the person's case keys, plus a reserved
  `no_match` option.
  - A case is used only if it's chosen with probability at or above
    `min_route_probability` (default 0.65, range 0 to 1).
  - Its key then maps to a connection id from the person's own saved
    settings. Nothing the model returns is ever used as an id or endpoint.
- A valid `no_match`, or a weak match, uses the person's fallback chat
  connection: `default_chat_id` if set, otherwise their default chat
  connection.
- **A failed decision isn't a `no_match`.** If the System One connection
  errors, times out, answers malformed, or can't be used anymore, the turn
  fails visibly before any chat model is called. A chosen connection that
  can't be used fails the same way. Nothing falls back to another model.
- With routing off, the person's default chat connection answers, as before.
- The route actually used is recorded on the turn's `started` activity
  marker: where the model came from, the case, the connection and model ids,
  and a short reason. No prompt or credential is recorded.

Every System One request carries one question. A run costs at most two
calls: one agent decision (the Space `choice`, or the personal Chat `noul`,
only for someone who opted in), then the routing `choice`, only when the run
uses routing. A message that starts several agents makes one routing call
per run.

### Settings: `GET` / `PUT /v1/model-routing`

Each person has their own settings, and both features are off by default.

| Field | Meaning |
| --- | --- |
| `enabled` | Use-case routing on or off (default `false`) |
| `infer_addressing` | Classification of your own unaddressed messages **in your personal Chat** on or off (default `false`). In Spaces, use each Space's `classify_unaddressed` ([preferences](spaces.md#your-preferences-in-a-space)). |
| `classifier_id` | Your System One connection. It's required when either feature is on, and it's also used for per-Space classification and the search rerank. |
| `embedding_id` | Your embeddings connection, used for [search](#semantic-search) |
| `default_chat_id` | The chat connection to use on `no_match`. Leave it out to keep your default. |
| `min_directedness` | 0.5 to 1, default 0.85 |
| `min_route_probability` | 0 to 1, default 0.65 |
| `cases` | Up to 16 entries of `{"key", "label", "description", "connection_id"}`. Keys match `[a-z][a-z0-9_]*`, up to 32 characters, are unique, and can't be `no_match`. Labels are up to 64 characters, and descriptions, the criterion System One judges, up to 500. |

Each System One decision has a 10-second time limit, and at most 8 run at
once across the server.

## Semantic search

> **Status: component-tested, with the native desktop screen checked. Not
> released.**
>
> - The server code is in `crates/server/src/search.rs` and `search/`. It
>   passed 14 unit tests and 4 HTTP tests against a **loopback fixture** that
>   speaks the OpenAI embeddings wire and the System One wire
>   ([acceptance.md](acceptance.md#semantic-search-unit-and-http-tests-2026-11-22-search-lane-local)).
> - `tests/e2e/intelligence.py` showed the shortlist reordered by pointwise
>   System One calls over the real wire, to a fixture, with shared
>   embeddings connections across users.
> - The desktop **Search** screen was checked natively against the real
>   backend, with keyless embeddings and System One fixtures
>   ([screenshot](screenshots/semantic-search.png)).
>
> No real vendor, cloud account, TypeSafe account or Ollama server has been
> tested. You choose your connections with `embedding_id` and
> `classifier_id` in [your routing settings](#settings-get--put-v1model-routing).

**What you can search.** Only what you can already read:

- your own personal Chat, and
- the rooms and threads of Spaces you're a member of **at the moment of the
  request**.

Only people's messages and the agent's final replies are indexed. Tool calls,
tool output, activity, status, errors, thread mirrors and reactions are
never indexed, and neither are runner files or workspaces. A message that
contains a Metro credential prefix (`metro_rt_`, `metro_pc_`) is skipped.
Search is your own feature. It doesn't give the personal Chat agent access
to Space transcripts. Files you upload are a separate corpus with its own
search. See [Documents & media](library.md), which is in development. This
message search doesn't change.

**How a search runs.**

1. **Retrieval.** Messages are embedded with your own `embeddings`
   connection, and ranked by exact cosine similarity on normalized vectors.
   Vectors are stored in the server's SQLite database, per person, per
   source (your Chat, or one room or thread) and per model fingerprint. New
   messages are indexed incrementally.
2. **Rerank.** If you've chosen a System One connection, the top shortlist,
   **at most 10** candidates, is scored with real System One calls. There's
   one request per query and candidate pair, with a single `noul` question
   (`relevant`) that uses the same rubric for every pair, following
   TypeSafe's
   [re-ranking pattern](https://docs.typesafe.ai/cookbooks/rerank_typesafe).
   An answer that isn't a well-formed `noul` between 0 and 1 isn't used as a
   score, and nothing is guessed.
3. **Honest labels.** Each response has a `ranking` field:
   - `classifier` means System One scored every result.
   - `embeddings` means nothing was reranked, for example because you have
     no System One connection or the call failed. `notice` says why.
   - `mixed` means the reranked shortlist comes first, and the rest follow
     in retrieval order.

   `partial: true`, with a `notice`, means not everything was searched yet.

**In the desktop app** (current source after 0.2.0, not released). Type in
the search box at the top of the window (<kbd>Ctrl</kbd>+<kbd>K</kbd>, or
<kbd>⌘</kbd>+<kbd>K</kbd> on macOS, focuses it). While you type, [quick
results](#quick-results-as-you-type) appear under it. Press **Return**, or
choose **More results**, to run the full search described here. Changing a
filter doesn't run it. Search opens in
**Conversations** mode. Pick where to search with the **In** chips:
**Everything**, **Personal Chat**, or one of your Spaces. The
**Documents & media** mode searches your files instead
([library.md](library.md#in-the-desktop-app)).

- Each result shows the author, where the message is (for example *Acme ›
  #lobby* or *Personal Chat*) and its text with your query words marked.
  Pressing the result opens the conversation. Results show no raw score.
  Results with no measurable similarity are folded under **Less relevant**.
- The badge above the results says how they were ordered:
  - **Reranked by System One** (`classifier`) appears only when your System
    One connection actually scored every result.
  - **Partly reranked** (`mixed`) means System One scored the top results,
    and the rest follow in similarity order.
  - **Similarity order** (`embeddings`) means nothing was reranked. The
    notice says why.
- When the search is partial, the app says so and offers **Index more**.

Earlier native GTK runs of the source after 0.2.0, before quick results
were added, checked the full search's Return behaviour, the scope chips and
the folded **Less relevant** results. The keyboard shortcut is covered by
automated tests.

#### Quick results as you type

This describes the current source after 0.2.0. It isn't released. It was
checked in native Linux GTK, in dark and light themes, at `95a3d2b`
([acceptance](acceptance.md#staged-quick-search-as-you-type-2026-11-23-project-manager-local)). Windows and macOS weren't checked.

- About 220 ms after you stop typing, the app sends what you typed to your
  Metro server (`POST /v1/search/quick`) as a **literal** lookup: a
  substring match, case-insensitive for ASCII letters only. Nothing is
  embedded, indexed or reranked, and nothing goes to any model or
  connection. It works without an embeddings or media connection.
- It matches:
  - the text of people's messages and the agent's final replies in your
    personal Chat, and in the rooms and threads of Spaces you're a member
    of now;
  - the **names** (and so the extensions) of files in your personal library
    and in those Spaces' libraries, indexed or not.
- It never looks inside files. Text or a file name containing a Metro
  credential prefix never matches.
- Each conversation is looked up over its 10,000 most recent messages, in at
  most 100 rooms and threads, the most recently active first. Use full
  search for anything older.
- It shows at most 8 suggestions in a scrollable list under the box:
  - a message shows a snippet around the match and where it is;
  - a file is one compact row: a small preview tile, its name, at most one
    short line under it, and its extension as a badge. The app reads the
    preview from the file through your Metro session, and it's never sent
    to a model. Files and messages get their own headings when both are
    listed.
    - Text and Markdown files show the start of their text, and a PNG shows
      the image.
    - A WAV shows its real waveform and its length.
    - A video shows its first frame when the computer has the codec.
    - MP3 shows its title and artist tags, with no waveform. DOCX shows its
      text where it can be read. These two are covered by automated tests
      only.
    - Without a preview, such as for a PDF or a video with no codec, the
      tile shows the file type's icon. The row's accessible name gives the
      reason.
- Where it looks follows the page you're on:
  - in Search, the mode and scope you picked;
  - in Library, files only, from where **Show files from** points. **More
    results** keeps that scope;
  - anywhere else, files and messages across everything you can read.

  It never widens a page's filters.
- **Choosing a suggestion** opens that message or file. Your access is
  checked again when it opens.
- **More results**, always the last row, opens full Search with your query
  and runs it.
- **Return** with no suggestion chosen runs the full search.
- A query longer than 200 characters gets no quick lookup. Return still
  runs the full search, which takes up to 1,000.
- Editing the query, leaving the box, changing the scope, losing a Space or
  signing out drops any answer still on its way. Suggestions from a Space
  you've left are never shown.

Only the full search sends your query to your embeddings connection (and
a shortlist to System One), as described above. Quick results don't.

The [screenshot](screenshots/semantic-search.png) is historical: it shows
the 0.2.0-era page, with a query field and button on the page and a score
label on each result (*System One relevance 0.95*, *similarity 0.81*, or
just *score* for partly reranked results). It shows *Reranked by System
One* because the keyless fixture's System One endpoint did score every
result. Its scores and its order prove that the wiring works, and say
nothing about how good the ranking is with a real model. The API still
returns each result's `score`.

**Privacy checks.** Your access is checked when candidates are read, again
just before any text goes to a model, and again on the final results. Each
source's text goes only to a model call made for that source's own
destination (that Space, or no Space for your Chat), so one Space's content
never travels in a call made for another. If you leave a Space, its results
disappear and your vectors for it are deleted. A Space whose connection may
not be used for it is skipped and counted in `notice`, without being named.

**When the model changes.** Changing your embeddings connection or its model
changes the fingerprint. The old vectors are discarded and indexing starts
again, and searches report `partial` until it catches up. Vectors with an
unexpected dimension are refused.

### API

| Request | Result |
| --- | --- |
| `POST /v1/search` `{"query", "scope": "chat"\|"space"\|"all", "space_id"?, "limit"?}` | `{"results": [{"space_id", "room_id", "event_id", "actor", "text", "score"}], "ranking", "partial", "notice"?}`. `space_id` is required with `"space"` and refused otherwise. |
| `POST /v1/search/index` `{"scope", "space_id"?}` | `{"indexed", "skipped", "remaining", "done", "notice"?}`. It indexes the next bounded batch. Call it again until `done`. |
| `POST /v1/search/quick` `{"query", "scope": "all"\|"chat"\|"personal"\|"space", "space_id"?, "mode": "all"\|"conversations"\|"library", "kind"?: "document"\|"image"\|"audio"\|"video"}` | `{"assets": [Asset], "messages": [{"space_id", "room_id", "event_id", "actor", "text"}]}`, at most 8 in total, with `Cache-Control: no-store`. A literal lookup with no model, described [above](#quick-results-as-you-type). The query is trimmed, and is 1–200 characters. The full contract is in [`crates/server/src/search/API.md`](../crates/server/src/search/API.md#post-v1searchquick). |

A search also indexes a small batch before it ranks, so a small history
doesn't need `/index`.

| Status | When |
| --- | --- |
| `400` | Empty or over-long query, a bad scope and `space_id` combination, or a bad limit |
| `404` | A `space_id` you can't see |
| `409` | You haven't chosen an embeddings connection |
| `502` | The embedding provider failed for every source requested. The message never contains keys. |

### Limits (from the code)

| Limit | Value |
| --- | --- |
| Query | 1–1,000 characters |
| Results | `limit` 1–25, default 10 |
| Candidates reranked by System One | at most 10, with 4 calls in flight |
| Text sent for embedding / to System One / returned | 4,000 / 2,000 / 1,000 characters |
| Spaces looked at per request | 20 (your Chat is extra) |
| Vectors kept per person and source | 20,000 |
| Vectors scored per request | 100,000 |
| Embedding calls per search / per `/index` call | 24 / 16 |
| Time limit per embedding call / per System One call | 30 s / 20 s |
| Search and index requests at once, per server | 8 |

There are no operator settings for search. Which connections it uses
(`embedding_id`, and the optional `classifier_id` for System One) comes from
each person's own model-routing settings. Those will be documented when that
lane lands. The fixture spoke OpenAI's `POST /v1/embeddings`
([reference](https://platform.openai.com/docs/api-reference/embeddings/create))
and TypeSafe's `POST /v1/systemone`. Ollama's
[`/api/embed`](https://docs.ollama.com/api/embed) and every real vendor are
untested.

## Scheduled prompts

These are now documented from the code in [schedules.md](schedules.md). That
page covers the API, five-field cron in an IANA time zone, the limits, the
statuses and `METRO_SCHEDULE_TICK_SECS`. A scheduled prompt counts as its
creator explicitly asking the agent (in a Space, its default agent), so the
classifier never sees it. It
has been demonstrated end to end against the real server binary with
scripted models ([acceptance.md](acceptance.md#demonstrated-scheduled-prompts-end-to-end-2026-11-22-scheduler-lane-local)).

## Operator settings

For scheduled prompts, see [schedules.md](schedules.md#operator-setting).
Nothing else yet. Setting names and examples will be added here as each part lands,
copied from the code and checked against a recorded run.

## What will count as verified

A part moves to *verified* in the table at the top only when
[acceptance.md](acceptance.md) records the run that shows it. Fixture and
scripted-model runs will be labelled as such. A real TypeSafe, OpenAI,
Ollama or vendor-account run will be listed separately, and only if one is
actually made.
