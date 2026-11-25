// Test-only: a Space's own routes layered over the shared fake server
// (test/fakeServer.ts), as crates/server answers them (spaces.rs,
// space_agents.rs, space_prefs.rs): members read, the owner writes (403),
// outsiders get 404; handles are unique, "agent" is reserved; a Space keeps
// at least one agent; preferences are the caller's own. Fake tokens only.

import { BASE, header, TOKENS, world } from "../../../../test/fakeServer";

type Req = { method: string; url: string; headers: unknown; body?: string | null };
export type FakeAgent = {
  id: string;
  space_id: string;
  name: string;
  handle: string;
  description: string;
  instructions: string;
  tools: string[];
  is_default: boolean;
  avatar_version: string | null;
  color: string;
};
type Sel = { provider_connection_id: string; model_id?: string; reasoning_effort?: string };
type Prefs = { classify_unaddressed: boolean; default_model: Sel | null; agent_models: Record<string, Sel>; routing: boolean };

export const TOOLS = ["workspace.list", "workspace.read", "check.run", "threads"];
const COLORS = ["#4f46e5", "#0891b2", "#059669", "#ca8a04", "#ea580c", "#dc2626", "#db2777", "#7c3aed"];

export function spaceWorld() {
  const f = world();
  let next = 1;
  const s = {
    descriptions: new Map<string, string>([["acme", "Where Acme ships."]]),
    agents: new Map<string, FakeAgent[]>(),
    prefs: new Map<string, Prefs>(),
    /** Users with a usable System One connection (the classifier). */
    classifier: new Set<string>(),
    toolCatalog: [...TOOLS],
    defaultTools: [...TOOLS] as string[] | undefined,
    log: [] as { user: string; method: string; path: string; body: any }[],
  };
  const agent = (sid: string, a: Partial<FakeAgent> & { name: string; handle: string }): FakeAgent => {
    const full: FakeAgent = { id: `ag-${next++}`, space_id: sid, description: "Helps.", instructions: "", tools: [...TOOLS], is_default: false, avatar_version: null, color: COLORS[0]!, ...a };
    s.agents.set(sid, [...(s.agents.get(sid) ?? []), full]);
    return full;
  };
  for (const sid of f.state.spaces.keys()) agent(sid, { name: "Metro", handle: "agent", description: "The Space's general assistant.", is_default: true });

  const userOf = (req: Req) => {
    const auth = header(req, "authorization") ?? "";
    return Object.keys(TOKENS).find((u) => auth === `Bearer ${TOKENS[u]}`);
  };
  const ok = (body: unknown, status = 200) => ({ status, body });
  const err = (status: number, error: string) => ({ status, body: { error } });
  const initial = (name: string) => ([...name].find((c) => /[\p{L}\p{N}]/u.test(c)) ?? "?").toUpperCase();
  const agentJson = (a: FakeAgent) => ({ ...a, initial: initial(a.name), created_at: "2026-11-25T00:00:00Z", updated_at: "2026-11-25T00:00:00Z" });
  const prefsOf = (sid: string, user: string): Prefs => s.prefs.get(`${sid}/${user}`) ?? { classify_unaddressed: false, default_model: null, agent_models: {}, routing: true };

  function handle(req: Req): unknown {
    const user = userOf(req);
    const url = new URL(req.url);
    const path = url.pathname;
    const m = /^\/v1\/spaces\/([^/]+)(?:\/(leave|agents|prefs)(?:\/([^/]+)(\/avatar)?)?)?$/.exec(path);
    if (!m) return undefined;
    const [, sidRaw, sub, aid, avatar] = m;
    const sid = decodeURIComponent(sidRaw!);
    // Only the routes this lane adds; the rest stays with the shared double.
    if (!sub && req.method === "POST") return undefined;
    const body = req.body ? (() => { try { return JSON.parse(req.body!); } catch { return null; } })() : null;
    s.log.push({ user: user ?? "", method: req.method, path, body });
    if (!user) return err(401, "invalid token");
    const space = f.state.spaces.get(sid);
    if (!space || !space.members.has(user)) return err(404, "no such Space");
    const owner = space.owner === user;
    const spaceJson = () => ({ id: space.id, name: space.name, description: s.descriptions.get(sid) ?? "", role: owner ? "owner" : "member", runner_paired: !!space.runnerToken });

    if (!sub) {
      if (req.method === "GET") return ok(spaceJson());
      if (!owner) return err(403, "only the owner can do that");
      if (req.method === "PATCH") {
        if (body.name !== undefined) {
          if (!String(body.name).trim()) return err(400, "name must not be empty");
          space.name = String(body.name).trim();
        }
        if (body.description !== undefined) {
          if (String(body.description).length > 500) return err(400, "description must be at most 500 printable characters");
          s.descriptions.set(sid, String(body.description).trim());
        }
        return ok(spaceJson());
      }
      if (req.method === "DELETE") {
        if (body?.confirm_name !== space.name) return err(400, "confirm_name must be exactly the Space's name");
        f.state.spaces.delete(sid);
        s.agents.delete(sid);
        return { status: 204, body: "" };
      }
      return err(405, "method not allowed");
    }
    if (sub === "leave") {
      if (owner) return err(409, "the owner can't leave their Space; delete it instead");
      space.members.delete(user);
      return ok({ left: true, stopped_runs: 1 });
    }
    if (sub === "prefs") {
      if (req.method === "GET") return ok({ ...prefsOf(sid, user), classifier_available: s.classifier.has(user) });
      if (req.method === "PUT") {
        if (body.classify_unaddressed && !s.classifier.has(user)) return err(409, "turn on a System One connection you can use in this Space first");
        const p: Prefs = { classify_unaddressed: !!body.classify_unaddressed, default_model: body.default_model ?? null, agent_models: body.agent_models ?? {}, routing: body.routing !== false };
        s.prefs.set(`${sid}/${user}`, p);
        return ok({ ...p, classifier_available: s.classifier.has(user) });
      }
    }
    // Agents.
    const list = s.agents.get(sid) ?? [];
    const taken = (handle: string, except?: string) => list.some((a) => a.handle === handle && a.id !== except) || space.members.has(handle);
    if (!aid) {
      if (req.method === "GET") return ok({ agents: list.map(agentJson), tools: s.toolCatalog, default_tools: s.defaultTools });
      if (req.method === "POST") {
        if (!owner) return err(403, "only the owner can do that");
        if (!/^[a-z0-9_-]{1,32}$/.test(body.handle ?? "")) return err(400, "handle must be 1..=32 of a-z, 0-9, _ and -");
        if (body.handle === "agent") return err(409, "@agent is reserved for the Space's default agent; choose another handle");
        if (taken(body.handle)) return err(409, "that handle is already used by another agent or a member of this Space");
        if (!String(body.description ?? "").trim()) return err(400, "description must be one line of 1..=160 printable characters");
        const a = agent(sid, { name: body.name, handle: body.handle, description: body.description, instructions: body.instructions ?? "", tools: body.tools ?? [...TOOLS], color: body.color ?? COLORS[0] });
        if (body.is_default) for (const x of s.agents.get(sid)!) x.is_default = x.id === a.id;
        return ok(agentJson(a), 201);
      }
    }
    const a = list.find((x) => x.id === decodeURIComponent(aid ?? ""));
    if (!a) return err(404, "no such agent");
    if (avatar) {
      if (req.method === "GET") return a.avatar_version ? { status: 200, headers: { "content-type": "image/png" }, body: "not-a-real-png" } : err(404, "no avatar");
      if (!owner) return err(403, "only the owner can do that");
      if (req.method === "PUT") {
        a.avatar_version = `v${next++}`;
        return ok(agentJson(a));
      }
      if (req.method === "DELETE") {
        a.avatar_version = null;
        return { status: 204, body: "" };
      }
    }
    if (req.method === "GET") return ok(agentJson(a));
    if (!owner) return err(403, "only the owner can do that");
    if (req.method === "PATCH") {
      if (body.is_default === false) return err(400, "is_default can only be set to true; make another agent the default instead");
      if (body.handle !== undefined) {
        if (!/^[a-z0-9_-]{1,32}$/.test(body.handle)) return err(400, "handle must be 1..=32 of a-z, 0-9, _ and -");
        if (body.handle === "agent" && a.handle !== "agent") return err(409, "@agent is reserved for the Space's default agent; choose another handle");
        if (taken(body.handle, a.id)) return err(409, "that handle is already used by another agent or a member of this Space");
      }
      for (const k of ["name", "handle", "description", "instructions", "tools", "color"] as const) if (body[k] !== undefined) (a as any)[k] = body[k];
      if (body.is_default) for (const x of list) x.is_default = x.id === a.id;
      return ok(agentJson(a));
    }
    if (req.method === "DELETE") {
      if (list.length <= 1) return err(409, "a Space needs at least one agent; add another before deleting this one");
      const rest = list.filter((x) => x.id !== a.id);
      if (a.is_default) rest[0]!.is_default = true;
      s.agents.set(sid, rest);
      return ok({ deleted: a.id, default_agent_id: rest.find((x) => x.is_default)?.id ?? null });
    }
    return err(405, "method not allowed");
  }

  const network: Record<string, (req: Req) => unknown> = {};
  for (const method of ["GET", "POST", "PUT", "PATCH", "DELETE"]) {
    const base = f.network[`${method} ${BASE}/v1/*`]!;
    network[`${method} ${BASE}/v1/*`] = (req: Req) => {
      const mine = handle(req);
      return mine !== undefined ? mine : base(req);
    };
  }
  return { f, s, network, agent };
}
