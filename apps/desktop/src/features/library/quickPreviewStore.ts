// The bytes behind quick-search file previews, shared by every
// QuickAssetPreview on screen and bounded:
//
// - Bytes come only through the authenticated content read (the bearer in a
//   header, never in a URL or a media widget's source), and only for a row
//   that is mounted. Text and WAV read a prefix (`Range: bytes=0-N`); images,
//   videos and DOCX read the whole file, up to a per-kind cap; an MP3 reads
//   its first 64 KB for its ID3 title, artist and album (never a waveform:
//   nothing here decodes MP3 audio). PDF isn't read at all.
// - At most QUICK_CONCURRENCY reads run at once, at most QUICK_MAX_LIVE
//   previews hold bytes, and at most QUICK_MAX_HELD_BYTES are held in all.
//   Past a budget a row says so instead of reading.
// - Rows that show the same file share one read. The last row to go aborts
//   the read, revokes the `blob:` URL and deletes any private preview file.
//   Sign-out, and losing the file's Space, do the same for every row at once,
//   and a read that answers after them is thrown away.
// - Private preview files (only for a video the platform's player can't play
//   from `blob:`, as on Linux) live in their own directory, apart from the
//   viewer's, and are removed on release, at sign-out and (left over from an
//   earlier run) at start. Their names carry this run's and session's token,
//   so a cleanup only ever removes files of an earlier run or session, never
//   one a preview just wrote. NSH's files API can't set permissions: they get
//   the process's defaults inside the app's data directory.
// Nothing is sent to a model, and no disk is scanned.

import { batch, effect, signal, untracked } from "nsh";
import type { Signal } from "nsh";
import { mkdir, readdir, stat, unlink, writeFile } from "fs/promises";
import proc from "process";
import { ApiError } from "../../api/client";
import { ACCEPTED } from "../../api/library";
import { disconnect, isAborted, onSessionEnd, sessionGuard, spaces } from "../../store";
import { formatSize, library } from "./libraryState";
import type { LibraryAsset } from "./libraryState";
import { docxSnippet, ID3_READ_BYTES, id3Tags, textSnippet, wavWaveform } from "./quickPreviewData";
import type { AudioTags, Waveform } from "./quickPreviewData";

export const QUICK_CONCURRENCY = 2;
export const QUICK_MAX_LIVE = 12;
export const QUICK_MAX_HELD_BYTES = 16 * 1024 * 1024;
/** What a preview reads of each kind at most. */
export const QUICK_TEXT_BYTES = 8 * 1024;
export const QUICK_WAV_BYTES = 1024 * 1024;
export const QUICK_IMAGE_MAX = 4 * 1024 * 1024;
export const QUICK_VIDEO_MAX = 4 * 1024 * 1024;
export const QUICK_DOCX_MAX = 2 * 1024 * 1024;

export type QuickPreview =
  | { status: "loading" }
  | { status: "text"; text: string }
  | { status: "image"; url: string }
  | { status: "video"; url: string }
  | { status: "waveform"; waveform: Waveform }
  | { status: "tags"; tags: AudioTags }
  | { status: "none"; reason: string };

type Plan = { kind: "text" | "markdown" | "wav" | "docx" | "mp3"; part: number } | { kind: "image" | "video"; whole: number } | { kind: "none"; reason: string };

type Entry = {
  asset: LibraryAsset;
  refs: number;
  state: Signal<QuickPreview>;
  abort: AbortController;
  epoch: number;
  /** Bytes reserved against QUICK_MAX_HELD_BYTES while this entry holds or reads them. */
  held: number;
  url: string | null;
  file: string | null;
  /** A video's bytes, kept only until its player shows it, to write a private file if `blob:` fails. */
  videoBytes: Uint8Array | null;
  /** It reads (or read) bytes: counted against QUICK_MAX_LIVE while it lasts. */
  reads: boolean;
};

const entries = new Map<string, Entry>();
const queue: Entry[] = [];
let epoch = 0;
let active = 0;
let heldBytes = 0;

const label = (kind: string) => ACCEPTED.find((a) => a.kind === kind)?.label ?? kind;
const joined = (spaceId: string | null) => spaceId === null || spaces().some((s) => s.id === spaceId);

