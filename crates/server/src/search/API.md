# Search API contract (server ⇄ desktop)

Lane: `crates/server/src/search.rs`, `crates/server/src/search/**`. This file
is the agreement the desktop UI codes against. It covers only what the
module implements.

Everything requires a human bearer token (the same `Human` auth as the other
`/v1/*` routes). Request bodies are JSON and share the 64 KiB human body
limit.

## What gets searched

- **Chat**: the caller's own personal Chat (`personal_events` with
  `user_id = caller`). Nobody else's Chat is ever read.
- **Space**: every top-level room and child thread of a Space the caller is
  a member of at the moment of the request.
- Only events of kind `message` (a person's message) and `agent` (the
  agent's final reply) are indexed. Tool calls, tool results, activity,
  status, errors, thread mirrors and reactions are never indexed. Texts that
  contain a Metro credential prefix (`metro_rt_`, `metro_pc_`) are skipped.
- No runner files or workspaces are indexed.

## `POST /v1/search`

Request:

```json
{ "query": "deploy checklist", "scope": "chat" | "space" | "all", "space_id": "sp_…", "limit": 10 }
```

- `query`: required, trimmed, 1–1000 characters. Empty → `400`.
- `scope`: required. `space` requires `space_id`; `chat` and `all` must not
  send one (`400`).
- `space_id` of a Space the caller is not a member of (or that does not
  exist) → `404` (`"space not found"`), like every other Space route.
- `limit`: optional, 1–25, default 10.

Response `200`:

```json
{
  "results": [
    {
      "space_id": null,          // null for personal Chat
      "room_id": null,           // null for personal Chat; room or thread id otherwise
      "event_id": 42,            // the event id to scroll to (chat events or room events)
      "actor": "alice",          // display name as written
      "text": "…",               // the event text, at most 1000 characters (cut on a char boundary, "…" appended)
      "score": 0.83              // 0..1; classifier score when reranked, else cosine similarity clamped to 0..1
    }
  ],
  "ranking": "classifier" | "embeddings" | "mixed",
  "partial": false,
  "notice": "…"                  // optional, human-readable, show it verbatim
}
```

- `ranking`:
  - `classifier`: every returned result was scored by the caller's System
    One classifier connection.
  - `embeddings`: no result was reranked (no classifier chosen, or it
    failed). `notice` then always says why.
  - `mixed`: the classifier scored the top shortlist (at most 10) and the
    rest follow in embedding order, or reranking failed for some sources.
    Reranked results always come first.
- `partial: true` means not everything was searched: some messages are not
  indexed yet, a source hit the per-request bound, or a Space was skipped.
  `notice` says what happened and how to continue (search again, or call
  `POST /v1/search/index` until `remaining` is 0).
- Ties are broken deterministically: higher score first, then Chat before
  Spaces, then `space_id`, then `room_id`, then higher (newer) `event_id`.

Errors (`{"error": "…"}`):

| Status | When |
| --- | --- |
| 400 | empty or too long query, bad scope/space_id combination, bad limit |
| 404 | `space_id` not visible to the caller |
| 409 | no embedding connection chosen (`"choose an embedding model in Settings to use search"`) |
| 502 | the embedding provider failed for every requested source (message is provider-safe, never contains keys) |

A Space whose connection may not be used for it (a destination-restricted
connection), or any source whose embedding call failed while others
succeeded, is omitted. The response then has `partial: true` and a
`notice` naming how many Spaces were skipped. Inaccessible Spaces are never
named.

## `POST /v1/search/index`

Indexes the next bounded batch for the caller, so a client can show
progress and continue on large histories.

Request: `{ "scope": "chat" | "space" | "all", "space_id": "sp_…" }` (same
rules as above).

Response `200`:

```json
{
  "indexed": 64,      // events embedded and stored by this call
  "skipped": 3,       // events looked at but not indexable (empty, credential-like, too long is truncated not skipped)
  "remaining": 120,   // indexable events still waiting, after this call, across the requested sources (exact count)
  "done": false,      // remaining == 0 and no source was skipped
  "notice": "…"       // optional, as for search
}
```

Call it again while `done` is false. `POST /v1/search` also indexes a small
bounded batch per source before it ranks, so for small histories the UI
never needs to call `/index`.

## Model settings

Search uses the caller's own routing preferences
(`model_routing::preferences`): `embedding_id` (required) and
`classifier_id` (optional). Changing either connection or its model changes
the fingerprint: old vectors are discarded and indexing restarts; the next
search reports `partial: true` until it catches up.
