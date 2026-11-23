// Documents & media: the user's uploaded files, the media model they chose,
// manual indexing, media search, and the viewer that opens a match.
//
// - Files reach the server only from the system's own picker, as their own
//   bytes, to the personal library or a Space the user is in now.
// - Uploading sends nothing to a model. Only "Index" sends one file, to the
//   media connection the user saved.
// - File bytes are fetched with the bearer in the Authorization header (the
//   client's binary transport), kept in memory only while the viewer shows
//   them, shown through `blob:` URLs (audio and video through a private
//   preview file where the platform's player can't play `blob:`, as on
//   Linux), and dropped (URLs revoked, files deleted) when the
//   viewer closes, the session ends or the file's Space is no longer the
//   user's. A late answer for a closed viewer, a previous session or a lost
//   Space is thrown away.

import { batch, computed, effect, Picker as SystemPicker, signal, untracked } from "nsh";
import { mkdir, readdir, unlink, writeFile } from "fs/promises";
import proc from "process";
import { homedir } from "os";
import { ApiError } from "../../api/client";
import type { AvailableModel } from "../../api/client";
import { ACCEPTED, acceptedOf, MAX_UPLOAD_BYTES, mediaKindOf } from "../../api/library";
import type { AssetKind, LibraryApi, LibraryAsset, LibraryHit, LibraryRanking, LibraryScope, Locator, MediaKind, UploadTarget } from "../../api/library";
import { ensurePrivateDir } from "./privateDir";
import { apiClient, describe, disconnect, isAborted, mainPlace, me, onSessionEnd, sessionGuard, spaces } from "../../store";

export type { LibraryAsset, LibraryHit, LibraryRanking, LibraryScope, Locator, MediaKind, UploadTarget };

export const LIBRARY_SEARCH_LIMIT = 10;
export const MAX_QUERY_CHARS = 1000;
/** Files one pick may upload. */
export const MAX_PICK = 10;

/** Which mode Search shows: message search, or search across Documents & media. */
export const searchSurface = signal<"conversations" | "library">("conversations");

/** A file's type for its icon: what it is, not its format. */
export function fileKindOf(kind: AssetKind | null, mimeType: string): MediaKind {
  const mime = kind ? ACCEPTED.find((a) => a.kind === kind)?.mime ?? mimeType : mimeType;
  return mediaKindOf(mime);
}

// ------------------------------------------------------------------ files

export const listScope = signal<LibraryScope>({ kind: "all" });
export const assets = signal<LibraryAsset[]>([]);
export const assetsLoaded = signal(false);
export const loadingAssets = signal(false);
export const assetsError = signal<string | null>(null);
export const nextCursor = signal<string | null>(null);
/** The server's configured limit, once a list said it (never above 8 MiB). */
export const maxFileBytes = signal(MAX_UPLOAD_BYTES);

export const uploadTarget = signal<UploadTarget>({ kind: "personal" });
export const uploading = signal(false);
export type UploadOutcome = { key: string; name: string; ok: boolean; message: string };
export const uploadOutcomes = signal<UploadOutcome[]>([]);

export type IndexState = { state: "indexing" | "indexed" | "failed"; message: string };
export const indexStates = signal<Record<string, IndexState>>({});

export const confirmingDelete = signal<string | null>(null);
export const deleting = signal<string | null>(null);
export const fileNotice = signal<string | null>(null);

// ------------------------------------------------------------------ media model

export const mediaModels = signal<AvailableModel[]>([]);
export const savedModelId = signal<string | null>(null);
export const suggestedModelId = signal<string | null>(null);
export const modelDraft = signal<string | null>(null);
export const modelsLoaded = signal(false);
export const modelError = signal<string | null>(null);
export const savingModel = signal(false);

export const savedModel = computed(() => mediaModels().find((m) => m.connectionId === savedModelId()) ?? null);
export const draftModel = computed(() => mediaModels().find((m) => m.connectionId === modelDraft()) ?? null);

// ------------------------------------------------------------------ search

export const libQuery = signal("");
export const libScope = signal<LibraryScope>({ kind: "all" });
export const libKind = signal<MediaKind | null>(null);
export type LibraryAnswer = { query: string; scope: LibraryScope; results: LibraryHit[]; ranking: LibraryRanking; partial: boolean; notice: string | null };
export const libAnswer = signal<LibraryAnswer | null>(null);
export const libSearching = signal(false);
export const libSearchError = signal<{ message: string; needsModel: boolean } | null>(null);

// ------------------------------------------------------------------ viewer

/**
 * What the viewer opens: a file, and the matching part when it came from a
 * search. `from` is the page it opened on (the Library's file list, or
 * Search's results); it shows only there and closes when that page goes.
 */
export type ViewTarget = {
  assetId: string;
  name: string;
  mimeType: string;
  kind: AssetKind | null;
  spaceId: string | null;
  locator: Locator | null;
  excerpt: string | null;
  query: string | null;
  from: "library" | "search";
};
export type ViewContent =
  | { status: "loading" }
  | { status: "error"; message: string }
  | { status: "text"; before: string; match: string; after: string; located: boolean; truncatedTop: boolean; truncatedBottom: boolean }
  | { status: "image"; url: string }
  | { status: "player"; url: string; audioOnly: boolean }
  | { status: "document"; sizeBytes: number };

