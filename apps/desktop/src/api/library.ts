// Documents & media: typed wrappers for the upload library
// (crates/server/src/library.rs) and its media search
// (crates/server/src/library_search.rs). JSON calls go through the client's
// `request`; uploads and file bytes go through `BinaryFn`, the same
// transport with the bearer token in the Authorization header only. A file's
// URL never carries a credential, and the server answers `no-store`.
// No state, no timers, no UI.

import { ApiError } from "./client";
import type { RequestFn } from "./client";

/** The formats the server accepts (validate.rs): declared type, extension and signature must agree. */
export type AssetKind = "text" | "markdown" | "pdf" | "docx" | "png" | "jpeg" | "wav" | "mp3" | "mp4" | "webm";

/** Largest file the server accepts (HARD_MAX_FILE_BYTES); a server may be configured lower. */
export const MAX_UPLOAD_BYTES = 8 * 1024 * 1024;

/** What the system picker offers, and the MIME type each extension is sent as. */
export const ACCEPTED: readonly { kind: AssetKind; extensions: readonly string[]; mime: string; label: string }[] = [
  { kind: "text", extensions: ["txt"], mime: "text/plain", label: "Text" },
  { kind: "markdown", extensions: ["md", "markdown"], mime: "text/markdown", label: "Markdown" },
  { kind: "pdf", extensions: ["pdf"], mime: "application/pdf", label: "PDF" },
  { kind: "docx", extensions: ["docx"], mime: "application/vnd.openxmlformats-officedocument.wordprocessingml.document", label: "Word" },
  { kind: "png", extensions: ["png"], mime: "image/png", label: "PNG" },
  { kind: "jpeg", extensions: ["jpg", "jpeg"], mime: "image/jpeg", label: "JPEG" },
  { kind: "wav", extensions: ["wav"], mime: "audio/wav", label: "WAV" },
  { kind: "mp3", extensions: ["mp3"], mime: "audio/mpeg", label: "MP3" },
  { kind: "mp4", extensions: ["mp4"], mime: "video/mp4", label: "MP4" },
  { kind: "webm", extensions: ["webm"], mime: "video/webm", label: "WebM" },
];

/** The accepted format of a file name, by its extension; null when it isn't one. */
export function acceptedOf(name: string) {
  const dot = name.lastIndexOf(".");
  if (dot < 0) return null;
  const ext = name.slice(dot + 1).toLowerCase();
  return ACCEPTED.find((a) => a.extensions.includes(ext)) ?? null;
}

/** One library file's metadata, as its readers see it (never its bytes). */
export type LibraryAsset = {
  id: string;
  name: string;
  mimeType: string;
  kind: AssetKind;
  sizeBytes: number;
  /** null: the uploader's personal library. */
  spaceId: string | null;
  uploaderId: string;
  sha256: string;
  /** Unix milliseconds. */
  createdAt: number;
};

export type AssetPage = { assets: LibraryAsset[]; nextCursor: string | null; maxFileBytes: number | null };

/** Where a list or search looks: `all` is personal plus every Space the caller is a member of now (checked by the server). */
export type LibraryScope = { kind: "personal" } | { kind: "space"; spaceId: string } | { kind: "all" };
/** Where an upload goes: never `all`. */
export type UploadTarget = { kind: "personal" } | { kind: "space"; spaceId: string };

export type MediaKind = "document" | "image" | "audio" | "video";

/** The matching part of a file. `text`: character offsets; `page`: a 1-based page; `time`: milliseconds. */
export type Locator =
  | { kind: "text"; start: number | null; end: number | null }
  | { kind: "page"; page: number }
  | { kind: "image" }
  | { kind: "time"; startMs: number; endMs: number | null };

/**
 * `classifier`: a System One `noul` scored this result's text excerpt.
 * `cosine`: embedding similarity only (images, audio and video always).
 */
export type ScoreKind = "classifier" | "cosine";

