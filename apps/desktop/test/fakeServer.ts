// An in-memory Metro server for headless tests (nsh test's network stubs).
// It speaks the desktop's HTTP contract closely enough to exercise the app:
// Spaces with members and runner tokens, rooms (with delegated threads),
// events with mentions and reactions, the personal Chat and agent runs.
// It is a test double, not the server.

export const BASE = "http://127.0.0.1:8787";
export const TOKENS: Record<string, string> = { alice: "tok-alice", bob: "tok-bob", carol: "tok-carol" };

type Ev = { id: number; kind: string; actor: string; text: string; mentions?: unknown[]; audience?: string };
type Req = { method: string; url: string; headers: unknown; body?: string | null };
type Space = { id: string; name: string; owner: string; members: Set<string>; runnerToken: string | null };
type Room = { id: string; name: string; parent_room_id?: string };
type Run = { id: string; space_id: string; room_id: string; status: string; actor: string; task: string };

export function header(req: Req, name: string): string | undefined {
  const h = req.headers as any;
  if (!h) return undefined;
  if (Array.isArray(h)) return h.find((p: [string, string]) => p[0].toLowerCase() === name)?.[1];
  for (const key of Object.keys(h)) if (key.toLowerCase() === name) return h[key];
  return undefined;
}

export function fakeServer() {
  let nextId = 100;
  const state = {
    down: false,
    ignoreCursor: false,
    spaces: new Map<string, Space>(),
    rooms: new Map<string, Room[]>(), // by space id
    events: new Map<string, Ev[]>(), // by `${space}/${room}` or `chat:${user}`
    runs: [] as Run[],
    log: [] as { user: string; method: string; path: string; body: any }[],
    afters: [] as string[],
  };

  const addSpace = (id: string, name: string, owner: string, members: string[] = [], paired = false) => {
    state.spaces.set(id, { id, name, owner, members: new Set([owner, ...members]), runnerToken: paired ? `runner-${id}` : null });
    state.rooms.set(id, [{ id: "lobby", name: "lobby" }]);
    state.events.set(`${id}/lobby`, []);
  };
  const addRoom = (sid: string, room: Room) => {
    state.rooms.get(sid)!.push(room);
    state.events.set(`${sid}/${room.id}`, []);
  };
  const post = (key: string, ev: Omit<Ev, "id">) => {
    const event = { id: nextId++, ...ev };
    state.events.get(key)!.push(event);
    return event;
  };
  const react = (key: string, target: number, emoji: string, present: boolean, kind: "user" | "agent", id: string) =>
    post(key, { kind: "reaction", actor: kind === "agent" ? "agent" : id, text: JSON.stringify({ target_event_id: target, emoji, present, reactor_kind: kind, reactor_id: id }) });

  const userOf = (req: Req) => {
    const auth = header(req, "authorization") ?? "";
    return Object.keys(TOKENS).find((u) => auth === `Bearer ${TOKENS[u]}`);
  };
  const ok = (body: unknown, status = 200) => ({ status, body });
  const err = (status: number, error: string) => ({ status, body: { error } });
  const spaceJson = (s: Space, user: string) => ({ id: s.id, name: s.name, role: s.owner === user ? "owner" : "member", runner_paired: !!s.runnerToken });

  function handle(req: Req) {
    if (state.down) return { error: "Connection refused" };
    const user = userOf(req);
    if (!user) return err(401, "invalid token");
    const url = new URL(req.url);
    const path = url.pathname;
    const body = req.body ? JSON.parse(req.body) : null;
    state.log.push({ user, method: req.method, path: path + url.search, body });
    const m = (re: RegExp) => re.exec(path);
    let r: RegExpExecArray | null;

    if (path === "/v1/me" && req.method === "GET") return ok({ id: user });
    if (path === "/v1/spaces" && req.method === "GET") return ok({ spaces: [...state.spaces.values()].filter((s) => s.members.has(user)).map((s) => spaceJson(s, user)) });
    if (path === "/v1/spaces" && req.method === "POST") {
      const id = `s${nextId++}`;
      addSpace(id, body.name, user);
      return ok(spaceJson(state.spaces.get(id)!, user), 201);
    }
    if (path === "/v1/runs" && req.method === "GET") {
      const visible = state.runs.filter((run) => state.spaces.get(run.space_id)?.members.has(user));
      return ok({ runs: [...visible].reverse() });
    }
    if ((r = m(/^\/v1\/runs\/([^/]+)\/stop$/)) && req.method === "POST") {
      const run = state.runs.find((x) => x.id === r![1]);
      if (!run) return err(404, "no such run");
      run.status = "stopped";
      return ok({});
    }
    if (path.startsWith("/v1/chat/")) {
      const key = `chat:${user}`;
      if (!state.events.has(key)) state.events.set(key, []);
      if (path === "/v1/chat/events") {
        const after = Number(url.searchParams.get("after"));
        return ok({ events: state.events.get(key)!.filter((e) => state.ignoreCursor || e.id > after) });
      }
      if (path === "/v1/chat/messages") {
        post(key, { kind: "message", actor: user, text: body.text });
        if (body.invoke_agent) post(key, { kind: "agent", actor: "agent", text: `(agent) re: ${body.text}` });
        return ok({}, 202);
      }
      if ((r = m(/^\/v1\/chat\/events\/(\d+)\/reactions$/))) {
        react(key, Number(r[1]), body.emoji, req.method === "PUT", "user", user);
        return ok({});
      }
    }
    if ((r = m(/^\/v1\/spaces\/([^/]+)(\/.*)?$/))) {
      const space = state.spaces.get(decodeURIComponent(r[1]));
      if (!space || !space.members.has(user)) return err(404, "no such space"); // outsiders get 404
      const rest = r[2] ?? "";
      const owner = space.owner === user;
      if (rest === "/members") {
        if (req.method === "GET") return ok({ members: [...space.members].map((u) => ({ user_id: u, role: u === space.owner ? "owner" : "member" })) });
        if (!owner) return err(403, "only the owner manages members");
        if (req.method === "POST") space.members.add(body.user_id);
        if (req.method === "DELETE") space.members.delete(body.user_id);
        return ok({});
      }
      if (rest === "/runner-token") {
        if (!owner) return err(403, "only the owner manages the runner");
        if (req.method === "POST") {
          space.runnerToken = `secret-runner-token-${nextId++}`;
          return ok({ token: space.runnerToken });
        }
        space.runnerToken = null;
        return ok({});
      }
      if (rest === "/rooms") {
        if (req.method === "GET") return ok({ rooms: state.rooms.get(space.id) });
        const room = { id: `r${nextId++}`, name: body.name };
        addRoom(space.id, room);
        return ok(room, 201);
      }
      const rr = /^\/rooms\/([^/]+)\/(events|messages|demo)(?:\/(\d+)\/reactions)?$/.exec(rest);
      if (rr) {
        const key = `${space.id}/${decodeURIComponent(rr[1])}`;
        if (!state.events.has(key)) return err(404, "no such room");
        if (rr[2] === "events" && rr[3]) {
          react(key, Number(rr[3]), body.emoji, req.method === "PUT", "user", user);
          return ok({});
        }
        if (rr[2] === "events") {
          const after = Number(url.searchParams.get("after"));
          state.afters.push(`${key}:${after}`);
          return ok({ events: state.events.get(key)!.filter((e) => state.ignoreCursor || e.id > after) });
        }
        if (rr[2] === "messages") {
          const mentions = body.mentions ?? [];
          const toAgent = body.invoke_agent || mentions.some((x: any) => x.kind === "agent");
          const toUsers = mentions.some((x: any) => x.kind === "user");
          const audience = toAgent && toUsers ? "mixed" : toAgent ? "agent" : toUsers ? "users" : "room";
          const ev = post(key, { kind: "message", actor: user, text: body.text, mentions, audience });
          if (toAgent) {
            post(key, { kind: "agent", actor: "agent", text: `(agent) on it: ${body.text}` });
            react(key, ev.id, "👀", true, "agent", "agent");
          }
          return ok({}, 202);
        }
        if (rr[2] === "demo") {
          if (!space.runnerToken) return err(409, "this Space has no paired runner");
          post(key, { kind: "demo", actor: user, text: "Demo: workspace.list on the paired runner" });
          post(key, { kind: "tool_result", actor: "runner", text: "README.md\nsrc/\n" });
          return ok({}, 202);
        }
      }
      return err(404, "not found");
    }
    return err(404, "not found");
  }

  const network: Record<string, (req: Req) => unknown> = {};
  for (const method of ["GET", "POST", "PUT", "DELETE"]) network[`${method} ${BASE}/v1/*`] = handle;
  return { state, network, addSpace, addRoom, post, react, posts: () => state.log.filter((l) => l.method !== "GET") };
}

/** The standard world: Alice owns Acme (paired, Bob is a member); Bob owns Side; Carol has nothing. */
export function world() {
  const f = fakeServer();
  f.addSpace("acme", "Acme", "alice", ["bob"], true);
  f.addSpace("side", "Side project", "bob", ["alice"]);
  f.post("acme/lobby", { kind: "message", actor: "bob", text: "hello from bob in acme" });
  f.post("side/lobby", { kind: "message", actor: "bob", text: "side lobby only" });
  return f;
}
