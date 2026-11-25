// Test-only: messages with Library files, layered over the Documents & media
// fake (src/features/library/test/fixtures.ts), as the server contract says:
// POST .../messages and /v1/chat/messages take optional `asset_ids` (at most
// 10, distinct, each a file the sender can read in exactly the destination's
// library), and message events carry `attachments` metadata. Failure knobs let
// tests fail one post or one upload deterministically.

import { BASE, header, spaceMessageText, TOKENS } from "../../../../test/fakeServer";
import { libraryWorld } from "../../library/test/fixtures";

type Req = { method: string; url: string; headers: unknown; body?: string | null };

export function attachmentWorld() {
  const w = libraryWorld();
  const f = w.f;
  const knobs = {
    /** The next N message posts answer 503 (after their files were uploaded). */
    failPosts: 0,
    /** Uploads of a file with this name answer 413. */
    failUploadOf: null as string | null,
    uploadDelay: 0,
  };
  const userOf = (req: Req) => {
    const auth = header(req, "authorization") ?? "";
    return Object.keys(TOKENS).find((u) => auth === `Bearer ${TOKENS[u]}`);
  };
  const err = (status: number, error: string) => ({ status, body: { error } });

  const handle = (req: Req): unknown => {
    const url = new URL(req.url);
    const path = url.pathname;
    const user = userOf(req);
    if (!user) return undefined;
    if (req.method === "POST" && path === "/v1/library/assets" && knobs.failUploadOf && url.searchParams.get("name") === knobs.failUploadOf) {
      return err(413, "this file is over your Library quota");
    }
    const chat = path === "/v1/chat/messages";
    const room = /^\/v1\/spaces\/([^/]+)\/rooms\/([^/]+)\/messages$/.exec(path);
    if (req.method !== "POST" || (!chat && !room)) return undefined;
    const body = JSON.parse(req.body || "{}");
    const ids: unknown[] = body.asset_ids ?? [];
    if (!Array.isArray(ids) || ids.length > 10 || new Set(ids).size !== ids.length) return err(400, "asset_ids: at most 10 distinct ids");
    const sid = room ? decodeURIComponent(room[1]!) : null;
    const files = [];
    for (const id of ids) {
      const a = w.l.assets.find((x) => x.id === id);
      // Exactly the destination's library: the sender's personal one in Chat, the Space's in its rooms.
      const fits = !!a && (sid === null ? a.space_id === null && a.uploader_id === user : a.space_id === sid && !!f.state.spaces.get(sid)?.members.has(user));
      if (!fits) return err(400, "asset_ids: a file isn't in this conversation's library");
      files.push({ asset_id: a!.id, name: a!.name, mime_type: a!.mime_type, kind: a!.kind, size_bytes: a!.size_bytes });
    }
    if (!String(body.text ?? "").trim() && files.length === 0) return err(400, "a message needs text or files");
    if (!chat && body.invoke_agent && !String(body.text ?? "").trim()) return err(400, "asking the agent needs text");
    if (knobs.failPosts > 0) {
      knobs.failPosts--;
      return err(503, "try again");
    }
    if (files.length === 0) return undefined; // the plain fake answers text-only messages
    const key = sid === null ? `chat:${user}` : `${sid}/${decodeURIComponent(room![2]!)}`;
    if (!f.state.events.has(key)) f.state.events.set(key, []);
    f.state.log.push({ user, method: "POST", path, body });
    const mentions: { kind: string }[] = body.mentions ?? [];
    const asks = chat ? body.save_note !== true && body.invoke_agent !== false : body.invoke_agent === true || mentions.some(m => m.kind === "agent");
    const people = !chat && mentions.some(m => m.kind === "user");
    const audience = asks ? (people ? "mixed" : "agent") : people ? "users" : "room";
    const fields = chat ? { text: body.text ?? "" } : { ...spaceMessageText(body), mentions };
    const event = f.post(key, { kind: "message", actor: user, actor_id: user, ...fields, attachments: files, audience } as any);
    if (asks) f.post(key, { kind: "agent", actor: "agent", text: `(agent) re: ${body.text}` });
    return { status: 202, body: chat ? {} : { ...event, addressing_note: null, routing_note: null } };
  };

  const network: Record<string, (req: Req) => unknown> = {};
  for (const method of ["GET", "POST", "PUT", "PATCH", "DELETE"]) {
    const base = w.network[`${method} ${BASE}/v1/*`]!;
    network[`${method} ${BASE}/v1/*`] = (req: Req) => {
      const mine = handle(req);
      const response = mine !== undefined ? mine : base(req);
      if (knobs.uploadDelay && method === "POST" && new URL(req.url).pathname === "/v1/library/assets") {
        return { ...(response as object), delay: knobs.uploadDelay };
      }
      return response;
    };
  }
  return { ...w, network, knobs };
}