/** What to read for this file, decided from its metadata alone. */
export function planFor(asset: LibraryAsset): Plan {
  const size = asset.sizeBytes;
  switch (asset.kind) {
    case "text":
    case "markdown":
      return { kind: asset.kind, part: QUICK_TEXT_BYTES };
    case "wav":
      return { kind: "wav", part: QUICK_WAV_BYTES };
    case "docx":
      return size <= QUICK_DOCX_MAX ? { kind: "docx", part: size } : { kind: "none", reason: `Word file of ${formatSize(size)}: too large for a quick preview.` };
    case "png":
    case "jpeg":
      return size <= QUICK_IMAGE_MAX ? { kind: "image", whole: size } : { kind: "none", reason: `${label(asset.kind)} image of ${formatSize(size)}: too large for a quick preview.` };
    case "mp4":
    case "webm":
      return size <= QUICK_VIDEO_MAX ? { kind: "video", whole: size } : { kind: "none", reason: `${label(asset.kind)} video of ${formatSize(size)}: too large for a quick preview.` };
    case "pdf":
      return { kind: "none", reason: "PDF: no preview here. Open it to read." };
    case "mp3":
      return { kind: "mp3", part: ID3_READ_BYTES };
    default:
      return { kind: "none", reason: "No preview for this type." };
  }
}

const budget = (p: Plan) => (p.kind === "none" ? 0 : "part" in p ? p.part : p.whole);

// ------------------------------------------------------------------ files

const previewDir = () => (proc.dataDir ? `${proc.dataDir}/quick-preview` : null);
/** This run's token: files of an earlier run (a crash, a kill) are the only ones a start-up cleanup removes. */
const RUN = crypto.randomUUID().slice(0, 8);
/** The prefix of this run's and this session's files. */
const filePrefix = () => `${RUN}-${epoch}-`;

function removeFile(path: string | null) {
  if (path) void unlink(path).catch(() => {});
}

/** Removes every preview file but this session's own (files of an earlier run, or of a session that ended). */
function purgeFiles() {
  const dir = previewDir();
  if (!dir) return;
  const keep = filePrefix();
  void readdir(dir)
    .then((names) => Promise.all(names.filter((n) => !n.startsWith(keep)).map((n) => unlink(`${dir}/${n}`).catch(() => {}))))
    .catch(() => {});
}
purgeFiles();

// ------------------------------------------------------------------ lifecycle

/** Drops everything an entry holds: the read, the URL, the private file, the bytes. */
function dropBytes(e: Entry) {
  e.abort.abort();
  if (e.url) {
    try {
      URL.revokeObjectURL(e.url);
    } catch {
      /* already gone */
    }
  }
  e.url = null;
  removeFile(e.file);
  e.file = null;
  e.videoBytes = null;
  heldBytes -= e.held;
  e.held = 0;
  const at = queue.indexOf(e);
  if (at >= 0) queue.splice(at, 1);
}

const current = (e: Entry) => e.epoch === epoch && entries.get(e.asset.id) === e && e.refs > 0 && joined(e.asset.spaceId);

/**
 * One more row shows `asset`: its preview, shared with any other row of the
 * same file. Call `releasePreview` when the row goes.
 */
export function acquirePreview(asset: LibraryAsset): () => QuickPreview {
  const found = entries.get(asset.id);
  if (found && found.epoch === epoch) {
    found.refs++;
    return found.state;
  }
  const e: Entry = { asset, refs: 1, state: signal<QuickPreview>({ status: "loading" } as QuickPreview), abort: new AbortController(), epoch, held: 0, url: null, file: null, videoBytes: null, reads: false };
  entries.set(asset.id, e);
  const plan = planFor(asset);
  if (!joined(asset.spaceId)) e.state.set({ status: "none", reason: "You can't open this file now." });
  else if (plan.kind === "none") e.state.set({ status: "none", reason: plan.reason });
  else if ([...entries.values()].filter((x) => x.reads).length >= QUICK_MAX_LIVE) e.state.set({ status: "none", reason: "Preview skipped: too many files at once." });
  else if (heldBytes + budget(plan) > QUICK_MAX_HELD_BYTES) e.state.set({ status: "none", reason: "Preview skipped to keep memory use low." });
  else {
    e.reads = true;
    e.held = budget(plan);
    heldBytes += e.held;
    queue.push(e);
    pump();
  }
  return e.state;
}

/** A row that showed `assetId` is gone; the last one frees everything. */
export function releasePreview(assetId: string) {
  const e = entries.get(assetId);
  if (!e) return;
  e.refs--;
  if (e.refs > 0) return;
  dropBytes(e);
  entries.delete(assetId);
}

function pump() {
  while (active < QUICK_CONCURRENCY && queue.length > 0) {
    const e = queue.shift()!;
    if (!current(e)) continue;
    active++;
    void load(e).finally(() => {
      active--;
      pump();
    });
  }
}