export const viewing = signal<ViewTarget | null>(null);
export const viewContent = signal<ViewContent | null>(null);
export const seekToken = signal(0);
export const playing = signal(false);
export const playerError = signal<string | null>(null);
/** The player's own position and length, in seconds, from its progress events. */
export const playerPosition = signal<{ current: number; duration: number } | null>(null);
export const saveNotice = signal<{ ok: boolean; message: string } | null>(null);
export const saving = signal(false);

let seq = 0;
let viewSeq = 0;
let viewAbort: AbortController | null = null;
/** The open file's bytes, for "Save a copy"; dropped with the viewer. */
let viewBytes: Uint8Array | null = null;
let viewUrl: string | null = null;
/** A private preview file, only when the platform's player can't play `blob:` URLs; deleted with the viewer. */
let viewFile: string | null = null;

/** Where such preview files live: the app's own data directory, emptied at start, on close and on sign-out. */
const previewDir = () => (proc.dataDir ? `${proc.dataDir}/media-preview` : null);

function removePreviewFile(path: string | null) {
  if (path) void unlink(path).catch(() => {});
}

/** This run's token and session count: a cleanup removes only files of an earlier run or session, never one just written. */
const RUN = crypto.randomUUID().slice(0, 8);
let previewSession = 0;
const previewPrefix = () => `${RUN}-${previewSession}-`;

/** Deletes any preview file left behind (a crash, a kill, an ended session); this session's own stay. */
function purgePreviews() {
  const dir = previewDir();
  if (!dir) return;
  const keep = previewPrefix();
  void readdir(dir)
    .then((names) => Promise.all(names.filter((n) => !n.startsWith(keep)).map((n) => unlink(`${dir}/${n}`).catch(() => {}))))
    .catch(() => {});
}
purgePreviews();

// ------------------------------------------------------------------ access

const METHODS = ["listAssets", "uploadAsset", "assetContent", "deleteAsset", "librarySettings", "putLibrarySettings", "indexAsset", "searchLibrary"] as const;

/** The session's library calls, or null when signed out. */
export function library(): LibraryApi | null {
  const c = apiClient() as unknown as Partial<LibraryApi> | null;
  if (!c) return null;
  if (METHODS.some((m) => typeof c[m] !== "function")) throw new ApiError("protocol", "This build of the app can't reach Documents & media.");
  return c as LibraryApi;
}

/** What to show for a failed call, or null when nothing should be shown (aborted, or a 401 that ends the session). */
function failure(error: unknown, what: string): string | null {
  if (isAborted(error)) return null;
  if (error instanceof ApiError && error.kind === "auth") {
    disconnect(error.message);
    return null;
  }
  return describe(error, what);
}

const statusOf = (error: unknown) => (error instanceof ApiError ? error.status : null);

function serverDetail(error: unknown): string | null {
  if (!(error instanceof ApiError)) return null;
  const m = /HTTP \d{3}\)?: (.+?)\.?$/.exec(error.message);
  return m ? m[1]! : null;
}

const joinedIds = () => new Set(spaces().map((s) => s.id));
export const spaceName = (spaceId: string) => spaces().find((s) => s.id === spaceId)?.name ?? "a Space";
const isJoined = (spaceId: string | null) => spaceId === null || spaces().some((s) => s.id === spaceId);

export function scopeLabel(scope: LibraryScope | UploadTarget): string {
  if (scope.kind === "all") return "Everything you can access";
  if (scope.kind === "personal") return "Your personal library";
  return spaceName(scope.spaceId);
}

/** Files of the personal library or of Spaces the user is in now. Nothing else is shown. */
export const visibleAssets = computed(() => {
  const joined = joinedIds();
  return assets().filter((a) => a.spaceId === null || joined.has(a.spaceId));
});

/** Results from the personal library or Spaces the user is in now, minus files deleted here since. */
const deletedIds = signal<ReadonlySet<string>>(new Set());
export const visibleHits = computed(() => {
  const answer = libAnswer();
  if (!answer) return [];
  const joined = joinedIds();
  const gone = deletedIds();
  return answer.results.filter((h) => (h.spaceId === null || joined.has(h.spaceId)) && !gone.has(h.assetId));
});
/** Results the server returned that the user can't read (any more): a lost Space or a deleted file. */
export const hiddenHits = computed(() => (libAnswer()?.results.length ?? 0) - visibleHits().length);

/**
 * A readable result with no measurable embedding similarity (a cosine score
 * of 0 or below). System One scores (`classifier`) are never folded: their
 * provenance stays as the server said.
 */
export const isLessRelevant = (h: LibraryHit) => h.scoreKind === "cosine" && !(h.score > 0);
/** Readable results shown at once. */
export const relevantHits = computed(() => visibleHits().filter((h) => !isLessRelevant(h)));
/** Readable results folded under "Less relevant", closed until the user opens it. */
export const lessRelevantHits = computed(() => visibleHits().filter(isLessRelevant));
export const lessRelevantOpen = signal(false);
/** What the results list draws now: the relevant ones, then the folded ones while open. */
export const shownHits = computed(() => (lessRelevantOpen() ? [...relevantHits(), ...lessRelevantHits()] : relevantHits()));

/** The uploader may delete; so may the owner of the file's Space. The server decides again. */
export function canDelete(asset: LibraryAsset): boolean {
  if (asset.uploaderId !== "" && asset.uploaderId === me()) return true;
  return asset.spaceId !== null && spaces().find((s) => s.id === asset.spaceId)?.role === "owner";
}

