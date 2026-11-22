# System One, semantic search, shared connections and scheduled prompts

> **Status: implemented, and every local gate passed. Commercial vendors
> haven't been validated.** See the
> [current local gate](acceptance.md#demonstrated-final-local-gate-with-documents--media-2026-11-22-primary-workspace-local):
> 497 Rust tests, all seven e2e scripts, and desktop `nsh test` 106/106.
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

- A message to people only (`@bob`, with no `@agent` and not sent with Ask
  agent) never starts the agent.
- Only `@agent` or **Ask agent** starts agent work, including in a mixed
  `@bob @agent` message. Text with no mention is ordinary conversation.
- A run uses the default model connection of the person who started it, or
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
  - `noul`, the probability of yes from 0 to 1, for addressing and the
    search rerank;
  - `choice`, one option with `confidence` and per-option `probabilities`,
    for use-case routing.

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
makes two separate decisions, each with its own opt-in:

- **Addressing:** whether an unaddressed message is asking the agent. This
  is [classification](#opt-in-classification-of-unaddressed-messages).
- **Answering model:** which chat connection answers a turn that has already
  started. This is [use-case routing](#use-case-routing).

| Message | Starts the agent? | Addressing decision | Answering model |
| --- | --- | --- | --- |
| People only: `@bob`, no `@agent`, not sent with Ask agent | Never | None. It's never sent to System One. | None. There's no turn. |
| To the agent: `@agent` or Ask agent, including mixed `@bob @agent` | Yes | None. Explicit recipients already decide it. | The model the person chose for this turn, if any; otherwise routing, if on; otherwise their default chat connection |
| Unaddressed, with the sender's classification **off** (the default) | No | None | None. There's no turn. |
| Unaddressed, with the sender's classification **on** | Only if System One says it's asking the agent | System One, on the sender's `system_one` connection | Same order as the row above |

## Opt-in classification of unaddressed messages

A person can turn on classification for **their own** outgoing messages, in
Space rooms or in their personal Chat. It decides only one thing: whether an
**unaddressed** message (no mention, and not sent with Ask agent) is asking
the agent.

- Only that one new message, never the room's history, goes to the sender's
  System One connection (`classifier_id`), as a single `noul`, cut to
  4,000 characters.
- Only an answer at or above `min_directedness` starts a run. The default is
  0.85, and it can be set anywhere from 0.5 to 1. The run belongs to the
  sender and uses the sender's connections.
- No answer, an uncertain answer, a malformed answer or an error starts
  nothing. The message stays ordinary conversation.
- A message that started a run this way is stored with `addressed_by:
  "classifier"`, which no client can send.
- It never sees a people-only message, which never starts the agent.
- It never sees a message to the agent. `@agent` and Ask agent, including a
  mixed `@bob @agent`, skip only this decision. Use-case routing can still
  pick their answering model.
- It never sees other members' messages because of someone else's opt-in.

With classification off (`infer_addressing: false`), which is the default,
unaddressed text stays ordinary conversation.

## Use-case routing

A person can map their use cases (for example *code review*, *quick
question*, *writing*) to chat connections they can use. It's a separate
opt-in from classification (`enabled`). It decides only which connection
answers, never whether the agent is addressed.

- It can run on any turn that has started for that person: from `@agent`,
  from Ask agent (mixed messages included), or from a classified unaddressed
  message.
- **A model the person chooses explicitly for that turn overrides routing.**
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

A message costs at most two System One calls: the addressing `noul` (only
for unaddressed messages from someone who opted in), then the routing
`choice`, only after a clear yes or on an explicit agent turn.

### Settings: `GET` / `PUT /v1/model-routing`

Each person has their own settings, and both features are off by default.

| Field | Meaning |
| --- | --- |
| `enabled` | Use-case routing on or off (default `false`) |
| `infer_addressing` | Classification of your own unaddressed messages on or off (default `false`) |
| `classifier_id` | Your System One connection. It's required when either feature is on, and it's also used for the search rerank. |
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

**In the desktop app.** Open **Search** in the sidebar, type what you're
looking for, and pick where to search: **Everything you can read**, **Your
personal Chat**, or one of your Spaces.

- Each result shows the author, where the message is (for example *Acme ›
  #lobby* or *Personal Chat*), its text, a score label, and **Open
  conversation**.
- The badge above the results says how they were ordered:
  - **Reranked by System One** (`classifier`) appears only when your System
    One connection actually scored every result.
  - **Partly reranked** (`mixed`) means System One scored the top results,
    and the rest follow in similarity order.
  - **Similarity order** (`embeddings`) means nothing was reranked. The
    notice says why.
- The score label names what the number is:
  - **System One relevance 0.95** when every result was reranked. It's the
    System One `noul`.
  - **similarity 0.81** when none was. It's the cosine similarity, clamped
    to 0–1.
  - Just **score** for partly reranked results, because the numbers then mix
    the two scales.

  It's a ranking signal for that search, not a measure of answer quality.
- When the search is partial, the app says so and offers to index more.

The [screenshot](screenshots/semantic-search.png) shows *Reranked by System
One* because the keyless fixture's System One endpoint did score every
result. Its scores and its order prove that the wiring works, and say
nothing about how good the ranking is with a real model.

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
creator explicitly asking the agent, so the classifier never sees it. It
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
