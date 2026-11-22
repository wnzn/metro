// Test-only: profile routes layered over the shared fake server
// (test/fakeServer.ts), as crates/server answers them: a profile is visible
// to its owner and to anyone who shares a Space with them right now, else
// 404; pictures are always served as PNG. Events carry `actor_id` and
// `created_at` as the server writes them. Fake tokens only.

import { BASE, header, TOKENS, world } from "../../../../test/fakeServer";

type Req = { method: string; url: string; headers: unknown; body?: string | null };
type FakeProfile = { display_name: string; bio: string | null; avatar: Uint8Array | null; version: number };

/** A real 48×48 PNG (a drawn portrait), so native screenshots show a picture. */
export const BOB_PNG = Uint8Array.from(atob("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"), (c) => c.charCodeAt(0));

export function profileWorld() {
  const f = world();
  const p = {
    profiles: new Map<string, FakeProfile>([
      ["alice", { display_name: "Alice Moreno", bio: "Runs the Acme rollout.", avatar: null, version: 0 }],
      ["bob", { display_name: "Bob Tan", bio: null, avatar: BOB_PNG, version: 1 }],
      ["carol", { display_name: "Carol", bio: null, avatar: null, version: 0 }],
    ]),
    log: [] as { user: string; method: string; path: string; headers: Record<string, string>; body: string | null }[],
  };
  const userOf = (req: Req) => {
    const auth = header(req, "authorization") ?? "";
    return Object.keys(TOKENS).find((u) => auth === `Bearer ${TOKENS[u]}`);
  };
  const ok = (body: unknown, status = 200) => ({ status, headers: { "cache-control": "no-store" }, body });
  const err = (status: number, error: string) => ({ status, body: { error } });
  const shares = (a: string, b: string) => a === b || [...f.state.spaces.values()].some((s) => s.members.has(a) && s.members.has(b));
  const json = (id: string) => {
    const x = p.profiles.get(id)!;
    return { id, display_name: x.display_name, bio: x.bio, avatar_version: x.avatar ? `v${x.version}` : null };
  };

  function handle(req: Req): unknown {
    const user = userOf(req);
    const url = new URL(req.url);
    const path = url.pathname;
    if (!path.startsWith("/v1/me/profile") && !path.startsWith("/v1/profiles/")) return undefined;
    const headers: Record<string, string> = {};
    for (const name of ["content-type", "authorization", "accept"]) {
      const v = header(req, name);
      if (v !== undefined) headers[name] = v;
    }
    p.log.push({ user: user ?? "", method: req.method, path, headers, body: req.body ?? null });
    if (!user) return err(401, "invalid token");
    if (path === "/v1/me/profile" && req.method === "GET") return ok(json(user));
    if (path === "/v1/me/profile" && req.method === "PATCH") {
      const body = req.body ? JSON.parse(req.body) : {};
      const me = p.profiles.get(user)!;
      if (body.display_name !== undefined) {
        const name = body.display_name === null ? user : String(body.display_name).trim();
        if (!name || name.length > 64) return err(400, "display_name must be 1 to 64 characters");
        if (name.toLowerCase() === "agent") return err(409, "that name is reserved");
        me.display_name = name;
      }
      if (body.bio !== undefined) me.bio = body.bio === null || body.bio === "" ? null : String(body.bio);
      return ok(json(user));
    }
    if (path === "/v1/me/profile/avatar" && req.method === "PUT") {
      const type = (header(req, "content-type") ?? "").toLowerCase();
      if (type !== "image/png" && type !== "image/jpeg") return err(415, "send a PNG or JPEG picture");
      const me = p.profiles.get(user)!;
      // The server re-encodes to PNG; the double keeps a known picture.
      me.avatar = BOB_PNG;
      me.version++;
      return ok(json(user));
    }
    if (path === "/v1/me/profile/avatar" && req.method === "DELETE") {
      p.profiles.get(user)!.avatar = null;
      return { status: 204, body: "" };
    }
    const m = /^\/v1\/profiles\/([^/]+)(\/avatar)?$/.exec(path);
    if (m && req.method === "GET") {
      const id = decodeURIComponent(m[1]!);
      if (!p.profiles.has(id) || !shares(user, id)) return err(404, "not found");
      if (!m[2]) return ok(json(id));
      const avatar = p.profiles.get(id)!.avatar;
      if (!avatar) return err(404, "no picture");
      return { status: 200, headers: { "content-type": "image/png", "cache-control": "private, no-store" }, body: avatar };
    }
    return err(404, "not found");
  }

  const network: Record<string, (req: Req) => unknown> = {};
  for (const method of ["GET", "POST", "PUT", "PATCH", "DELETE"]) {
    const base = f.network[`${method} ${BASE}/v1/*`]!;
    network[`${method} ${BASE}/v1/*`] = (req: Req) => {
      const mine = handle(req);
      return mine !== undefined ? mine : base(req);
    };
  }
  /** A room event as the server writes it now: the author's stable id and the stored time. */
  const say = (key: string, ev: { kind: string; actor: string; text: string; actor_id?: string | null; created_at?: string | null; mentions?: unknown[]; audience?: string; activity?: unknown }) =>
    f.post(key, { actor_id: null, created_at: null, ...ev } as any);
  return { f, p, network, say };
}