// ------------------------------------------------------------------ loading

/** Loads (or reloads) the first page of files and the media model choice: the Library page. */
export async function openLibrary(): Promise<void> {
  await Promise.all([loadAssets(false), loadModels()]);
}

/** Search's Documents & media mode needs only the media model choice (to say where a query goes). */
export async function openLibrarySearch(): Promise<void> {
  if (!modelsLoaded()) await loadModels();
}

export function setListScope(scope: LibraryScope) {
  if (scope.kind === "space" && !isJoined(scope.spaceId)) return;
  listScope.set(scope);
  void loadAssets(false);
}

export async function loadAssets(more: boolean): Promise<void> {
  const scope = listScope();
  const ok = sessionGuard(scope.kind === "space" ? scope.spaceId : undefined);
  const mine = ++seq;
  const cursor = more ? nextCursor() : null;
  if (more && !cursor) return;
  batch(() => {
    loadingAssets.set(true);
    assetsError.set(null);
  });
  try {
    const api = library();
    if (!api) return;
    const page = await api.listAssets(scope, cursor);
    if (!ok() || mine !== seq) return;
    batch(() => {
      const known = more ? assets() : [];
      const seen = new Set(known.map((a) => a.id));
      assets.set([...known, ...page.assets.filter((a) => !seen.has(a.id))]);
      nextCursor.set(page.nextCursor);
      if (page.maxFileBytes) maxFileBytes.set(Math.min(page.maxFileBytes, MAX_UPLOAD_BYTES));
      assetsLoaded.set(true);
    });
  } catch (error) {
    if (!ok() || mine !== seq) return;
    if (statusOf(error) === 404 && scope.kind === "space") {
      batch(() => {
        listScope.set({ kind: "all" });
        assets.set([]);
        assetsError.set("You're not a member of that Space (any more), so its files aren't listed. Showing everything you can access.");
      });
      void loadAssets(false);
      return;
    }
    const message = failure(error, "Couldn't list your files");
    if (message) assetsError.set(message);
  } finally {
    if (ok() && mine === seq) loadingAssets.set(false);
  }
}

export async function loadModels(): Promise<void> {
  const ok = sessionGuard();
  modelError.set(null);
  try {
    const api = library();
    const c = apiClient();
    if (!api || !c) return;
    const [models, settings] = await Promise.all([c.availableModels(), api.librarySettings()]);
    if (!ok()) return;
    batch(() => {
      // Only adapters that embed media themselves: a text-only embeddings connection never stands in.
      mediaModels.set(models.filter((m) => m.capability === "embeddings" && m.mediaInputs === true && m.ready !== false));
      savedModelId.set(settings.embeddingId);
      suggestedModelId.set(settings.suggestedId);
      modelDraft.set(settings.embeddingId);
      modelsLoaded.set(true);
    });
  } catch (error) {
    if (!ok()) return;
    if (statusOf(error) === 404) {
      modelError.set("This server has no Documents & media search (GET /v1/library/settings answered 404).");
      return;
    }
    const message = failure(error, "Couldn't load media models");
    if (message) modelError.set(message);
  }
}

export async function saveModel(): Promise<void> {
  const id = modelDraft();
  if (id !== null && !mediaModels().some((m) => m.connectionId === id)) return;
  const ok = sessionGuard();
  batch(() => {
    savingModel.set(true);
    modelError.set(null);
  });
  try {
    const api = library();
    if (!api) return;
    const saved = await api.putLibrarySettings(id);
    if (!ok()) return;
    batch(() => {
      savedModelId.set(saved.embeddingId);
      modelDraft.set(saved.embeddingId);
      if (saved.suggestedId) suggestedModelId.set(saved.suggestedId);
    });
  } catch (error) {
    if (!ok()) return;
    const detail = serverDetail(error);
    const status = statusOf(error);
    if (status === 422 || status === 409 || status === 400) {
      modelError.set(`That connection can't be used for Documents & media${detail ? `: ${detail}` : ""}.`);
      return;
    }
    const message = failure(error, "Couldn't save the media model");
    if (message) modelError.set(message);
  } finally {
    if (ok()) savingModel.set(false);
  }
}

// ------------------------------------------------------------------ upload

export function setUploadTarget(target: UploadTarget) {
  if (target.kind === "space" && !isJoined(target.spaceId)) return;
  uploadTarget.set(target);
}

const FILTER_EXTENSIONS = ACCEPTED.flatMap((a) => [...a.extensions]);