export type LibraryHit = {
  assetId: string;
  name: string;
  mimeType: string;
  kind: MediaKind;
  spaceId: string | null;
  locator: Locator;
  excerpt: string | null;
  score: number;
  scoreKind: ScoreKind;
};

export type LibraryRanking = "classifier" | "embeddings" | "mixed";
export type LibrarySearchResponse = { results: LibraryHit[]; ranking: LibraryRanking; partial: boolean; notice: string | null };

export type IndexItem = { assetId: string; status: "indexed" | "not_found" | "unsupported" | "too_large" | "failed"; indexedSegments: number; partial: boolean; error: string | null };
export type IndexResult = { indexedSegments: number; partial: boolean; items: IndexItem[]; notice: string | null };

/** The person's media embedding choice. `suggestedId`: their message-search connection, when the server confirmed it media-capable; never used until saved. */
export type LibrarySettings = { embeddingId: string | null; suggestedId: string | null; selectionPrecondition: boolean };

/** A file's bytes, read through an authenticated request. */
export type AssetContent = { bytes: Uint8Array; contentType: string };

/**
 * The binary transport: the client's error mapping and timeout, bearer in
 * the Authorization header, `redirect: "error"`. `body` is raw bytes.
 */
export type BinaryFn = (
  method: "GET" | "POST",
  path: string,
  init: { body?: Blob | Uint8Array; contentType?: string; accept?: string; timeoutMs?: number; signal?: AbortSignal; range?: string },
) => Promise<{ status: number; contentType: string; bytes: Uint8Array; contentRange?: string | null }>;

const enc = encodeURIComponent;

function scopeQuery(scope: LibraryScope | UploadTarget): string {
  return scope.kind === "space" ? `scope=space&space_id=${enc(scope.spaceId)}` : `scope=${scope.kind}`;
}

function scopeBody(scope: LibraryScope): Record<string, unknown> {
  return scope.kind === "space" ? { scope: "space", space_id: scope.spaceId } : { scope: scope.kind };
}

export type LibraryApi = ReturnType<typeof libraryApi>;

