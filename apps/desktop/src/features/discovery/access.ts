// The session's search and schedule calls, through the store's client (which
// spreads `discoveryApi(request)` into itself). Never touches the token.

import { ApiError } from "../../api/client";
import type { DiscoveryApi } from "../../api/discovery";
import { apiClient, describe, disconnect, isAborted } from "../../store";

const METHODS = ["search", "indexSearch", "listSchedules", "createSchedule", "updateSchedule", "deleteSchedule", "scheduleHistory"] as const;

/** The signed-in session's discovery calls, or null when signed out. */
export function discovery(): DiscoveryApi | null {
  const c = apiClient() as unknown as Partial<DiscoveryApi> | null;
  if (!c) return null;
  if (METHODS.some((m) => typeof c[m] !== "function")) {
    throw new ApiError("protocol", "This build of the app can't reach search or scheduled tasks.");
  }
  return c as DiscoveryApi;
}

/**
 * Turns a failed call into what to show, or null when nothing should be
 * shown: aborted (signed out), or a 401 that ends the session here.
 */
export function failure(error: unknown, what: string): string | null {
  if (isAborted(error)) return null;
  if (error instanceof ApiError && error.kind === "auth") {
    disconnect(error.message);
    return null;
  }
  return describe(error, what);
}

export const statusOf = (error: unknown) => (error instanceof ApiError ? error.status : null);

/** The server's own `{error}` text inside an HTTP failure, if it gave one. */
export function serverDetail(error: unknown): string | null {
  if (!(error instanceof ApiError)) return null;
  const m = /HTTP \d{3}\)?: (.+?)\.?$/.exec(error.message);
  return m ? m[1] : null;
}