export function formatSize(bytes: number): string {
  if (bytes < 1024) return `${bytes} B`;
  if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(bytes < 10 * 1024 ? 1 : 0)} KB`;
  return `${(bytes / (1024 * 1024)).toFixed(1)} MB`;
}

let outcomeKey = 0;

/** Shows the system's file picker, then uploads each picked file's bytes to the chosen target. */
export async function chooseAndUpload(): Promise<void> {
  if (uploading()) return;
  const target = uploadTarget();
  if (target.kind === "space" && !isJoined(target.spaceId)) return;
  const ok = sessionGuard(target.kind === "space" ? target.spaceId : undefined);
  const limit = maxFileBytes();
  batch(() => {
    uploading.set(true);
    fileNotice.set(null);
  });
  const outcomes: UploadOutcome[] = [];
  const note = (name: string, good: boolean, message: string) => outcomes.push({ key: `u${++outcomeKey}`, name, ok: good, message });
  try {
    let picked;
    try {
      picked = await SystemPicker.pickFiles({ multiple: true, limit: MAX_PICK, filters: [{ name: "Documents & media", extensions: FILTER_EXTENSIONS }], maxSize: limit });
    } catch (error) {
      if (ok()) fileNotice.set(`The file picker couldn't open: ${String((error as Error)?.message ?? error)}`);
      return;
    }
    if (!ok() || picked.length === 0) return;
    const api = library();
    if (!api) return;
    for (const p of picked) {
      if (!ok()) return;
      // The Space may have been lost while the picker was open.
      if (target.kind === "space" && !isJoined(target.spaceId)) {
        note(p.name, false, `Not uploaded: you're no longer a member of ${spaceName(target.spaceId)}.`);
        continue;
      }
      const format = acceptedOf(p.name);
      if (!format) {
        note(p.name, false, "Not uploaded: this type isn't supported. Use TXT, MD, PDF, DOCX, PNG, JPEG, WAV, MP3, MP4 or WebM.");
        continue;
      }
      if (p.status === "tooLarge" || !p.file || p.size > limit) {
        note(p.name, false, `Not uploaded: it's ${formatSize(p.size)}; files are limited to ${formatSize(limit)}.`);
        continue;
      }
      if (p.size === 0) {
        note(p.name, false, "Not uploaded: the file is empty.");
        continue;
      }
      try {
        const asset = await api.uploadAsset(target, p.name, format.mime, p.file);
        if (!ok()) return;
        note(asset.name, true, `Uploaded to ${scopeLabel(target)} (${formatSize(asset.sizeBytes)}). Nothing was sent to a model.`);
        const scope = listScope();
        const belongs = scope.kind === "all" || (scope.kind === "personal" && asset.spaceId === null) || (scope.kind === "space" && asset.spaceId === scope.spaceId);
        if (belongs) assets.set([asset, ...assets().filter((a) => a.id !== asset.id)]);
      } catch (error) {
        if (!ok()) return;
        if (error instanceof ApiError && error.kind === "auth") {
          disconnect(error.message);
          return;
        }
        if (isAborted(error)) return;
        const status = statusOf(error);
        const detail = serverDetail(error);
        if (status === 404 && target.kind === "space") note(p.name, false, `Not uploaded: you're not a member of ${spaceName(target.spaceId)} (any more).`);
        else if (status === 415) note(p.name, false, `Not uploaded: the server refused its type${detail ? ` (${detail})` : ""}.`);
        else if (status === 413) note(p.name, false, `Not uploaded: ${detail ?? "it's over the size limit or your quota"}.`);
        else note(p.name, false, `Not uploaded: ${describe(error, "upload failed")}`);
      }
    }
  } finally {
    if (ok()) {
      batch(() => {
        uploading.set(false);
        if (outcomes.length) uploadOutcomes.set([...outcomes, ...uploadOutcomes()].slice(0, 10));
      });
    }
  }
}

// ------------------------------------------------------------------ index and delete

export async function indexAsset(asset: LibraryAsset): Promise<void> {
  if (indexStates()[asset.id]?.state === "indexing") return;
  if (!savedModelId()) {
    indexStates.set({ ...indexStates(), [asset.id]: { state: "failed", message: "Choose and save a media model first." } });
    return;
  }
  const ok = sessionGuard(asset.spaceId ?? undefined);
  const set = (s: IndexState) => indexStates.set({ ...indexStates(), [asset.id]: s });
  set({ state: "indexing", message: `Sending to ${savedModel()?.name ?? "your media model"}…` });
  try {
    const api = library();
    if (!api) return;
    const result = await api.indexAsset(asset.id);
    if (!ok() || !isJoined(asset.spaceId)) return;
    const item = result.items.find((i) => i.assetId === asset.id);
    if (item && item.status !== "indexed") {
      set({ state: "failed", message: item.error ?? `Not indexed (${item.status.replace("_", " ")}).` });
      return;
    }
    const segments = item?.indexedSegments ?? result.indexedSegments;
    const partial = item?.partial ?? result.partial;
    set({
      state: "indexed",
      message: `Indexed ${segments} ${segments === 1 ? "part" : "parts"} in your own index${partial ? " (partly: the model or a limit stopped early)" : ""}.${asset.spaceId !== null ? " Other members index it for themselves." : ""}${result.notice ? ` ${result.notice}` : ""}`,
    });
  } catch (error) {
    if (!ok()) return;
    const status = statusOf(error);
    const detail = serverDetail(error);
    if (status === 404) set({ state: "failed", message: "That file is gone or no longer yours to see." });
    else if (status === 409) set({ state: "failed", message: `Not indexed: ${detail ?? "choose a media model you may use for this file"}.` });
    else if (status === 422) set({ state: "failed", message: "Not indexed: the saved connection can't embed documents, images, audio or video." });
    else if (status === 502) set({ state: "failed", message: `Not indexed: the media model failed${detail ? ` (${detail})` : ""}. Nothing was stored.` });
    else if (status === 413 || status === 415) set({ state: "failed", message: `Not indexed: ${detail ?? "the file can't be sent to the model"}.` });
    else if (status === 503 || status === 429) set({ state: "failed", message: "File search is busy. Try again shortly." });
    else {
      const message = failure(error, "Indexing failed");
      if (message) set({ state: "failed", message });
    }
  }
}

