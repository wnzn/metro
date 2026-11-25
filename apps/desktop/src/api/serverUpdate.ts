import type { RequestFn } from "./client";

export type ServerUpdate = { current: string; available: boolean; latest: { version: string; releaseUrl: unknown } | null };

export function serverUpdateApi(request: RequestFn) {
  return {
    /** Org admins only. A server without this endpoint also answers 404. */
    async serverUpdate(signal: AbortSignal): Promise<ServerUpdate | null> {
      const data = await request("GET", "/v1/server/update", undefined, signal);
      if (typeof data !== "object" || data === null || !("current" in data) || typeof data.current !== "string") return null;
      const latest = "latest" in data ? data.latest : null;
      return {
        current: data.current,
        available: "available" in data && data.available === true,
        latest: typeof latest === "object" && latest !== null && "version" in latest
          && typeof latest.version === "string" && "historical" in latest && latest.historical === false
          ? { version: latest.version, releaseUrl: "release_url" in latest ? latest.release_url : null }
          : null,
      };
    },
  };
}
