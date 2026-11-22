# Library search API contract (server ⇄ desktop)

Lane: `crates/server/src/library_search.rs`, `crates/server/src/library_search/**`.
This file is what the desktop UI and QA code against. It covers only what
the module implements. Message search (`POST /v1/search`) is separate and
unchanged.

All routes need a human bearer token. Bodies are JSON and share the 64 KiB
human body limit. Unknown body fields → `400`. Errors are `{"error": "…"}`;
show the message as it is.

## What is searched

- Only files someone **uploaded** to the library (`/v1/library/assets`):
  the caller's personal library, and the libraries of Spaces they are a
  member of **now**.
- Only files the caller **indexed themselves** with `POST /v1/library/index`.
  Each person has their own index, built with their own connection. Bob
  indexing a Space file does not make it searchable for Alice. Nothing is
  indexed automatically. Uploading, listing and searching never send a file
  anywhere.
- Indexing sends the file's bytes to the caller's chosen media embedding
  connection, for the file's own destination (no Space for personal files,
  the file's Space otherwise). Whoever runs that endpoint, including the
  owner of a shared device or connection, sees the bytes and segments sent.
  The UI must say so before indexing.
- Types that are indexed:
  - documents: `text/plain`, `text/markdown`, `application/pdf`, and
    DOCX (`application/vnd.openxmlformats-officedocument.wordprocessingml.document`)
  - images: `image/png`, `image/jpeg`
  - audio: `audio/wav` (and its variants), `audio/mpeg`
  - video: `video/mp4`, `video/webm`
- **PDF and DOCX:** the model (`google/embeddinggemma-2`) takes no native
  PDF or DOCX input. Metro sends the file's genuine bytes, after the access
  checks, and the reference model service prepares them:
  - PDF pages with text are extracted into text chunks with `page`
    locators and real excerpts.
  - PDF pages without text are rendered and embedded as images. They get a
    `page` locator and no excerpt (no OCR).
  - DOCX body text is extracted into `text` locators with excerpts.

  The service decides how to prepare each file. Metro never presents a raw
  PDF as native model input. If the service can't prepare a file (corrupt,
  encrypted, too many pages), indexing fails with a clear `502` and nothing
  is stored.
- Files of at most 8 MiB (`413` above that).

## `GET /v1/library/settings`

```json
{ "embedding_id": null, "suggested_id": "pc_…" }
```

- `embedding_id`: the media embedding connection the person chose, or
  `null`. It is separate from message search's `embedding_id` in
  `/v1/model-routing`, and it is never filled in automatically.
- `suggested_id`: present only while `embedding_id` is `null`, and only when
  the person's message-search embedding connection is confirmed
  media-capable (`media_inputs: true`). Show it as a suggestion. It is not
  used until it is saved with `PUT`.
- Pick connections from `GET /v1/models/available` entries with
  `capability: "embeddings"` and `media_inputs: true`.

## `PUT /v1/library/settings`

Request: `{ "embedding_id": "pc_…" | null }`. Response: the same shape as
`GET` (without `suggested_id`).

- `422`: the connection is not media-capable (a text-only embeddings
  connection never stands in for media).
- `409`: the connection is unknown or can't be used by the caller (the
  message is provider-safe).
- Choosing a **different** connection, or `null`, deletes the caller's whole
  library index. The UI should confirm first. Saving the same id again keeps
  the index.

## `POST /v1/library/index`

Request: exactly one of these.

```json
{ "asset_id": "…" }
{ "asset_ids": ["…", "…"] }
```

`asset_ids` holds 1 to 8 distinct ids. For more files, the UI loops.

Response `200`:

```json
{
  "indexed_segments": 3,
  "partial": false,
  "items": [
    { "asset_id": "…", "status": "indexed", "kind": "document",
      "indexed_segments": 3, "partial": false }
  ],
  "notice": "…"
}
```

- `status` is one of `indexed`, `not_found`, `unsupported`, `too_large` or
  `failed`. Every status except `indexed` comes with `error`, a message for
  people.
- `kind` (only for `indexed`) is one of `document`, `image`, `audio` or
  `video`.
- An item's `partial: true` means the model embedded only part of the file.
- The top-level `partial` is true if any item is partial or not indexed.
- Reindexing a file replaces its previous index.

With a single `asset_id`, failures are HTTP errors instead of items:

| Status | When |
| --- | --- |
| 400 | bad body |
| 404 | `"file not found"`: missing, or not readable by the caller now (another person's personal file, a Space they are not in, a membership removed during indexing) |
| 409 | `"choose a media embedding model in Settings to search your files"`; the connection can't be used for that file's Space; the index is full (50,000 segments); a change while the model ran: `"…changed, or its share was revoked, while the file was indexed; nothing was stored"` (setting switched, connection reconfigured, share revoked) or `"the file changed while it was indexed; index it again"` |
| 413 | file larger than 8 MiB |
| 415 | a type the server never sends (none of the library's accepted types: every type above is sent) |
| 422 | the chosen connection is not media-capable |
| 502 | the model or model service failed, refused the type, couldn't prepare the file, or answered outside the protocol. The message is provider-safe, for example `the media embedding model does not support video/webm files`. Nothing is stored. |
| 503 | busy, try again |

In bulk mode, the 409 for no model, the 409 for a full index, 503 and 500
still fail the whole request. Every other failure becomes that item's
status.

## `POST /v1/library/search`

```json
{ "query": "invoice terms", "scope": "personal" | "space" | "all",
  "space_id": "s-…", "limit": 10, "kind": "document" | "image" | "audio" | "video" }
```

- `query`: 1 to 1000 characters after trimming.
- `space_id`: required with `space` and refused otherwise (`400`). A Space
  the caller isn't in → `404 "space not found"`.
- `limit`: 1 to 25, default 10.
- `kind`: optional filter.
- `all` covers the personal library plus at most 20 Spaces. If more Spaces
  were left out, the response is `partial` with a notice.

Response `200`:

```json
{
  "results": [
    {
      "asset_id": "…", "name": "notes.txt", "mime_type": "text/plain",
      "kind": "document", "space_id": null,
      "locator": { "kind": "page", "page": 2 },
      "excerpt": "The invoice total and billing terms.",
      "score": 0.93, "score_kind": "cosine"
    }
  ],
  "ranking": "embeddings",
  "partial": false,
  "notice": "…"
}
```

- `locator` has only the fields of its kind:
  - `{kind:"page", page, start?, end?}`: a 1-based PDF page. `start`/`end`
    (both or neither, `start <= end`) are character offsets into that
    page's extracted text. A rendered page without text has neither and no
    excerpt.
  - `{kind:"text", start?, end?}`: offsets into the document's text (both or
    neither, `start <= end`). Offsets are always Unicode **code points**,
    never bytes:
    - for `text/plain` and `text/markdown`, into the file's text as the
      service reads it: UTF-8 with the BOM removed, `\r\n` and `\r` turned
      into `\n`, control and format characters dropped except `\n` and `\t`
    - for DOCX, into the extracted body text.
  - `{kind:"image"}`: the whole image.
  - `{kind:"time", start_ms, end_ms}`: for audio and video, `start_ms <=
    end_ms`.
  - `query` never appears in results.
- `excerpt`: documents only, at most 1000 characters, whitespace
  collapsed.
  - **Uploaded text files:** Metro derives the excerpt itself from the
    file's own bytes at the locator's code-point range. If the model's
    excerpt doesn't match that text (exactly, or as its `…`-cut prefix), or
    the range is missing or outside the text, there is no excerpt. Text a
    model made up is never shown or sent to System One.
  - **PDF and DOCX:** the model service's extracted text, the only text
    Metro has for them.
  - **Images, audio, video and rendered PDF pages:** never an excerpt. Any
    caption or transcript the model returns is dropped.
- At most 3 results come from one file. `name`, `mime_type` and `space_id`
  are the library's current values.
- `score_kind` is per result and says what `score` is:
  - `cosine`: cosine similarity of unit vectors, clamped to 0..1.
  - `classifier`: a System One `noul` for that document excerpt.

  The two are different measures, so don't compare them or display them as
  the same thing.
- `ranking`:
  - `embeddings`: nothing was reranked. Possible reasons: no System One
    connection chosen in `/v1/model-routing`, nothing eligible, or System
    One failed (`notice` says which).
  - `classifier`: **every** returned result was scored by System One.
  - `mixed`: System One reranked some document excerpts among the positions
    those documents already held. Images, audio and video (and any
    unreranked result) keep their embedding position and cosine score.
- System One only ever sees eligible **document** text excerpts, never a
  rendered PDF page, which has no excerpt: at most 10,
  one query and excerpt pair per call, each sent for its own source.
  Images, audio and video bytes or excerpts are never sent to it, and file
  names are never used for ranking.
- `partial: true` means not everything could be searched:
  - files indexed with a different model or dimension were dropped from
    the index ("index them again")
  - a Space's connection grant is missing (that Space is skipped and not
    named)
  - the per-request bound was hit
  - Spaces were left out.