export function askDelete(assetId: string | null) {
  confirmingDelete.set(assetId);
}

export async function confirmDelete(asset: LibraryAsset): Promise<void> {
  if (deleting()) return;
  const ok = sessionGuard();
  batch(() => {
    deleting.set(asset.id);
    fileNotice.set(null);
  });
  try {
    const api = library();
    if (!api) return;
    await api.deleteAsset(asset.id);
    if (!ok()) return;
    forgetAsset(asset.id);
    fileNotice.set(`Deleted ${asset.name} and its index.`);
  } catch (error) {
    if (!ok()) return;
    const status = statusOf(error);
    if (status === 403) fileNotice.set("Only the uploader or the Space's owner can delete this file.");
    else if (status === 404) {
      forgetAsset(asset.id);
      fileNotice.set(`${asset.name} is already gone, or no longer yours to see.`);
    } else {
      const message = failure(error, "Couldn't delete the file");
      if (message) fileNotice.set(message);
    }
  } finally {
    if (ok()) batch(() => {
      deleting.set(null);
      confirmingDelete.set(null);
    });
  }
}

function forgetAsset(id: string) {
  batch(() => {
    assets.set(assets().filter((a) => a.id !== id));
    const next = new Set(deletedIds());
    next.add(id);
    deletedIds.set(next);
    const { [id]: _gone, ...rest } = indexStates();
    indexStates.set(rest);
  });
  if (viewing()?.assetId === id) closeViewer();
}

// ------------------------------------------------------------------ search

export function setLibScope(scope: LibraryScope) {
  if (scope.kind === "space" && !isJoined(scope.spaceId)) return;
  libScope.set(scope);
}

export async function runLibrarySearch(): Promise<void> {
  const query = libQuery().trim();
  if (!query) {
    libSearchError.set({ message: "Type what you're looking for.", needsModel: false });
    return;
  }
  if (query.length > MAX_QUERY_CHARS) {
    libSearchError.set({ message: `Searches are at most ${MAX_QUERY_CHARS} characters.`, needsModel: false });
    return;
  }
  const scope = libScope();
  const kind = libKind();
  const ok = sessionGuard();
  const mine = ++seq;
  batch(() => {
    libSearching.set(true);
    libSearchError.set(null);
  });
  try {
    const api = library();
    if (!api) return;
    const answer = await api.searchLibrary(query, scope, LIBRARY_SEARCH_LIMIT, kind);
    if (!ok() || mine !== seq) return;
    batch(() => {
      lessRelevantOpen.set(false);
      libAnswer.set({ query, scope, ...answer });
    });
  } catch (error) {
    if (!ok() || mine !== seq) return;
    const status = statusOf(error);
    const detail = serverDetail(error);
    if (status === 404 && scope.kind === "space") {
      batch(() => {
        libScope.set({ kind: "all" });
        libAnswer.set(null);
        libSearchError.set({ message: "You're not a member of that Space (any more), so its files can't be searched. The scope is back to everything you can access.", needsModel: false });
      });
      return;
    }
    if (status === 409) libSearchError.set({ message: `File search needs a media model: ${detail ?? "choose one above"}.`, needsModel: true });
    else if (status === 422) libSearchError.set({ message: "The saved connection can't embed documents, images, audio or video. Choose a media model above.", needsModel: true });
    else if (status === 502) libSearchError.set({ message: `The media model couldn't be reached, so nothing was searched${detail ? `: ${detail}` : ""}.`, needsModel: false });
    else if (status === 503 || status === 429) libSearchError.set({ message: "File search is busy. Try again in a moment.", needsModel: false });
    else {
      const message = failure(error, "Search failed");
      if (message) libSearchError.set({ message, needsModel: false });
    }
  } finally {
    if (ok() && mine === seq) libSearching.set(false);
  }
}

// ------------------------------------------------------------------ viewer

/** `m:ss` (or `h:mm:ss`). */
export function clock(ms: number): string {
  const total = Math.max(0, Math.floor(ms / 1000));
  const h = Math.floor(total / 3600);
  const m = Math.floor((total % 3600) / 60);
  const s = String(total % 60).padStart(2, "0");
  return h > 0 ? `${h}:${String(m).padStart(2, "0")}:${s}` : `${m}:${s}`;
}

/** Where a match is, in words. */
export function locatorText(locator: Locator | null): string {
  if (!locator) return "From the start";
  switch (locator.kind) {
    case "page":
      return `Page ${locator.page}`;
    case "time":
      return locator.endMs !== null && locator.endMs > locator.startMs ? `${clock(locator.startMs)}–${clock(locator.endMs)}` : `At ${clock(locator.startMs)}`;
    case "image":
      return "The whole image";
    case "text":
      return "Matching section";
  }
}

/** What the Open button says for a match. */
export function openLabel(hit: LibraryHit): string {
  const l = hit.locator;
  if (l.kind === "page") return `Open at page ${l.page}`;
  if (l.kind === "time") return `Play from ${clock(l.startMs)}`;
  if (l.kind === "text") return "Open at section";
  return "Open image";
}

function kindFromMime(mime: string): AssetKind | null {
  const base = mime.split(";")[0]!.trim().toLowerCase();
  return ACCEPTED.find((a) => a.mime === base || (a.kind === "wav" && base.includes("wav")) || (a.kind === "mp3" && base === "audio/mp3") || (a.kind === "markdown" && base === "text/x-markdown"))?.kind ?? null;
}

