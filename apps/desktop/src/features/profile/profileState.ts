// Profiles in this session: the signed-in user's own (edited in Settings ›
// Profile) and the people whose names and pictures the transcripts show.
//
// - Everything here belongs to one session. `resetProfiles` runs when the
//   session ends (sign-out, 401, a new connection): pending requests are
//   ignored, every picture's `blob:` URL is revoked, nothing is kept.
// - Others' profiles come only from the server's permission-checked
//   endpoints. A 404 means "not yours to see": initials, no error. Seen
//   profiles are checked again after a while, and at once when the user's
//   Spaces or members change, so a revoked share drops the picture.
// - Pictures are bytes fetched with the user's credential and shown through
//   `blob:` URLs; never a remote URL, never stored on disk.
// - A new picture comes only from the system's file picker, limited to PNG
//   and JPEG, checked by its bytes before it is sent.

import { batch, effect, Picker as SystemPicker, signal, untracked } from "nsh";
import { ApiError } from "../../api/client";
import { AVATAR_EXTENSIONS, MAX_AVATAR_BYTES, MAX_BIO, MAX_DISPLAY_NAME, sniffAvatar } from "../../api/profile";
import type { AvatarMime, Profile } from "../../api/profile";
import { activeSpaceId, apiClient, describe, disconnect, isAborted, me, members, mentionProfileEvidence, onSessionEnd, sessionGuard, spaces } from "../../store";

/** How long a seen profile is trusted before it is checked again. */
export const RECHECK_MS = 60_000;

export type PersonState = {
  /** "loading" before the first answer; "hidden": the server doesn't show it to this user (404). */
  status: "loading" | "ready" | "hidden" | "error";
  profile: Profile | null;
  /** A `blob:` URL of the picture, or null (none, not loaded, or not allowed). */
  avatarUrl: string | null;
};

const people = signal<Record<string, PersonState>>({});

// Internal bookkeeping, not rendered.
const checkedAt = new Map<string, number>();
type ProfileRead = { scope?: object; work: Promise<void> };
const inflight = new Map<string, ProfileRead>();
/** The picture version each `blob:` URL shows. */
const shownVersion = new Map<string, string>();
/** Bumped by `resetProfiles`: answers that arrive later are dropped. */
let generation = 0;
let recheckTimer: ReturnType<typeof setInterval> | undefined;

/** The person's state; undefined until something asked for it (`ensurePerson`). */
export function personState(userId: string): PersonState | undefined {
  return people()[userId];
}

/** Their display name, when the server shows a non-empty one. */
export function profileName(userId: string | null | undefined): string | null {
  if (!userId) return null;
  const name = people()[userId]?.profile?.displayName;
  return name ? name : null;
}

function setPerson(userId: string, next: PersonState) {
  const current = untracked(people)[userId];
  if (current?.avatarUrl && current.avatarUrl !== next.avatarUrl) {
    URL.revokeObjectURL(current.avatarUrl);
    shownVersion.delete(userId);
  }
  people.set({ ...untracked(people), [userId]: next });
}

/** Loads someone's profile (and picture) once per `RECHECK_MS`; later calls are free. */
export function ensurePerson(userId: string, force = false): Promise<void> {
  return ensureProfile(userId, force, false);
}

export function ensureMentionPerson(userId: string): Promise<void> {
  return ensureProfile(userId, false, true);
}

function ensureProfile(userId: string, force: boolean, forMention: boolean): Promise<void> {
  if (!userId) return Promise.resolve();
  const evidence = untracked(() => mentionProfileEvidence(userId));
  const needsScopedRead = forMention && evidence && !evidence.verified;
  const running = inflight.get(userId);
  if (running && (!needsScopedRead || running.scope === evidence.scope)) return running.work;
  const last = checkedAt.get(userId);
  if (!force && !needsScopedRead && last !== undefined && Date.now() - last < RECHECK_MS) return Promise.resolve();
  // Re-entry may need its own read even while a different visit's request
  // is in flight. Only the current request updates the general profile cache.
  const entry: ProfileRead = {
    scope: evidence?.scope,
    work: Promise.resolve().then(() => loadPerson(userId, evidence, () => inflight.get(userId) === entry)).finally(() => {
      if (inflight.get(userId) === entry) inflight.delete(userId);
    }),
  };
  inflight.set(userId, entry);
  startRechecks();
  return entry.work;
}