async function load(e: Entry): Promise<void> {
  const ok = sessionGuard(e.asset.spaceId ?? undefined);
  const plan = planFor(e.asset);
  const api = library() as (ReturnType<typeof library> & { assetContentPart?: unknown }) | null;
  if (!api || plan.kind === "none") return;
  const live = () => ok() && current(e);
  try {
    let bytes: Uint8Array;
    if ("part" in plan && plan.kind !== "docx" && typeof api.assetContentPart === "function") {
      bytes = (await api.assetContentPart(e.asset.id, plan.part, e.abort.signal)).bytes;
    } else {
      bytes = (await api.assetContent(e.asset.id, e.abort.signal)).bytes;
      if ("part" in plan && plan.kind !== "docx") bytes = bytes.subarray(0, plan.part);
    }
    if (!live()) return;
    const set = (p: QuickPreview) => e.state.set(p);
    switch (plan.kind) {
      case "text":
      case "markdown": {
        const t = textSnippet(bytes, plan.kind === "markdown");
        set(t ? { status: "text", text: t } : { status: "none", reason: "No readable text at the start of this file." });
        break;
      }
      case "docx": {
        const t = docxSnippet(bytes);
        set(t ? { status: "text", text: t } : { status: "none", reason: "Word file: no readable text found." });
        break;
      }
      case "mp3": {
        const t = id3Tags(bytes);
        set(t ? { status: "tags", tags: t } : { status: "none", reason: "MP3 audio without a title or artist tag. Open it to play." });
        break;
      }
      case "wav": {
        const w = wavWaveform(bytes);
        set(w ? { status: "waveform", waveform: w } : { status: "none", reason: "WAV audio in a format this can't read." });
        break;
      }
      case "image":
      case "video": {
        const mime = ACCEPTED.find((a) => a.kind === e.asset.kind)?.mime ?? e.asset.mimeType;
        e.url = URL.createObjectURL(new Blob([bytes as BlobPart], { type: mime }));
        if (plan.kind === "video") e.videoBytes = bytes;
        set({ status: plan.kind, url: e.url });
        break;
      }
    }
    // Text and waveforms keep nothing but what they show.
    if (plan.kind !== "image" && plan.kind !== "video") {
      heldBytes -= e.held;
      e.held = 0;
    }
  } catch (error) {
    if (!live() || isAborted(error)) return;
    if (error instanceof ApiError && error.kind === "auth") {
      disconnect(error.message);
      return;
    }
    const status = error instanceof ApiError ? error.status : null;
    e.state.set({ status: "none", reason: status === 404 ? "This file is gone, or no longer yours to see." : "The preview couldn't be loaded." });
    dropBytes(e);
  }
}

/** The video's player showed it: its spare bytes go. */
export function videoShown(assetId: string) {
  const e = entries.get(assetId);
  if (e) e.videoBytes = null;
}

/**
 * The video's player couldn't play the `blob:` source. Linux's player plays
 * local files only, so the bytes already in hand go once to a private file,
 * deleted with the preview. Any other failure is said as it is.
 */
export async function videoFailed(assetId: string): Promise<void> {
  const e = entries.get(assetId);
  const s = e ? untracked(e.state) : null;
  const dir = previewDir();
  if (!e || !s || s.status !== "video" || !s.url.startsWith("blob:") || !e.videoBytes || !dir || e.file) {
    if (e && current(e)) {
      dropBytes(e);
      e.state.set({ status: "none", reason: "This video can't be shown here. Open it to play." });
    }
    return;
  }
  const bytes = e.videoBytes;
  e.videoBytes = null;
  const ext = ACCEPTED.find((a) => a.kind === e.asset.kind)?.extensions[0] ?? "bin";
  const path = `${dir}/${filePrefix()}${crypto.randomUUID()}.${ext}`;
  try {
    await mkdir(dir, { recursive: true });
    // Never write into a directory others could write to (a planted link or file).
    if (((await stat(dir)).mode & 0o022) !== 0) throw new Error("the preview directory is writable by others");
    await writeFile(path, bytes);
  } catch {
    removeFile(path);
    if (current(e)) {
      dropBytes(e);
      e.state.set({ status: "none", reason: "This video can't be shown here. Open it to play." });
    }
    return;
  }
  if (!current(e)) {
    removeFile(path);
    return;
  }
  e.file = path;
  e.state.set({ status: "video", url: `file://${path}` });
}

// A Space the user is no longer in: its previews go at once, bytes and all.
effect(() => {
  spaces();
  untracked(() => {
    batch(() => {
      for (const e of entries.values()) {
        if (e.epoch === epoch && !joined(e.asset.spaceId) && untracked(e.state).status !== "none") {
          dropBytes(e);
          e.state.set({ status: "none", reason: "You're no longer a member of this file's Space." });
        }
      }
    });
  });
});

onSessionEnd(() => {
  epoch++;
  batch(() => {
    for (const e of entries.values()) {
      dropBytes(e);
      e.state.set({ status: "none", reason: "Signed out." });
    }
  });
  entries.clear();
  queue.length = 0;
  heldBytes = 0;
  purgeFiles();
});

/** For tests: how many previews hold or read bytes, and how many bytes. */
export const quickPreviewBudget = () => ({ rows: [...entries.values()].filter((e) => e.reads).length, heldBytes, active, queued: queue.length });
