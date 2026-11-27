import { ApiError, decodeEvent, parseDeletion } from "./client";
import type { RequestFn, RoomEvent } from "./client";
import { decodeDirectoryPerson } from "./directory";
import type { DirectoryPerson } from "./directory";
import type { Reactions } from "../store";

export type DirectMessage = { id: string; peer: DirectoryPerson; unread: number; lastId: number };
export type ForwardSource =
  | { kind: "space"; space_id: string; room_id: string; event_id: number }
  | { kind: "chat"; event_id: number }
  | { kind: "dm"; dm_id: string; event_id: number };
export type ForwardDestination = { kind: "space"; space_id: string; room_id: string } | { kind: "dm"; user_id: string };

const root = (id: string) => `/v1/dms/${encodeURIComponent(id)}`;
function decodeDm(value: unknown): DirectMessage {
  const v = value as Record<string, unknown> | null;
  const peer = decodeDirectoryPerson(v?.peer);
  if (!peer || typeof v?.id !== "string" || !v.id || typeof v.unread !== "number" || !Number.isInteger(v.unread)
    || v.unread < 0 || v.unread > 99)
    throw new ApiError("protocol", "The server did not return a conversation.");
  const lastId = (v.last as { id?: number } | undefined)?.id;
  return { id: v.id, peer, unread: v.unread, lastId: lastId && Number.isSafeInteger(lastId) && lastId > 0 ? lastId : 0 };
}

function messages(value: unknown, deletions = false): RoomEvent[] {
  if (!Array.isArray(value)) throw new ApiError("protocol", "The server did not return a list of messages.");
  return value.map(v => {
    const event = decodeEvent(v);
    if (event?.kind !== "message" && !(deletions && event && parseDeletion(event))) {
      throw new ApiError("protocol", "The server returned an invalid message.");
    }
    return event!;
  });
}

export function dmsApi(request: RequestFn) {
  return {
    async listDms(signal?: AbortSignal): Promise<DirectMessage[]> {
      const data = await request("GET", "/v1/dms", undefined, signal) as { dms?: unknown[] } | null;
      if (!Array.isArray(data?.dms)) throw new ApiError("protocol", "The server did not return a conversation list.");
      return data.dms.map(decodeDm);
    },
    async startDm(userId: string) {
      const dm = decodeDm(await request("POST", "/v1/dms", { user_id: userId }));
      if (dm.peer.id !== userId) throw new ApiError("protocol", "The server did not confirm the selected person.");
      return dm;
    },
    async dmEvents(id: string, after: number) {
      const v = await request("GET", `${root(id)}/events?after=${after}&deletions=1`) as { events?: unknown; reactions?: unknown[] } | null;
      const events = messages(v?.events, true);
      if (!Array.isArray(v?.reactions)) throw new ApiError("protocol", "The server did not return the message reactions.");
      const reactions: Reactions = {};
      const ids = new Set(events.map(e => e.id));
      for (const item of v.reactions) {
        const r = item as Record<string, unknown> | null;
        if (!r || typeof r.target_event_id !== "number" || !ids.has(r.target_event_id)
          || r.reactor_kind !== "user" || typeof r.reactor_id !== "string"
          || typeof r.emoji !== "string" || !["👍", "👀", "✅", "🙏", "❤️", "🎉"].includes(r.emoji)) continue;
        const byEmoji = reactions[r.target_event_id] ??= {};
        byEmoji[r.emoji] = [...(byEmoji[r.emoji] ?? []), `user:${r.reactor_id}`];
      }
      return { events, reactions };
    },
    async postDm(id: string, text: string, replyTo?: number) {
      return messages([await request("POST", `${root(id)}/messages`, { text, reply_to: replyTo })])[0]!;
    },
    /** Deletes the caller's own DM message. */
    async deleteDm(id: string, eventId: number) {
      const data = await request("DELETE", `${root(id)}/events/${eventId}`) as { events?: unknown } | null;
      const events = messages(data?.events, true);
      if (!events.some(e => parseDeletion(e)?.targetEventId === eventId)) {
        throw new ApiError("protocol", "The server did not confirm the deletion.");
      }
      return events;
    },
    readDm: (id: string, lastRead: number) => request("PUT", `${root(id)}/read`, { last_read: lastRead }),
    hideDm: (id: string) => request("DELETE", root(id)),
    reactDm: (id: string, eventId: number, emoji: string, present: boolean) =>
      request(present ? "PUT" : "DELETE", `${root(id)}/events/${eventId}/reactions`, { emoji }),
    async forwardMessage(source: ForwardSource, destination: ForwardDestination, comment?: string) {
      const data = await request("POST", "/v1/forward", { source, destination, comment }) as
        { events?: unknown; omitted_attachments?: number } | null;
      const events = messages(data?.events);
      const omitted = data?.omitted_attachments;
      if (!events.length || typeof omitted !== "number" || !Number.isSafeInteger(omitted) || omitted < 0)
        throw new ApiError("protocol", "The server did not confirm the forwarded message.");
      return { events, omittedAttachments: omitted };
    },
  };
}
