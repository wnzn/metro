// Profiles: a person's display name, short bio and picture.
//
// - The signed-in user reads and edits their own (`/v1/me/profile`), and sets
//   or removes their picture with raw PNG or JPEG bytes.
// - Anyone else's profile and picture come from `/v1/profiles/{id}`, which the
//   server answers only for the user themself or someone they share a Space
//   with right now; anyone else is a 404 (shown as initials, never an error).
// - Pictures travel only as bytes on the authenticated transport: never a
//   URL with a credential in it, and never fetched from anywhere else.

import { ApiError } from "./client";
import type { RequestFn } from "./client";

export type Profile = {
  /** The stable identity-provider id; never the display name. */
  id: string;
  displayName: string;
  bio: string;
  /** Changes whenever the picture does; null: no picture. */
  avatarVersion: string | null;
};

export type ProfilePatch = { displayName?: string; bio?: string };

/** Formats a picture may be in, by MIME type. */
export type AvatarMime = "image/png" | "image/jpeg";

/** The client's caps; the server is the judge and its message is shown as-is. */
export const MAX_AVATAR_BYTES = 1024 * 1024;
export const MAX_DISPLAY_NAME = 64;
export const MAX_BIO = 280;
export const AVATAR_EXTENSIONS = ["png", "jpg", "jpeg"] as const;

/** The transport for raw bytes (the client's `binary`), with the methods profiles need. */
export type RawFn = (
  method: "GET" | "POST" | "PUT",
  path: string,
  init: { body?: Blob | Uint8Array; contentType?: string; accept?: string; timeoutMs?: number; signal?: AbortSignal; range?: string },
) => Promise<{ status: number; contentType: string; bytes: Uint8Array; contentRange?: string | null }>;

const enc = encodeURIComponent;

const isObject = (v: unknown): v is Record<string, unknown> => typeof v === "object" && v !== null && !Array.isArray(v);

/** The profile in a response, or null when it isn't the documented shape. */
export function parseProfile(v: unknown): Profile | null {
  if (!isObject(v)) return null;
  const id = typeof v.id === "string" ? v.id : typeof v.id === "number" && Number.isFinite(v.id) ? String(v.id) : "";
  if (!id) return null;
  if (v.display_name != null && typeof v.display_name !== "string") return null;
  if (v.bio != null && typeof v.bio !== "string") return null;
  if (v.avatar_version != null && typeof v.avatar_version !== "string" && typeof v.avatar_version !== "number") return null;
  const version = v.avatar_version == null ? null : String(v.avatar_version).trim();
  return {
    id,
    displayName: typeof v.display_name === "string" ? v.display_name.trim() : "",
    bio: typeof v.bio === "string" ? v.bio : "",
    avatarVersion: version ? version : null,
  };
}

/** What the bytes are, by their first bytes (never by a file name or a header alone). */
export function sniffAvatar(bytes: Uint8Array): AvatarMime | null {
  if (bytes.length >= 8 && bytes[0] === 0x89 && bytes[1] === 0x50 && bytes[2] === 0x4e && bytes[3] === 0x47 && bytes[4] === 0x0d && bytes[5] === 0x0a && bytes[6] === 0x1a && bytes[7] === 0x0a) {
    return "image/png";
  }
  if (bytes.length >= 3 && bytes[0] === 0xff && bytes[1] === 0xd8 && bytes[2] === 0xff) return "image/jpeg";
  return null;
}

export type ProfileApi = ReturnType<typeof profileApi>;

export function profileApi(request: RequestFn, raw: RawFn) {
  const expect = (data: unknown, what: string): Profile => {
    const p = parseProfile(data);
    if (!p) throw new ApiError("protocol", `${what} did not return { id, display_name, bio, avatar_version }.`);
    return p;
  };
  return {
    async myProfile(): Promise<Profile> {
      return expect(await request("GET", "/v1/me/profile"), "GET /v1/me/profile");
    },

    async updateMyProfile(patch: ProfilePatch): Promise<Profile> {
      const body: Record<string, string> = {};
      if (patch.displayName !== undefined) body.display_name = patch.displayName;
      if (patch.bio !== undefined) body.bio = patch.bio;
      return expect(await request("PATCH", "/v1/me/profile", body), "PATCH /v1/me/profile");
    },

    /** Sets the user's picture. The bytes must already be a PNG or JPEG. */
    async setMyAvatar(bytes: Uint8Array, mime: AvatarMime): Promise<Profile | null> {
      const res = await raw("PUT", "/v1/me/profile/avatar", { body: bytes, contentType: mime, accept: "application/json", timeoutMs: 30_000 });
      return decodeJson(res.bytes);
    },

    async removeMyAvatar(): Promise<Profile | null> {
      const data = await request("DELETE", "/v1/me/profile/avatar");
      return parseProfile(data);
    },

    /** Someone's profile, as the server lets this user see it; null when it doesn't (404). */
    async profileOf(userId: string): Promise<Profile | null> {
      try {
        return expect(await request("GET", `/v1/profiles/${enc(userId)}`), "GET /v1/profiles/{id}");
      } catch (error) {
        if (error instanceof ApiError && error.kind === "not_found") return null;
        throw error;
      }
    },

    /** Someone's picture bytes; null when there is none or it isn't theirs to show (404). */
    async avatarOf(userId: string, signal?: AbortSignal): Promise<{ mime: AvatarMime; bytes: Uint8Array } | null> {
      let res;
      try {
        res = await raw("GET", `/v1/profiles/${enc(userId)}/avatar`, { accept: "image/png, image/jpeg", signal });
      } catch (error) {
        if (error instanceof ApiError && error.kind === "not_found") return null;
        throw error;
      }
      const mime = sniffAvatar(res.bytes);
      const declared = res.contentType.split(";")[0]!.trim().toLowerCase();
      if (!mime || (declared && declared !== mime && declared !== "image/jpg")) {
        throw new ApiError("protocol", "The picture the server sent isn't a PNG or JPEG.");
      }
      return { mime, bytes: res.bytes };
    },
  };
}

function decodeJson(bytes: Uint8Array): Profile | null {
  if (!bytes.length) return null;
  try {
    return parseProfile(JSON.parse(new TextDecoder().decode(bytes)));
  } catch {
    return null;
  }
}