export function openAsset(asset: LibraryAsset) {
  void openTarget({ assetId: asset.id, name: asset.name, mimeType: asset.mimeType, kind: asset.kind, spaceId: asset.spaceId, locator: null, excerpt: null, query: null, from: "library" });
}

/**
 * A file uploaded elsewhere (a message's attachment) joins the list when it
 * belongs to the scope shown, as the Library's own uploads do.
 */
export function noteUploaded(asset: LibraryAsset) {
  const scope = listScope();
  const belongs = scope.kind === "all" || (scope.kind === "personal" && asset.spaceId === null) || (scope.kind === "space" && asset.spaceId === scope.spaceId);
  if (belongs && assetsLoaded()) assets.set([asset, ...assets().filter((a) => a.id !== asset.id)]);
}

export function openLibraryHit(hit: LibraryHit) {
  const known = assets().find((a) => a.id === hit.assetId);
  void openTarget({
    assetId: hit.assetId,
    name: hit.name,
    mimeType: hit.mimeType,
    kind: known?.kind ?? kindFromMime(hit.mimeType),
    spaceId: hit.spaceId,
    locator: hit.locator,
    excerpt: hit.excerpt,
    query: libAnswer()?.query ?? null,
    from: "search",
  });
}

function dropViewBytes() {
  viewAbort?.abort();
  viewAbort = null;
  viewBytes = null;
  if (viewUrl) {
    try {
      URL.revokeObjectURL(viewUrl);
    } catch {
      /* already gone */
    }
  }
  viewUrl = null;
  removePreviewFile(viewFile);
  viewFile = null;
}

export function closeViewer() {
  viewSeq++;
  dropViewBytes();
  batch(() => {
    viewing.set(null);
    viewContent.set(null);
    playing.set(false);
    playerError.set(null);
    playerPosition.set(null);
    saveNotice.set(null);
    saving.set(false);
  });
}

/** Text documents show this many characters around the match. */
const CONTEXT_CHARS = 1200;
const WHOLE_TEXT_CHARS = 6000;

/** A UTF-8 byte offset of `bytes` as an index into its decoded string; null when it isn't on a character boundary. */
function byteToIndex(bytes: Uint8Array, offset: number): number | null {
  if (offset > bytes.length) return null;
  if (offset < bytes.length && (bytes[offset]! & 0xc0) === 0x80) return null;
  return new TextDecoder().decode(bytes.subarray(0, offset)).length;
}

function textSection(text: string, bytes: Uint8Array, locator: Locator | null, excerpt: string | null): Extract<ViewContent, { status: "text" }> {
  let start = -1;
  let end = -1;
  // Text locators count UTF-8 bytes of the file.
  if (locator?.kind === "text" && locator.start !== null) {
    const s = byteToIndex(bytes, locator.start);
    const e = locator.end !== null ? byteToIndex(bytes, Math.min(locator.end, bytes.length)) : null;
    if (s !== null && s < text.length) {
      start = s;
      end = Math.min(text.length, Math.max(start + 1, e ?? start + 1));
    }
  }
  // An excerpt the model returned is the surer anchor when offsets don't land on it.
  if (excerpt) {
    const probe = excerpt.replace(/\s+/g, " ").trim().slice(0, 80);
    const at = probe ? text.replace(/\s/g, " ").indexOf(probe) : -1;
    if (at >= 0 && (start < 0 || at < start - 5 || at > end)) {
      start = at;
      end = Math.min(text.length, at + Math.max(probe.length, excerpt.length));
    }
  }
  if (start < 0) {
    const shown = text.slice(0, WHOLE_TEXT_CHARS);
    return { status: "text", before: shown, match: "", after: "", located: false, truncatedTop: false, truncatedBottom: text.length > shown.length };
  }
  let from = Math.max(0, start - CONTEXT_CHARS);
  const para = text.lastIndexOf("\n\n", start);
  if (para >= from) from = para + 2;
  const to = Math.min(text.length, end + CONTEXT_CHARS);
  return { status: "text", before: text.slice(from, start), match: text.slice(start, end), after: text.slice(end, to), located: true, truncatedTop: from > 0, truncatedBottom: to < text.length };
}