async function loadPerson(userId: string, evidence: ReturnType<typeof mentionProfileEvidence>, latest: () => boolean): Promise<void> {
  const api = apiClient();
  if (!api || !latest()) return;
  const gen = generation;
  const ok = sessionGuard();
  const live = () => ok() && gen === generation;
  if (!untracked(people)[userId]) setPerson(userId, { status: "loading", profile: null, avatarUrl: null });
  const complete = evidence?.start();
  let profile: Profile | null;
  try {
    profile = await api.profileOf(userId);
    if (profile && profile.id !== userId) throw new Error("Profile identity did not match the request");
  } catch (error) {
    if (!live() || isAborted(error)) return;
    if (error instanceof ApiError && error.kind === "auth") return disconnect(error.message);
    if (error instanceof ApiError && error.kind === "forbidden") complete?.(null);
    if (!latest()) return;
    // Not allowed any more (403) is the same as hidden; anything else keeps what is shown.
    if (error instanceof ApiError && error.kind === "forbidden") {
      checkedAt.set(userId, Date.now());
      setPerson(userId, { status: "hidden", profile: null, avatarUrl: null });
    } else {
      const current = untracked(people)[userId];
      if (current?.status === "loading") setPerson(userId, { ...current, status: "error" });
    }
    return;
  }
  if (!live()) return;
  complete?.(profile?.displayName);
  if (!latest()) return;
  checkedAt.set(userId, Date.now());
  if (!profile) {
    setPerson(userId, { status: "hidden", profile: null, avatarUrl: null });
    return;
  }
  const current = untracked(people)[userId];
  const keepUrl = profile.avatarVersion && current?.avatarUrl && shownVersion.get(userId) === profile.avatarVersion ? current.avatarUrl : null;
  setPerson(userId, { status: "ready", profile, avatarUrl: keepUrl });
  if (profile.avatarVersion && !keepUrl) await loadAvatar(userId, profile.avatarVersion, () => live() && latest());
}

async function loadAvatar(userId: string, version: string, live: () => boolean): Promise<void> {
  const api = apiClient();
  if (!api) return;
  let picture: { mime: AvatarMime; bytes: Uint8Array } | null;
  try {
    picture = await api.avatarOf(userId);
  } catch (error) {
    if (!live() || isAborted(error)) return;
    if (error instanceof ApiError && error.kind === "auth") return disconnect(error.message);
    return; // initials stay
  }
  if (!live()) return;
  const current = untracked(people)[userId];
  // The profile changed or was hidden while the picture loaded.
  if (!current || current.status !== "ready" || current.profile?.avatarVersion !== version) return;
  if (!picture) return;
  showPicture(userId, version, picture.bytes, picture.mime);
}

function showPicture(userId: string, version: string, bytes: Uint8Array, mime: AvatarMime) {
  const current = untracked(people)[userId];
  if (!current) return;
  const url = URL.createObjectURL(new Blob([bytes as BlobPart], { type: mime }));
  setPerson(userId, { ...current, avatarUrl: url });
  shownVersion.set(userId, version);
}

/** The picture couldn't be drawn: drop it, initials show instead. */
export function pictureFailed(userId: string) {
  const current = untracked(people)[userId];
  if (current?.avatarUrl) setPerson(userId, { ...current, avatarUrl: null });
}

function startRechecks() {
  if (recheckTimer !== undefined) return;
  recheckTimer = setInterval(() => {
    for (const id of Object.keys(untracked(people))) void ensurePerson(id);
  }, RECHECK_MS);
}

/** Checks every seen profile again now (the user's Spaces or members changed). */
export function recheckAll() {
  for (const id of Object.keys(untracked(people))) void ensurePerson(id, true);
}

// Sharing a Space is what lets people see each other: check again whenever
// it may have changed (the user's Spaces, a Space's members) and on entering
// a Space.
let lastSharing = "";
effect(() => {
  const key = `${spaces().map((s) => s.id).join(",")}|${activeSpaceId() ?? ""}|${(members() ?? []).map((m) => m.userId).join(",")}`;
  untracked(() => {
    if (key === lastSharing) return;
    const first = lastSharing === "";
    lastSharing = key;
    if (!first && apiClient()) recheckAll();
  });
});

