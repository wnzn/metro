// Attaching files to a message: the composer's picker stages files
// (./attachmentDraft), and Send uploads them to exactly the destination's
// library before posting the message with their ids:
//
// - The personal Chat: the sender's personal library (only they see it).
// - A Space's room or thread: that Space's shared library (its current
//   members see it).
//
// Uploading is the Library's own authenticated, validated upload; it never
// indexes a file or sends its bytes to a model. A file Send uploaded keeps
// its id, so retrying after a failed post never uploads it twice; a file
// removed or a pick cancelled before Send was never uploaded. Staged files
// are dropped at sign-out.

import { batch, Picker as SystemPicker, signal } from "nsh";
import { ApiError, MAX_MESSAGE_ATTACHMENTS } from "../../api/client";
import type { AttachedFile } from "../../api/client";
import { ACCEPTED, acceptedOf } from "../../api/library";
import type { UploadTarget } from "../../api/library";
import { activeSpace, composerScope, describe, disconnect, isAborted, onSessionEnd, onTransientClear, openPlace, registerAttachments, sessionGuard, spaces } from "../../store";
import type { SendDestination } from "../../store";
import { formatSize, library, maxFileBytes, noteUploaded, openAsset } from "../library/libraryState";
import { clearAllStaged, dropSent, markUploaded, newStagedKey, stage, stagedFor } from "./attachmentDraft";
import type { StagedFile } from "./attachmentDraft";

export { MAX_MESSAGE_ATTACHMENTS };

const FILTER_EXTENSIONS = ACCEPTED.flatMap((a) => [...a.extensions]);
const TYPES = "TXT, MD, PDF, DOCX, PNG, JPEG, WAV, MP3, MP4 or WebM";

/** What the last pick left out, for the place it was picked in. */
export const pickNote = signal<{ scope: string; text: string } | null>(null);
export const picking = signal(false);
let pickEpoch = 0;
function cancelPick() {
  pickEpoch++;
  picking.set(false);
}
onTransientClear(cancelPick);

/** Where a place's staged files will go and who sees them, in a few words; the model gets the text only. */
export function destinationCue(scope: string): string {
  if (scope === "chat") return "Saved to your Library · only you · the agent gets text only";
  const name = activeSpace()?.name ?? "this Space";
  return `Saved to ${name}'s Library · its members see them · the agent gets text only`;
}

/**
 * Opens a message's file in the Library's viewer. `placeSpaceId` is the
 * message's own place (null: the personal Chat), the only thing that says
 * which library holds the file. Only a personal file or a file of a Space
 * the user is a member of now; the server checks access again as the viewer
 * reads the bytes (a header credential, never a URL). Resolves to what to
 * tell the user when it can't open, else null.
 */
export function openMessageFile(file: AttachedFile, placeSpaceId: string | null): string | null {
  if (placeSpaceId !== null && !spaces().some((s) => s.id === placeSpaceId)) return `You're no longer a member of the Space that holds “${file.name}”.`;
  const base = file.mimeType.split(";")[0]!.trim().toLowerCase();
  const format = ACCEPTED.find((a) => a.kind === file.kind) ?? acceptedOf(file.name) ?? ACCEPTED.find((a) => a.mime === base) ?? null;
  if (!format) return `“${file.name}” can't be opened here.`;
  openPlace("library");
  openAsset({ id: file.assetId, name: file.name, mimeType: file.mimeType, kind: format.kind, sizeBytes: file.sizeBytes, spaceId: placeSpaceId, uploaderId: "", sha256: "", createdAt: 0 });
  return null;
}

/** Shows the system's file picker and stages what it gives for the active place. Uploads nothing. */
export async function pickAttachments(pick: (options: Parameters<typeof SystemPicker.pickFiles>[0]) => Promise<readonly Picked[]> = (options) => SystemPicker.pickFiles(options)): Promise<void> {
  const scope = composerScope();
  if (!scope || picking()) return;
  const room = MAX_MESSAGE_ATTACHMENTS - stagedFor(scope).length;
  if (room <= 0) {
    pickNote.set({ scope, text: `A message carries at most ${MAX_MESSAGE_ATTACHMENTS} files.` });
    return;
  }
  const limit = maxFileBytes();
  const ticket = ++pickEpoch;
  const ok = sessionGuard(scope === "chat" ? undefined : activeSpace()?.id);
  const current = () => ticket === pickEpoch && ok() && composerScope() === scope;
  batch(() => {
    picking.set(true);
    pickNote.set(null);
  });
  try {
    let picked;
    try {
      picked = await pick({ multiple: true, limit: room, filters: [{ name: "Documents & media", extensions: FILTER_EXTENSIONS }], maxSize: limit });
    } catch (error) {
      if (current()) pickNote.set({ scope, text: `The file picker couldn't open: ${String((error as Error)?.message ?? error)}` });
      return;
    }
    if (!current()) return;
    const { files, problems } = checkPicked(picked, stagedFor(scope), limit);
    stage(scope, files);
    if (problems.length) pickNote.set({ scope, text: problems.join(" ") });
  } finally {
    if (ticket === pickEpoch) picking.set(false);
  }
}