async function openTarget(target: ViewTarget): Promise<void> {
  if (!isJoined(target.spaceId)) return;
  const mine = ++viewSeq;
  dropViewBytes();
  const abort = new AbortController();
  viewAbort = abort;
  const ok = sessionGuard(target.spaceId ?? undefined);
  batch(() => {
    viewing.set(target);
    viewContent.set({ status: "loading" });
    playing.set(false);
    playerError.set(null);
    playerPosition.set(null);
    saveNotice.set(null);
    seekToken.set(0);
  });
  const current = () => ok() && mine === viewSeq && isJoined(target.spaceId);
  try {
    const api = library();
    if (!api) return;
    // The server checks access again for this read; the token goes in a header only.
    const content = await api.assetContent(target.assetId, abort.signal);
    if (!current()) return;
    viewBytes = content.bytes;
    const kind = target.kind ?? kindFromMime(content.contentType || target.mimeType);
    if (kind === "text" || kind === "markdown") {
      let text: string;
      try {
        text = new TextDecoder("utf-8", { fatal: true }).decode(content.bytes);
      } catch {
        viewContent.set({ status: "error", message: "The file isn't valid UTF-8 text, so it isn't shown." });
        return;
      }
      viewContent.set(textSection(text, content.bytes, target.locator, target.excerpt));
      return;
    }
    const mime = kind ? ACCEPTED.find((a) => a.kind === kind)!.mime : target.mimeType;
    const media = mediaKindOf(mime);
    if (media === "image" || media === "audio" || media === "video") {
      const url = URL.createObjectURL(new Blob([content.bytes as BlobPart], { type: mime }));
      if (!current()) {
        URL.revokeObjectURL(url);
        return;
      }
      viewUrl = url;
      viewContent.set(media === "image" ? { status: "image", url } : { status: "player", url, audioOnly: media === "audio" });
      return;
    }
    viewContent.set({ status: "document", sizeBytes: content.bytes.length });
  } catch (error) {
    if (!current()) return;
    if (statusOf(error) === 404) {
      viewContent.set({ status: "error", message: "That file is gone, or you can no longer see it." });
      return;
    }
    const message = failure(error, "Couldn't open the file");
    if (message) viewContent.set({ status: "error", message });
  }
}

/**
 * The player couldn't play the `blob:` source. NSH's Linux player (GtkVideo)
 * plays local files only, so the bytes already in memory go to a private
 * preview file once, which is deleted when the viewer closes, the session
 * ends or the Space is lost. Any other failure is shown as it is.
 */
export async function playerFailed(message: string): Promise<void> {
  const c = viewContent();
  const target = viewing();
  const bytes = viewBytes;
  const dir = previewDir();
  if (!c || c.status !== "player" || !target || !bytes || !dir || !c.url.startsWith("blob:") || viewFile) {
    playerError.set(message);
    return;
  }
  const mine = viewSeq;
  const ext = ACCEPTED.find((a) => a.kind === target.kind)?.extensions[0] ?? "bin";
  const path = `${dir}/${previewPrefix()}${crypto.randomUUID()}.${ext}`;
  // No byte is written until the directory is confirmed private.
  const refusal = await ensurePrivateDir(dir);
  if (refusal !== null) {
    if (mine === viewSeq) playerError.set(`${message} It wasn't copied to a private preview file either: ${refusal}.`);
    return;
  }
  try {
    await writeFile(path, bytes);
  } catch (error) {
    if (mine === viewSeq) playerError.set(`${message} (a private preview file couldn't be written: ${String((error as Error)?.message ?? error)})`);
    removePreviewFile(path);
    return;
  }
  if (mine !== viewSeq || !isJoined(target.spaceId)) {
    removePreviewFile(path);
    return;
  }
  viewFile = path;
  viewContent.set({ ...c, url: `file://${path}` });
}

/** Jumps the player to the match and plays. */
export function playFromMatch() {
  batch(() => {
    seekToken.set(seekToken() + 1);
    playing.set(true);
  });
}