// ---------------------------------------------------------------- own profile (Settings › Profile)

export const myProfile = signal<Profile | null>(null);
export const myProfileLoading = signal(false);
export const myProfileError = signal<string | null>(null);
export const nameDraft = signal("");
export const bioDraft = signal("");
export const savingProfile = signal(false);
export const pictureBusy = signal<null | "upload" | "remove">(null);
/** The last outcome on the Profile page, said once. */
export const profileNotice = signal<{ ok: boolean; message: string } | null>(null);

/** The own profile's fields differ from what the server has. */
export function profileDirty(): boolean {
  const p = myProfile();
  return !!p && (nameDraft().trim() !== p.displayName || bioDraft().trim() !== p.bio.trim());
}

export function revertProfile() {
  const p = myProfile();
  if (!p || savingProfile()) return;
  batch(() => {
    nameDraft.set(p.displayName);
    bioDraft.set(p.bio);
    profileNotice.set(null);
  });
}

function adoptMine(p: Profile, keepDrafts = false) {
  batch(() => {
    myProfile.set(p);
    if (!keepDrafts) {
      nameDraft.set(p.displayName);
      bioDraft.set(p.bio);
    }
  });
  // The transcripts show the same name and picture.
  const current = untracked(people)[p.id];
  const keepUrl = p.avatarVersion && current?.avatarUrl && shownVersion.get(p.id) === p.avatarVersion ? current.avatarUrl : null;
  setPerson(p.id, { status: "ready", profile: p, avatarUrl: keepUrl });
  checkedAt.set(p.id, Date.now());
}

/** Loads the signed-in user's profile for editing. */
export async function loadMyProfile(): Promise<void> {
  const api = apiClient();
  const id = me();
  if (!api || !id || myProfileLoading() || savingProfile() || pictureBusy()) return;
  const gen = generation;
  const ok = sessionGuard();
  const live = () => ok() && gen === generation;
  batch(() => {
    myProfileLoading.set(true);
    myProfileError.set(null);
  });
  try {
    const p = await api.myProfile();
    if (!live()) return;
    adoptMine(p);
    const current = untracked(people)[p.id];
    if (p.avatarVersion && !current?.avatarUrl) await loadAvatar(p.id, p.avatarVersion, live);
  } catch (error) {
    if (!live() || isAborted(error)) return;
    if (error instanceof ApiError && error.kind === "auth") return disconnect(error.message);
    myProfileError.set(error instanceof ApiError && error.kind === "not_found" ? "This server doesn't support profiles yet." : describe(error, "Couldn't load your profile"));
  } finally {
    if (live()) myProfileLoading.set(false);
  }
}

/** Why the drafts can't be saved, or null. */
export function profileProblem(): string | null {
  const name = nameDraft().trim();
  if (!name) return "Enter a display name.";
  if ([...name].length > MAX_DISPLAY_NAME) return `Use at most ${MAX_DISPLAY_NAME} characters for your name.`;
  if ([...bioDraft().trim()].length > MAX_BIO) return `Use at most ${MAX_BIO} characters for your bio.`;
  return null;
}

export async function saveProfile(): Promise<void> {
  const api = apiClient();
  const p = myProfile();
  if (!api || !p || savingProfile() || myProfileLoading() || pictureBusy() || profileProblem()) return;
  const gen = generation;
  const ok = sessionGuard();
  const live = () => ok() && gen === generation;
  const patch: { displayName?: string; bio?: string } = {};
  const name = nameDraft().trim();
  const bio = bioDraft().trim();
  if (name !== p.displayName) patch.displayName = name;
  if (bio !== p.bio.trim()) patch.bio = bio;
  if (!Object.keys(patch).length) return;
  batch(() => {
    savingProfile.set(true);
    profileNotice.set(null);
  });
  try {
    const saved = await api.updateMyProfile(patch);
    if (!live()) return;
    adoptMine(saved);
    profileNotice.set({ ok: true, message: "Profile saved. People who share a Space with you see the new name and bio." });
  } catch (error) {
    if (!live() || isAborted(error)) return;
    if (error instanceof ApiError && error.kind === "auth") return disconnect(error.message);
    profileNotice.set({ ok: false, message: describe(error, "Couldn't save your profile") });
  } finally {
    if (live()) savingProfile.set(false);
  }
}