export function libraryApi(request: RequestFn, binary: BinaryFn) {
  return {
    async listAssets(scope: LibraryScope, cursor?: string | null): Promise<AssetPage> {
      const data = await request("GET", `/v1/library/assets?${scopeQuery(scope)}${cursor ? `&cursor=${enc(cursor)}` : ""}`);
      if (!isObject(data) || !Array.isArray(data.assets)) throw new ApiError("protocol", "GET /v1/library/assets did not return { assets: [...] }.");
      return {
        assets: data.assets.map(toAsset).filter((a): a is LibraryAsset => a !== null),
        nextCursor: typeof data.next_cursor === "string" && data.next_cursor ? data.next_cursor : null,
        maxFileBytes: typeof data.max_file_bytes === "number" && data.max_file_bytes > 0 ? data.max_file_bytes : null,
      };
    },

    /** Uploads the file's own bytes. Uploading sends nothing to any model. */
    async uploadAsset(target: UploadTarget, name: string, mime: string, body: Blob | Uint8Array): Promise<LibraryAsset> {
      const res = await binary("POST", `/v1/library/assets?${scopeQuery(target)}&name=${enc(name)}`, { body, contentType: mime, accept: "application/json", timeoutMs: 120_000 });
      const asset = toAsset(parseJson(res.bytes, "POST /v1/library/assets"));
      if (!asset) throw new ApiError("protocol", "POST /v1/library/assets did not return the stored file.");
      return asset;
    },

    async getAsset(id: string): Promise<LibraryAsset> {
      const asset = toAsset(await request("GET", `/v1/library/assets/${enc(id)}`));
      if (!asset) throw new ApiError("protocol", "GET /v1/library/assets/{id} did not return the file's metadata.");
      return asset;
    },

    /** The file's bytes, through the server's access check, never cached (`no-store`). */
    async assetContent(id: string, signal?: AbortSignal): Promise<AssetContent> {
      const res = await binary("GET", `/v1/library/assets/${enc(id)}/content`, { accept: "*/*", timeoutMs: 120_000, signal });
      return { bytes: res.bytes, contentType: res.contentType };
    },

    /**
     * At most the first `maxBytes` of the file, through the same access check
     * (a single `Range: bytes=0-N` the server supports). A server that
     * answers with the whole file is cut here; `whole` says whether the bytes
     * are the entire file.
     */
    async assetContentPart(id: string, maxBytes: number, signal?: AbortSignal): Promise<AssetContent & { whole: boolean }> {
      const n = Math.max(1, Math.floor(maxBytes));
      const res = await binary("GET", `/v1/library/assets/${enc(id)}/content`, { accept: "*/*", timeoutMs: 60_000, signal, range: `bytes=0-${n - 1}` });
      const total = /\/(\d+)\s*$/.exec(res.contentRange ?? "")?.[1];
      if (res.status === 206) return { bytes: res.bytes.subarray(0, n), contentType: res.contentType, whole: total !== undefined && Number(total) <= res.bytes.length };
      return { bytes: res.bytes.length > n ? res.bytes.subarray(0, n) : res.bytes, contentType: res.contentType, whole: res.bytes.length <= n };
    },

    async deleteAsset(id: string): Promise<void> {
      await request("DELETE", `/v1/library/assets/${enc(id)}`);
    },

    async librarySettings(): Promise<LibrarySettings> {
      const data = await request("GET", "/v1/library/settings");
      if (!isObject(data) || (data.embedding_id !== null && !optionalId(data.embedding_id))) {
        throw new ApiError("protocol", "GET /v1/library/settings did not return a valid selection.");
      }
      return {
        embeddingId: optionalId(data.embedding_id),
        suggestedId: optionalId(data.suggested_id),
        selectionPrecondition: data.selection_precondition === true,
      };
    },

    async putLibrarySettings(embeddingId: string | null, expected: string | null): Promise<LibrarySettings> {
      // The legacy route has no precondition. Never use it, even as a retry.
      const data = await request("PUT", "/v1/library/settings/selection", { embedding_id: embeddingId, expected_embedding_id: expected });
      if (!isObject(data) || data.selection_precondition !== true || data.embedding_id !== embeddingId) {
        throw new ApiError("protocol", "The server did not confirm the media model selection.");
      }
      return { embeddingId, suggestedId: optionalId(data.suggested_id), selectionPrecondition: true };
    },

    /** Sends this one file to the chosen media model. */
    async indexAsset(assetId: string): Promise<IndexResult> {
      const data = await request("POST", "/v1/library/index", { asset_id: assetId });
      if (!isObject(data) || typeof data.indexed_segments !== "number") throw new ApiError("protocol", "POST /v1/library/index did not return { indexed_segments, partial }.");
      const items: IndexItem[] = [];
      for (const it of Array.isArray(data.items) ? data.items : []) {
        if (!isObject(it) || !isId(it.asset_id)) continue;
        const status = ["indexed", "not_found", "unsupported", "too_large", "failed"].includes(String(it.status)) ? (it.status as IndexItem["status"]) : "failed";
        items.push({
          assetId: String(it.asset_id),
          status,
          indexedSegments: typeof it.indexed_segments === "number" ? it.indexed_segments : 0,
          partial: it.partial === true,
          error: shortText(it.error, 300),
        });
      }
      return { indexedSegments: data.indexed_segments, partial: data.partial === true, items, notice: shortText(data.notice, 500) };
    },

    async searchLibrary(query: string, scope: LibraryScope, limit: number, kind?: MediaKind | null): Promise<LibrarySearchResponse> {
      const body: Record<string, unknown> = { query, ...scopeBody(scope), limit };
      if (kind) body.kind = kind;
      const data = await request("POST", "/v1/library/search", body);
      if (!isObject(data) || !Array.isArray(data.results)) throw new ApiError("protocol", "POST /v1/library/search did not return { results: [...] }.");
      const ranking: LibraryRanking = data.ranking === "classifier" || data.ranking === "mixed" ? data.ranking : "embeddings";
      return {
        results: data.results.map(toHit).filter((h): h is LibraryHit => h !== null),
        ranking,
        partial: data.partial === true,
        notice: shortText(data.notice, 500),
      };
    },
  };
}