function safeFileName(name: string): string {
  const base = name.split(/[\\/]/).pop() ?? "file";
  const cleaned = base.replace(/[\u0000-\u001f<>:"|?*]/g, "_").replace(/^\.+/, "").trim();
  return cleaned || "file";
}

/** The bytes the viewer holds, written to the user's Downloads folder on their explicit request. */
export async function saveCopy(): Promise<void> {
  const target = viewing();
  const bytes = viewBytes;
  if (!target || !bytes || saving()) return;
  const mine = viewSeq;
  saving.set(true);
  saveNotice.set(null);
  try {
    const dir = `${homedir()}/Downloads`;
    await mkdir(dir, { recursive: true });
    const name = safeFileName(target.name);
    const dot = name.lastIndexOf(".");
    const stem = dot > 0 ? name.slice(0, dot) : name;
    const ext = dot > 0 ? name.slice(dot) : "";
    let path = `${dir}/${name}`;
    for (let n = 1; n < 100; n++) {
      try {
        const { stat } = await import("fs/promises");
        await stat(path);
        path = `${dir}/${stem} (${n})${ext}`;
      } catch {
        break;
      }
    }
    await writeFile(path, bytes);
    if (mine !== viewSeq) return;
    saveNotice.set({ ok: true, message: `Saved a copy to ${path}.` });
  } catch (error) {
    if (mine !== viewSeq) return;
    saveNotice.set({ ok: false, message: `Couldn't save a copy: ${String((error as Error)?.message ?? error)}` });
  } finally {
    if (mine === viewSeq) saving.set(false);
  }
}

// ------------------------------------------------------------------ thumbnails

/** Most image results that get a thumbnail at once. */
export const MAX_THUMBNAILS = 6;
/**
 * Small previews of image results: `blob:` URLs of bytes read through the
 * same authenticated request the viewer uses (the server checks access again
 * for each), only for image results drawn on Search's Documents & media
 * results right now: not folded under "Less relevant", not behind an open
 * viewer, not on another page or mode. Nothing goes to a model. A thumbnail
 * that stops being drawn is revoked at once; a read that answers after its
 * result stopped being drawn, the session changed or the Space was lost is
 * thrown away.
 */
export const thumbnails = signal<Record<string, string>>({});
let thumbSeq = 0;
let thumbAbort: AbortController | null = null;
let thumbKey = "";

function dropThumbnails(keep?: (assetId: string) => boolean) {
  const next: Record<string, string> = {};
  for (const [id, url] of Object.entries(thumbnails())) {
    if (keep?.(id)) next[id] = url;
    else {
      try {
        URL.revokeObjectURL(url);
      } catch {
        /* already gone */
      }
    }
  }
  thumbnails.set(next);
}

function stopThumbnails() {
  thumbSeq++;
  thumbAbort?.abort();
  thumbAbort = null;
  thumbKey = "";
}

/** The image results drawn with a thumbnail now, at most MAX_THUMBNAILS. */
const wantedThumbnails = computed(() => {
  const onResults = mainPlace() === "search" && searchSurface() === "library" && !viewing() && libAnswer() !== null;
  if (!onResults) return [] as string[];
  return [...new Set(shownHits().filter((h) => h.kind === "image").map((h) => h.assetId))].slice(0, MAX_THUMBNAILS);
});

async function syncThumbnails(ids: readonly string[]): Promise<void> {
  const key = ids.join("|");
  if (key === thumbKey) return;
  stopThumbnails();
  thumbKey = key;
  const mine = thumbSeq;
  const wanted = new Set(ids);
  dropThumbnails((id) => wanted.has(id));
  const missing = ids.filter((id) => !thumbnails()[id]);
  if (missing.length === 0) return;
  const api = library();
  if (!api) return;
  const ok = sessionGuard();
  const abort = new AbortController();
  thumbAbort = abort;
  const stillWanted = (id: string) => mine === thumbSeq && ok() && untracked(wantedThumbnails).includes(id);
  for (const id of missing) {
    const hit = untracked(shownHits).find((h) => h.assetId === id);
    if (!hit || !stillWanted(id)) return;
    try {
      const content = await api.assetContent(id, abort.signal);
      // Kept only for the same session and the same drawn results, in a Space the user is still in.
      if (mine !== thumbSeq || !ok()) return;
      if (!stillWanted(id) || !isJoined(hit.spaceId) || deletedIds().has(id)) continue;
      const mime = (content.contentType || hit.mimeType).split(";")[0]!.trim();
      if (mediaKindOf(mime) !== "image") continue;
      const url = URL.createObjectURL(new Blob([content.bytes as BlobPart], { type: mime }));
      thumbnails.set({ ...thumbnails(), [id]: url });
    } catch (error) {
      if (mine !== thumbSeq || isAborted(error)) return;
      if (error instanceof ApiError && error.kind === "auth") {
        disconnect(error.message);
        return;
      }
      // A file that can't be read just keeps its icon.
    }
  }
  if (mine === thumbSeq) thumbAbort = null;
}

effect(() => {
  const ids = wantedThumbnails();
  untracked(() => void syncThumbnails(ids));
});

// ------------------------------------------------------------------ lifecycle

// The viewer belongs to the page it opened on: leaving that page (or Search's
// Documents & media mode) closes it and drops its bytes.
effect(() => {
  const here = mainPlace();
  const surface = searchSurface();
  untracked(() => {
    const v = viewing();
    if (!v) return;
    const stays = v.from === "library" ? here === "library" : here === "search" && surface === "library";
    if (!stays) closeViewer();
  });
});

// A Space the user is no longer in: its files, results and any open preview go at once.
effect(() => {
  const joined = new Set(spaces().map((s) => s.id));
  untracked(() => {
    const lost = (sid: string | null) => sid !== null && !joined.has(sid);
    batch(() => {
      if (assets().some((a) => lost(a.spaceId))) assets.set(assets().filter((a) => !lost(a.spaceId)));
      const ls = listScope();
      if (ls.kind === "space" && lost(ls.spaceId)) listScope.set({ kind: "all" });
      const ss = libScope();
      if (ss.kind === "space" && lost(ss.spaceId)) libScope.set({ kind: "all" });
      const ut = uploadTarget();
      if (ut.kind === "space" && lost(ut.spaceId)) uploadTarget.set({ kind: "personal" });
      const answer = libAnswer();
      if (answer && answer.results.some((h) => lost(h.spaceId))) libAnswer.set({ ...answer, results: answer.results.filter((h) => !lost(h.spaceId)) });
    });
    const v = viewing();
    if (v && lost(v.spaceId)) closeViewer();
  });
});

onSessionEnd(() => {
  seq++;
  stopThumbnails();
  closeViewer();
  dropThumbnails();
  lessRelevantOpen.set(false);
  previewSession++;
  purgePreviews();
  batch(() => {
    searchSurface.set("conversations");
    listScope.set({ kind: "all" });
    assets.set([]);
    assetsLoaded.set(false);
    loadingAssets.set(false);
    assetsError.set(null);
    nextCursor.set(null);
    maxFileBytes.set(MAX_UPLOAD_BYTES);
    uploadTarget.set({ kind: "personal" });
    uploading.set(false);
    uploadOutcomes.set([]);
    indexStates.set({});
    confirmingDelete.set(null);
    deleting.set(null);
    fileNotice.set(null);
    mediaModels.set([]);
    savedModelId.set(null);
    suggestedModelId.set(null);
    modelDraft.set(null);
    modelsLoaded.set(false);
    modelError.set(null);
    savingModel.set(false);
    libQuery.set("");
    libScope.set({ kind: "all" });
    libKind.set(null);
    libAnswer.set(null);
    libSearching.set(false);
    libSearchError.set(null);
    deletedIds.set(new Set());
  });
});