/** Shows the system's file picker for one PNG or JPEG, then sets it as the user's picture. */
export async function choosePicture(): Promise<void> {
  const api = apiClient();
  const p = myProfile();
  if (!api || !p || pictureBusy()) return;
  const gen = generation;
  const ok = sessionGuard();
  const live = () => ok() && gen === generation;
  batch(() => {
    pictureBusy.set("upload");
    profileNotice.set(null);
  });
  try {
    let picked;
    try {
      picked = await SystemPicker.pickFiles({ multiple: false, filters: [{ name: "Pictures (PNG, JPEG)", extensions: [...AVATAR_EXTENSIONS] }], maxSize: MAX_AVATAR_BYTES });
    } catch (error) {
      if (live()) profileNotice.set({ ok: false, message: `The file picker couldn't open: ${String((error as Error)?.message ?? error)}` });
      return;
    }
    if (!live() || picked.length === 0) return;
    const file = picked[0]!;
    if (file.status === "tooLarge" || !file.file || file.size > MAX_AVATAR_BYTES) {
      profileNotice.set({ ok: false, message: `That picture is too large. Use a PNG or JPEG up to ${MAX_AVATAR_BYTES / (1024 * 1024)} MB.` });
      return;
    }
    const bytes = new Uint8Array(await file.file.arrayBuffer());
    if (!live()) return;
    const mime = sniffAvatar(bytes);
    if (!mime) {
      profileNotice.set({ ok: false, message: "That file isn't a PNG or JPEG picture." });
      return;
    }
    const saved = await api.setMyAvatar(bytes, mime);
    if (!live()) return;
    const next = saved ?? (await api.myProfile());
    if (!live()) return;
    adoptMine(next, true);
    // Show what was sent at once; the server's version names it.
    if (next.avatarVersion) showPicture(next.id, next.avatarVersion, bytes, mime);
    profileNotice.set({ ok: true, message: "Picture updated." });
  } catch (error) {
    if (!live() || isAborted(error)) return;
    if (error instanceof ApiError && error.kind === "auth") return disconnect(error.message);
    profileNotice.set({ ok: false, message: describe(error, "Couldn't update your picture") });
  } finally {
    if (live()) pictureBusy.set(null);
  }
}

export async function removePicture(): Promise<void> {
  const api = apiClient();
  const p = myProfile();
  if (!api || !p || pictureBusy() || !p.avatarVersion) return;
  const gen = generation;
  const ok = sessionGuard();
  const live = () => ok() && gen === generation;
  batch(() => {
    pictureBusy.set("remove");
    profileNotice.set(null);
  });
  try {
    const saved = await api.removeMyAvatar();
    if (!live()) return;
    adoptMine(saved ?? { ...p, avatarVersion: null }, true);
    profileNotice.set({ ok: true, message: "Picture removed. Your initials show instead." });
  } catch (error) {
    if (!live() || isAborted(error)) return;
    if (error instanceof ApiError && error.kind === "auth") return disconnect(error.message);
    profileNotice.set({ ok: false, message: describe(error, "Couldn't remove your picture") });
  } finally {
    if (live()) pictureBusy.set(null);
  }
}

/** Drops everything of the session: profiles, pictures (URLs revoked), drafts and pending work. */
export function resetProfiles() {
  generation++;
  if (recheckTimer !== undefined) clearInterval(recheckTimer);
  recheckTimer = undefined;
  for (const state of Object.values(untracked(people))) if (state.avatarUrl) URL.revokeObjectURL(state.avatarUrl);
  checkedAt.clear();
  inflight.clear();
  shownVersion.clear();
  lastSharing = "";
  batch(() => {
    people.set({});
    myProfile.set(null);
    myProfileLoading.set(false);
    myProfileError.set(null);
    nameDraft.set("");
    bioDraft.set("");
    savingProfile.set(false);
    pictureBusy.set(null);
    profileNotice.set(null);
  });
}

onSessionEnd(resetProfiles);
