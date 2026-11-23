# Search API contract (server ⇄ desktop)

Lane: `crates/server/src/search.rs`, `crates/server/src/search/**`. This file
is the agreement the desktop UI codes against. It covers only what the
module implements, plus the quick lookup at the end, which lives in
`crates/server/src/quick_search.rs`.

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

## `POST /v1/search/quick`

A literal, model-free lookup for typeahead. It's implemented in
`crates/server/src/quick_search.rs`, outside this module. Nothing is
embedded, indexed, cached or sent to a model, and no file bytes are read.
It needs no embeddings, System One or media connection.

Request:

```json
{ "query": "deploy", "scope": "all" | "chat" | "personal" | "space", "space_id": "sp_…",
  "mode": "all" | "conversations" | "library", "kind": "document" | "image" | "audio" | "video" }
```

- `query`: required. It's trimmed, and must then be 1–200 Unicode
  characters. Empty or longer → `400`.
- `scope`: required. It never widens.
  - `chat`: the personal Chat's messages only.
  - `personal`: the personal library only.
  - `space`: one Space's rooms, threads and library. It needs `space_id`.
  - `all`: all of these.
- `space_id`: only with `"space"`. It's refused with any other scope
  (`400`). Missing with `"space"` → `400`. A Space the caller isn't a member
  of, a Space that doesn't exist, or a malformed id → `404` (`"space not
  found"`).
- `mode`: required. `conversations` returns messages only, `library` files
  only, and `all` both.
- `kind`: optional. It narrows files to one category: `document` is TXT,
  Markdown, PDF and DOCX; `image` is PNG and JPEG; `audio` is WAV and MP3;
  `video` is MP4 and WebM. It's refused with `mode: "conversations"`
  (`400`).
- Unknown fields are refused. A malformed JSON body gets the JSON
  extractor's own status, with its message.

Matching:

- The query is a plain substring. It's case-insensitive for ASCII only, and
  `%`, `_` and `\` are matched literally.
- **Messages:** only `message` and `agent` events in the caller's own
  personal Chat, and in the rooms and threads of Spaces they're a member of
  at the time of the request.
  - Each conversation is searched over its 10,000 most recent messages.
  - At most 100 readable rooms and threads are searched, the most recently
    active first.
- **Files:** library asset **names**, and so their extensions, that the
  caller may read now: their personal library and their Spaces' libraries.
  Unindexed files match too. File contents are never matched.
- Text or a file name that contains a Metro credential prefix (`metro_rt_`,
  `metro_pc_`, or the managed-account token prefix) never matches. It's
  excluded in SQL before any limit is applied.
- Visibility is checked in the same SQL statement that returns each row.
  The whole lookup runs under one hold of the database lock, so every row
  reflects the same moment.

Response `200`, with `Cache-Control: no-store`:

```json
{
  "assets": [Asset],                 // the library's Asset shape (docs/library.md), newest first
  "messages": [
    { "space_id": null, "room_id": null, "event_id": 42, "actor": "alice", "text": "…" }
  ]                                  // newest first; null ids mean the personal Chat
}
```

- At most **8** results, messages and files together. When both match, each
  kind keeps up to 4 if the other has more than 4.
- A message's `text` is a snippet of at most 160 characters around its
  first match, with whitespace flattened and `…` where it was cut.

The desktop sends this about 220 ms after the last keystroke, for queries of
200 characters or fewer. Return and **More results** call `POST /v1/search`
or `POST /v1/library/search`, never this route.