- When nothing is indexed in scope, `results` is empty, the notice says
  only indexed files are searched, and **no model call is made**.

Expected `200` for a PDF and a DOCX (QA). Alice's personal `report.pdf`
has page 1 with text, page 2 a scan, and page 3 with text. `notes.docx` is
in Space `s-1`, and the caller indexed both.

```json
POST /v1/library/search {"query": "billing invoice", "scope": "all"}
{
  "results": [
    { "asset_id": "…", "name": "report.pdf", "mime_type": "application/pdf",
      "kind": "document", "space_id": null,
      "locator": { "kind": "page", "page": 3, "start": 18, "end": 54 },
      "excerpt": "The invoice total and billing terms.",
      "score": 0.9, "score_kind": "classifier" },
    { "asset_id": "…", "name": "notes.docx",
      "mime_type": "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
      "kind": "document", "space_id": "s-1",
      "locator": { "kind": "text", "start": 25, "end": 53 },
      "excerpt": "Send the invoice to billing.",
      "score": 0.71, "score_kind": "cosine" }
  ],
  "ranking": "mixed",
  "partial": false,
  "notice": "…"
}
```

A scanned or rendered PDF page comes back as `{"kind": "page", "page": 2}`
with no `excerpt` and `score_kind: "cosine"`, also when System One is
configured. An excerpt is never built from a file name. Optional extras in the service's response (per-row
`modality`, top-level `notes`) are accepted and not shown.

Errors:

| Status | When |
| --- | --- |
| 400 | bad query, scope, space_id, limit or kind |
| 404 | Space not visible |
| 409 | no media embedding connection chosen |
| 502 | every source failed upstream (transient: busy, offline, timeout) |
| 503 | busy |

## Access guarantees (QA)

- A file is checked through the library's own SQL access rules when it is
  read, right before its bytes leave, and again after the model answers.
  The store then checks membership again in the same transaction, so
  nothing is stored for someone removed mid-call. Metro also checks the
  uploader or membership itself, even if the library answer were wrong.
- Search reads index rows joined with current `space_members`. Each
  candidate is rechecked through the library before any System One call and
  again on the final results.
- Rows for files that changed (sha256), files gone, or Spaces the person
  left are deleted on the next request. Index rows reference
  `library_assets(id) ON DELETE CASCADE`, so deleting a file deletes
  everyone's index of it.
- Fingerprint and dimension: the provider fingerprint includes the
  connection, model, endpoint and config revision. A change drops the old
  vectors. A changed vector dimension under the same fingerprint is dropped
  too.
- **Definitive versus transient.** When the connection is definitively
  unusable for a library (deleted, share revoked, wrong capability, not a
  media adapter, no model, a device connection revoked), that person's rows
  for it are deleted even though no fingerprint can be read; `notice` says
  so. A transient failure (busy, offline, timeout, endpoint error) only
  skips that library and keeps its rows (`partial: true`).
- **Indexing races.** The result is stored in one immediate transaction
  that first rechecks:
  - the person's setting still names the connection the request used
  - the file still exists, with the same sha256 and Space, and they may
    still read it
  - the connection still resolves to the same grant and configuration
    (`providers::sharing::authorize`: same row, provider, model, config
    revision, endpoint and access)

  If any of these fails, nothing is written: a deleted file is `404`, not
  a 500. A slow request on an old model can't land vectors after a switch,
  a delete or a revocation.
- **Purges can't remove fresh rows.** Every row carries the connection it
  was built with and a write sequence number. A request's purges only touch
  rows written before it started, and only while the setting still names
  its connection. So a slow request holding an old fingerprint never
  deletes an index a newer request wrote with the fresh model.

## Owner hooks

These are in `library_search::hooks`. They run in the caller's connection
or transaction. Rows are also dropped lazily on the next request, so a
missed hook never exposes anything.

- `asset_deleted(conn, asset_id)`: also covered by the FK cascade.
- `space_deleted(conn, space_id)`
- `member_removed(conn, space_id, user_id)`: already called by
  `Db::remove_member` in the same transaction. It deletes only that
  person's rows for that Space and never the Space's files.
- For the provider owner (optional; they make purges immediate instead of
  on next use):
  - `connection_changed(conn, connection_id)`: a connection was
    reconfigured (model, endpoint, key, adapter) or deleted. Drops
    everyone's rows built with it.
  - `connection_access_changed(conn, connection_id, owner_id)`: a share
    was revoked or narrowed. Drops every row built with it by anyone but
    its owner. It may drop more than strictly needed, never less.