// ------------------------------------------------------------------ parsing

function isObject(v: unknown): v is Record<string, unknown> {
  return typeof v === "object" && v !== null && !Array.isArray(v);
}
const isId = (v: unknown): v is string | number => typeof v === "string" || typeof v === "number";
const optionalId = (v: unknown): string | null => (isId(v) && String(v) !== "" ? String(v) : null);
const shortText = (v: unknown, max: number): string | null => (typeof v === "string" && v.trim() ? v.trim().slice(0, max) : null);
const KINDS: readonly string[] = ACCEPTED.map((a) => a.kind);
const count = (v: unknown): number | null => (typeof v === "number" && Number.isFinite(v) && v >= 0 ? Math.floor(v) : null);

function parseJson(bytes: Uint8Array, what: string): unknown {
  try {
    return JSON.parse(new TextDecoder().decode(bytes));
  } catch {
    throw new ApiError("protocol", `${what} answered with something that is not JSON.`);
  }
}

export function toAsset(v: unknown): LibraryAsset | null {
  if (!isObject(v) || !isId(v.id) || typeof v.name !== "string" || typeof v.kind !== "string" || !KINDS.includes(v.kind)) return null;
  return {
    id: String(v.id),
    name: v.name,
    mimeType: typeof v.mime_type === "string" ? v.mime_type : "application/octet-stream",
    kind: v.kind as AssetKind,
    sizeBytes: count(v.size_bytes) ?? 0,
    spaceId: optionalId(v.space_id),
    uploaderId: isId(v.uploader_id) ? String(v.uploader_id) : "",
    sha256: typeof v.sha256 === "string" ? v.sha256 : "",
    createdAt: count(v.created_at) ?? 0,
  };
}

function toLocator(v: unknown): Locator | null {
  if (!isObject(v)) return null;
  switch (v.kind) {
    case "text":
      return { kind: "text", start: count(v.start), end: count(v.end) };
    case "page": {
      const page = count(v.page);
      return page !== null && page >= 1 ? { kind: "page", page } : null;
    }
    case "image":
      return { kind: "image" };
    case "time": {
      const startMs = count(v.start_ms);
      return startMs === null ? null : { kind: "time", startMs, endMs: count(v.end_ms) };
    }
    default:
      return null;
  }
}

const MEDIA_KINDS: readonly string[] = ["document", "image", "audio", "video"];

function toHit(v: unknown): LibraryHit | null {
  if (!isObject(v) || !isId(v.asset_id) || typeof v.name !== "string") return null;
  const locator = toLocator(v.locator);
  if (!locator || typeof v.score !== "number" || !Number.isFinite(v.score)) return null;
  const mimeType = typeof v.mime_type === "string" ? v.mime_type : "application/octet-stream";
  const kind = typeof v.kind === "string" && MEDIA_KINDS.includes(v.kind) ? (v.kind as MediaKind) : mediaKindOf(mimeType);
  const excerpt = shortText(v.excerpt, 1000);
  return {
    assetId: String(v.asset_id),
    name: v.name,
    mimeType,
    kind,
    spaceId: optionalId(v.space_id),
    locator,
    excerpt,
    score: v.score,
    // A System One score needs a text excerpt: anything else is similarity.
    scoreKind: v.score_kind === "classifier" && excerpt !== null ? "classifier" : "cosine",
  };
}

export function mediaKindOf(mime: string): MediaKind {
  const base = mime.split(";")[0]!.trim().toLowerCase();
  if (base.startsWith("image/")) return "image";
  if (base.startsWith("audio/")) return "audio";
  if (base.startsWith("video/")) return "video";
  return "document";
}
