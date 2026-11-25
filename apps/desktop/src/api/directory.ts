import { ApiError } from "./client";
import type { RequestFn } from "./client";

export type DirectoryPerson = { id: string; name: string; avatarVersion: string | null };
export type DirectoryPage = { users: DirectoryPerson[]; nextCursor: string | null };

export function decodeDirectoryPerson(value: unknown): DirectoryPerson | null {
  const v = value as Record<string, unknown> | null;
  return typeof v?.id === "string" && !!v.id && typeof v.display_name === "string"
    && (v.avatar_version === null || typeof v.avatar_version === "string")
    ? { id: v.id, name: v.display_name, avatarVersion: v.avatar_version } : null;
}

export function directoryApi(request: RequestFn) {
  return {
    async searchDirectory(q: string, cursor: string | null, signal: AbortSignal): Promise<DirectoryPage> {
      const query = `q=${encodeURIComponent(q)}&limit=20${cursor ? `&cursor=${encodeURIComponent(cursor)}` : ""}`;
      const v = await request("GET", `/v1/directory?${query}`, undefined, signal) as Record<string, unknown> | null;
      if (!Array.isArray(v?.users)) throw new ApiError("protocol", "The directory did not return a list of people.");
      return { users: v.users.map(decodeDirectoryPerson).filter(p => p !== null),
        nextCursor: typeof v.next_cursor === "string" ? v.next_cursor : null };
    },
  };
}