type Picked = { name: string; size: number; status: "read" | "tooLarge"; file: Blob | null };

/** Which picked files can be staged, and why the others can't. Pure; exported for tests. */
export function checkPicked(picked: readonly Picked[], already: readonly StagedFile[], limit: number): { files: StagedFile[]; problems: string[] } {
  const files: StagedFile[] = [];
  const problems: string[] = [];
  for (const p of picked) {
    const format = acceptedOf(p.name);
    if (!format) problems.push(`“${p.name}” isn't attached: use ${TYPES}.`);
    else if (p.status === "tooLarge" || !p.file || p.size > limit) problems.push(`“${p.name}” isn't attached: it's ${formatSize(p.size)}; the limit is ${formatSize(limit)}.`);
    else if (p.size === 0) problems.push(`“${p.name}” isn't attached: it's empty.`);
    else if ([...already, ...files].some((f) => f.name === p.name && f.size === p.size)) continue; // the same file twice
    else if (already.length + files.length >= MAX_MESSAGE_ATTACHMENTS) problems.push(`“${p.name}” isn't attached: a message carries at most ${MAX_MESSAGE_ATTACHMENTS} files.`);
    else files.push({ key: newStagedKey(), name: p.name, mime: format.mime, kind: format.kind, size: p.size, file: p.file, assetId: null });
  }
  return { files, problems };
}

const targetOf = (to: SendDestination): UploadTarget => (to.kind === "chat" ? { kind: "personal" } : { kind: "space", spaceId: to.spaceId });
const placeOf = (to: SendDestination) => (to.kind === "chat" ? "your personal Library" : `${spaces().find((s) => s.id === to.spaceId)?.name ?? "the Space"}'s Library`);

/** Why one upload failed, in a few words. */
function uploadProblem(error: unknown): string {
  const status = error instanceof ApiError ? error.status : null;
  if (status === 413) return "it's over the size limit or the Library's quota";
  if (status === 415) return "the server refused its type";
  if (status === 404 || status === 403) return "you can't add files there (any more)";
  return describe(error, "the upload failed");
}

registerAttachments({
  pending: (scope) => stagedFor(scope).length > 0,

  async upload(scope, to, ok) {
    const list = stagedFor(scope);
    if (list.length > MAX_MESSAGE_ATTACHMENTS) return { error: `A message carries at most ${MAX_MESSAGE_ATTACHMENTS} files; remove some.` };
    if (to.kind === "room" && !spaces().some((s) => s.id === to.spaceId)) return { error: "You're no longer a member of this Space; nothing was sent." };
    const api = library();
    if (!api) return { error: "" };
    const target = targetOf(to);
    const ids: string[] = [];
    for (const f of list) {
      if (!ok()) return { error: "" };
      if (f.assetId) {
        ids.push(f.assetId);
        continue;
      }
      try {
        const asset = await api.uploadAsset(target, f.name, f.mime, f.file);
        if (!ok()) return { error: "" };
        markUploaded(scope, f.key, asset.id);
        ids.push(asset.id);
        noteUploaded(asset);
      } catch (error) {
        if (!ok() || isAborted(error)) return { error: "" };
        if (error instanceof ApiError && error.kind === "auth") {
          disconnect(error.message);
          return { error: "" };
        }
        const done = ids.length + list.slice(ids.length).filter((x) => x.assetId).length;
        const saved = done === 0 ? "No file was uploaded" : `${done} of ${list.length} files ${done === 1 ? "is" : "are"} in ${placeOf(to)}`;
        return { error: `“${f.name}” wasn't uploaded: ${uploadProblem(error)}. ${saved}; nothing was sent. Send again to retry (files already uploaded aren't uploaded twice), or remove it.` };
      }
    }
    return { ids, keys: list.map((f) => f.key) };
  },

  sent(scope, keys) {
    dropSent(scope, keys);
    if (!stagedFor(scope).length && pickNote()?.scope === scope) pickNote.set(null);
  },
});

onSessionEnd(() => {
  clearAllStaged();
  batch(() => {
    pickNote.set(null);
    cancelPick();
  });
});
