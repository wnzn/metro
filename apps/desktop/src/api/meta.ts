import { ApiError } from "./client";
import type { RequestFn } from "./client";

export type SpacesAccess = { enabled: boolean; notice_shown: boolean; excluded_space_ids: string[] };
export type MetaNote = { tool: string; spaces?: string[]; count?: number };
export type SpacePost = {
  spaceId: string; spaceName: string; roomId: string; roomName: string; eventId: number;
  threadName?: string; parentRoomId?: string; runId?: string;
  agent: { id: string; handle: string; name: string };
};

const object = (v: unknown): v is Record<string, unknown> => !!v && typeof v === "object" && !Array.isArray(v);
const id = (v: unknown): v is string => typeof v === "string" && !!v.trim();
const eventId = (v: unknown): v is number => typeof v === "number" && Number.isSafeInteger(v) && v > 0;

export function readSpacePost(v: unknown): SpacePost | undefined {
  if (!object(v) || !id(v.space_id) || !id(v.room_id) || !eventId(v.event_id) ||
    typeof v.space_name !== "string" || typeof v.room_name !== "string" ||
    !object(v.agent) || !id(v.agent.id) || !id(v.agent.handle) || typeof v.agent.name !== "string") return;
  return {
    spaceId: v.space_id, spaceName: v.space_name, roomId: v.room_id, roomName: v.room_name, eventId: v.event_id,
    threadName: typeof v.thread_name === "string" ? v.thread_name : undefined,
    parentRoomId: id(v.parent_room_id) ? v.parent_room_id : undefined,
    runId: id(v.run_id) ? v.run_id : undefined,
    agent: { id: v.agent.id, handle: v.agent.handle, name: v.agent.name },
  };
}

export function readMetaNote(v: unknown): MetaNote | undefined {
  if (!object(v) || typeof v.tool !== "string" || !/^[a-z][a-z0-9_]{0,63}$/.test(v.tool)) return;
  return {
    tool: v.tool,
    spaces: Array.isArray(v.spaces) ? v.spaces.filter((s): s is string => typeof s === "string") : undefined,
    count: typeof v.count === "number" && Number.isSafeInteger(v.count) && v.count >= 0 ? v.count : undefined,
  };
}

export function metaApi(request: RequestFn) {
  const path = "/v1/chat/spaces-access";
  const access = async (patch?: Partial<SpacesAccess>): Promise<SpacesAccess> => {
    const v = await request(patch ? "PUT" : "GET", path, patch);
    if (!object(v) || typeof v.enabled !== "boolean" || typeof v.notice_shown !== "boolean" ||
      !Array.isArray(v.excluded_space_ids) || !v.excluded_space_ids.every(id)) {
      throw new ApiError("protocol", "Metro Server did not return Chat Space access settings.");
    }
    return { enabled: v.enabled, notice_shown: v.notice_shown, excluded_space_ids: v.excluded_space_ids };
  };
  return {
    getSpacesAccess: () => access(),
    setSpacesAccess: (patch: Partial<SpacesAccess>) => access(patch),
    async stopChatTurn(turnId: string) {
      const v = await request("POST", `/v1/chat/turns/${encodeURIComponent(turnId)}/stop`, {});
      if (!object(v) || v.turn_id !== turnId || v.status !== "cancelled") {
        throw new ApiError("protocol", "Metro Server did not confirm that the Chat turn was stopped.");
      }
    },
  };
}
