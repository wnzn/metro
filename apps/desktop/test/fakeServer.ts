// An in-memory Metro server for headless tests (nsh test's network stubs).
// It speaks the desktop's HTTP contract closely enough to exercise the app:
// Spaces with members and runner tokens, rooms (with delegated threads),
// events with mentions and reactions, the personal Chat and agent runs,
// registered runners and their Space attachments, the provider catalog and
// personal model connections (with a device connector that is offline until
// a test says otherwise), connection capabilities and sharing, the models a
// user may pick, and per-user model routing.
// It is a test double, not the server.

export const BASE = "http://127.0.0.1:8787";
export const TOKENS: Record<string, string> = { alice: "tok-alice", bob: "tok-bob", carol: "tok-carol" };

type Activity = { id: string; name: string; source: "agent" | "demo"; phase?: string };
type Ev = { id: number; kind: string; actor: string; text: string; mentions?: unknown[]; audience?: string; activity?: Activity };
type Req = { method: string; url: string; headers: unknown; body?: string | null };
type Space = { id: string; name: string; owner: string; members: Set<string>; runnerToken: string | null };
type Room = { id: string; name: string; parent_room_id?: string };
type Run = { id: string; space_id: string; room_id: string; status: string; actor: string; task: string };
type Runner = { id: string; name: string; visibility: "personal" | "organization"; owner: string; token: string };
type Capability = "chat" | "system_one" | "embeddings";
type Share = { visibility: "personal" | "spaces" | "organization"; space_ids: string[] };
type Thinking = { efforts: string[]; wire: "none" | "reasoning_effort" | "reasoning" };
/** One model of a connection's own list (provider_connection_models). */
type ConnModel = { model_id: string; display_name: string | null; listed_name: string | null; enabled: boolean; available: boolean; source: "listed" | "manual"; position: number; thinking: Thinking | null };
type Conn = { id: string; owner: string; name: string; provider_id: string; base_url: string | null; model: string | null; is_default: boolean; key: string | null; deviceToken: string | null; capability: Capability; share: Share; models: ConnModel[] };
type RoutingCase = { key: string; label: string; description: string; connection_id: string | null };
type Routing = { enabled: boolean; infer_addressing: boolean; classifier_id: string | null; embedding_id: string | null; default_chat_id: string | null; min_directedness: number; min_route_probability: number; cases: RoutingCase[] };
const DEFAULT_ROUTING: Routing = { enabled: false, infer_addressing: false, classifier_id: null, embedding_id: null, default_chat_id: null, min_directedness: 0.85, min_route_probability: 0.65, cases: [] };

/** The catalog this double offers: chat, System One and embeddings adapters, on devices and on the server. */
export const CATALOG: { providers: { id: string; name: string; requires_key: boolean; allow_custom_url: boolean; default_base_url?: string; execution: string; capability: Capability; lists_models: boolean }[]; allowed_base_urls: string[] } = {
  providers: [
    { id: "device-openai-oauth", name: "Sign in with ChatGPT (eligible local integrations)", requires_key: false, allow_custom_url: false, execution: "device", capability: "chat", lists_models: true },
    { id: "device-claude-code", name: "Claude (local CLI)", requires_key: false, allow_custom_url: false, execution: "device", capability: "chat", lists_models: true },
    { id: "device-openai-compatible", name: "Local OpenAI-compatible endpoint", requires_key: false, allow_custom_url: false, execution: "device", capability: "chat", lists_models: true },
    { id: "device-systemone", name: "System One on this device", requires_key: false, allow_custom_url: false, execution: "device", capability: "system_one", lists_models: true },
    { id: "device-embeddings", name: "Embeddings on this device", requires_key: false, allow_custom_url: false, execution: "device", capability: "embeddings", lists_models: true },
    { id: "openai", name: "OpenAI API", requires_key: true, allow_custom_url: false, default_base_url: "https://api.openai.com/v1", execution: "server", capability: "chat", lists_models: true },
    { id: "openai-embeddings", name: "OpenAI embeddings", requires_key: true, allow_custom_url: false, default_base_url: "https://api.openai.com/v1", execution: "server", capability: "embeddings", lists_models: true },
    { id: "anthropic", name: "Anthropic", requires_key: true, allow_custom_url: false, default_base_url: "https://api.anthropic.com/v1", execution: "server", capability: "chat", lists_models: true },
    { id: "custom", name: "Custom OpenAI-compatible endpoint", requires_key: false, allow_custom_url: true, execution: "server", capability: "chat", lists_models: true },
    { id: "systemone", name: "TypeSafe System One", requires_key: true, allow_custom_url: false, default_base_url: "https://api.typesafe.ai/v1", execution: "server", capability: "system_one", lists_models: true },
    { id: "custom-embeddings", name: "Self-hosted embeddings (OpenAI-compatible)", requires_key: false, allow_custom_url: true, execution: "server", capability: "embeddings", lists_models: false },
  ],
  allowed_base_urls: ["http://127.0.0.1:11434/v1"],
};
export const ORG_ADMINS = ["alice"];

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
    runners: new Map<string, Runner>(),
    attached: new Map<string, string>(), // space id -> runner id
    attachIfEmpty: true,
    missingEmptyAttachRoute: false,
    beforeEmptyAttach: null as ((sid: string) => void) | null,
    runnerRelease: new Map<string, { presence: Record<string, unknown>; update: Record<string, unknown> }>(),
    rejectRunnerUpdate: new Set<string>(),
    connections: new Map<string, Conn>(),
    /** Device connection ids whose metro-connector is online. */
    online: new Set<string>(),
    /** Model routing per user (missing: the defaults, all off). */
    routing: new Map<string, Routing>(),
    /** The server has no model routing (an older version): 404. */
    noRouting: false,
    /** The operator's METRO_MODEL (GET /v1/models/available `operator_model`, for users with no default chat connection). */
    operatorModel: null as string | null,
    /** What each provider lists (POST …/model-list/refresh, `fetch_models`); a device lists only while online. */
    listings: new Map<string, { id: string; name?: string }[]>([
      ["openai", [{ id: "gpt-fixture-1" }, { id: "gpt-fixture-2", name: "GPT Fixture 2" }]],
      ["device", [{ id: "fixture-local-small" }, { id: "fixture-local-large" }]],
    ]),
    /** Delays (ms) for GET paths (pathname, or pathname plus query), to test that late answers don't win. */
    delays: new Map<string, number>(),
    /**
     * Monitoring events (GET /v1/monitoring/events): per Space (owner only,
     * `?space_id=`), the caller's own personal Chat (`?scope=me`, from the
     * token alone) and the instance (org admins). `supported: false` answers
     * 404, as a server without monitoring does; `noMe: true` answers 400 to
     * `scope`, as a server with Space and instance events only does;
     * `unauthorized` answers 401.
     */
    monitoring: { supported: true, noMe: false, unauthorized: false, instance: [] as unknown[], spaces: new Map<string, unknown[]>(), users: new Map<string, unknown[]>() },
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
  const spaceJson = (s: Space, user: string) => ({ id: s.id, name: s.name, role: s.owner === user ? "owner" : "member", runner_paired: !!s.runnerToken || state.attached.has(s.id) });
  const admin = (user: string) => ORG_ADMINS.includes(user);
  const visibleRunner = (r: Runner | undefined, user: string) => !!r && (r.visibility === "organization" || r.owner === user);
  const runnerJson = (r: Runner, user: string) => ({
    id: r.id, name: r.name, visibility: r.visibility,
    can_manage: r.visibility === "personal" ? r.owner === user : admin(user),
    ...state.runnerRelease.get(r.id),
  });
  /** Self-hosted OpenAI-compatible connections take per-model thinking options. */
  const customThinking = (c: Conn) => c.provider_id === "custom" || c.provider_id === "device-openai-compatible";
  const modelViews = (c: Conn) =>
    [...(c.models ?? [])].sort((a, b) => a.position - b.position).map((m) => ({
      model_id: m.model_id, display_name: m.display_name ?? m.listed_name ?? m.model_id, custom_name: m.display_name, listed_name: m.listed_name,
      enabled: m.enabled, available: m.available, source: m.source, position: m.position, is_default: m.model_id === c.model,
      thinking: m.thinking, reasoning_efforts: m.thinking?.efforts ?? [],
    }));
  const connJson = (c: Conn) => ({ id: c.id, name: c.name, provider_id: c.provider_id, base_url: c.base_url, model: c.model, is_default: c.is_default, has_key: !!c.key, capability: c.capability, visibility: c.share.visibility, custom_thinking: customThinking(c), models: modelViews(c) });
  const listView = (c: Conn) => ({ connection_id: c.id, default_model: c.model, custom_thinking: customThinking(c), models: modelViews(c) });
  /** The connection's model is always an enabled entry of its list; with none, the first enabled one becomes it. */
  const settle = (c: Conn) => {
    c.models ??= [];
    if (c.model && !c.models.some((m) => m.model_id === c.model)) c.models.push({ model_id: c.model, display_name: null, listed_name: null, enabled: true, available: true, source: "manual", position: c.models.length, thinking: null });
    if (!c.model) c.model = c.models.find((m) => m.enabled)?.model_id ?? null;
  };
  /** What a provider lists now, or an error. */
  const listingOf = (c: Conn): { ok: true; models: { id: string; name?: string }[] } | { ok: false; status: number; error: string } => {
    if (c.provider_id.startsWith("device-")) return c.deviceToken && state.online.has(c.id) ? { ok: true, models: state.listings.get("device") ?? [] } : { ok: false, status: 503, error: "metro-connector is offline" };
    const listed = state.listings.get(c.provider_id);
    return listed ? { ok: true, models: listed } : { ok: false, status: 422, error: "this endpoint does not list models" };
  };
  /** Merges a listing: new ones enabled, gone ones unavailable; the owner's entries stay. */
  const merge = (c: Conn, listed: { id: string; name?: string }[]) => {
    settle(c);
    let added = 0;
    let unavailable = 0;
    for (const l of listed) {
      const m = c.models.find((x) => x.model_id === l.id);
      if (m) {
        m.available = true;
        if (l.name) m.listed_name = l.name;
      } else {
        c.models.push({ model_id: l.id, display_name: null, listed_name: l.name ?? null, enabled: true, available: true, source: "listed", position: c.models.length, thinking: null });
        added++;
      }
    }
    for (const m of c.models) if (m.source === "listed" && m.available && !listed.some((l) => l.id === m.model_id)) (m.available = false), unavailable++;
    settle(c);
    return { added, unavailable };
  };
  /** Whether `user` may use `c`: their own, or shared with the organization or a Space both are in now. */
  const usable = (c: Conn, user: string, scope?: string | null) =>
    c.owner === user ||
    c.share.visibility === "organization" ||
    (c.share.visibility === "spaces" && c.share.space_ids.some((sid) => {
      if (scope !== undefined && sid !== scope) return false;
      const s = state.spaces.get(sid);
      return !!s && s.members.has(user) && s.members.has(c.owner);
    }));
  const usableAs = (id: unknown, capability: Capability, user: string) => {
    const c = typeof id === "string" ? state.connections.get(id) : undefined;
    return !!c && c.capability === capability && usable(c, user) && !!c.model;
  };
  const spaceRunnerJson = (s: Space) => {
    if (s.runnerToken) return { id: null, name: "Dedicated Space runner", visibility: "dedicated", kind: "dedicated" };
    const r = state.runners.get(state.attached.get(s.id) ?? "");
    return r ? { id: r.id, name: r.name, visibility: r.visibility, kind: "registered" } : null;
  };
  const addRunner = (name: string, visibility: "personal" | "organization", owner: string) => {
    const r: Runner = { id: `rn${nextId++}`, name, visibility, owner, token: `metro_rt_v2_secret${nextId++}` };
    state.runners.set(r.id, r);
    return r;
  };

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

    if (path === "/v1/monitoring/events" && req.method === "GET") {
      if (state.monitoring.unauthorized) return err(401, "invalid token");
      if (!state.monitoring.supported) return err(404, "not found");
      const sid = url.searchParams.get("space_id");
      const scope = url.searchParams.get("scope");
      if ([...url.searchParams.keys()].some((k) => k !== "space_id" && k !== "scope") || (sid !== null && scope !== null)) return err(400, "ask for one scope");
      if (scope !== null) {
        if (state.monitoring.noMe) return err(400, "unknown query");
        if (scope !== "me") return err(400, "scope must be `me`");
        return ok({ events: state.monitoring.users.get(user) ?? [] });
      }
      if (sid !== null) {
        const s = state.spaces.get(sid);
        if (!s || !s.members.has(user)) return err(404, "no such space");
        if (s.owner !== user) return err(403, "only the owner sees a Space's monitoring events");
        return ok({ events: state.monitoring.spaces.get(sid) ?? [] });
      }
      if (url.search) return err(400, "unknown query");
      if (!admin(user)) return err(403, "only organization admins see instance events");
      return ok({ events: state.monitoring.instance });
    }
    if (path === "/v1/me" && req.method === "GET") return ok({ id: user, actor: user, is_org_admin: admin(user) });
    if (path === "/v1/runners" && req.method === "GET") {
      return ok({
        runners: [...state.runners.values()].filter((x) => visibleRunner(x, user)).map((x) => runnerJson(x, user)),
        ...(state.attachIfEmpty ? { attach_if_empty: true } : {}),
      });
    }
    if (path === "/v1/runners" && req.method === "POST") {
      if (!body?.name?.trim()) return err(400, "name is required");
      if (body.visibility === "organization" && !admin(user)) return err(403, "only an organization admin can create an organization runner");
      const created = addRunner(body.name.trim(), body.visibility, user);
      return ok({ runner: runnerJson(created, user), token: created.token }, 201);
    }
    if ((r = m(/^\/v1\/runners\/([^/]+)(\/token|\/update)?$/))) {
      const runner = state.runners.get(decodeURIComponent(r[1]));
      if (!visibleRunner(runner, user)) return err(404, "no such runner");
      if (!runnerJson(runner!, user).can_manage) return err(403, "only an organization admin can manage an organization runner");
      if (r[2] === "/update" && req.method === "POST") {
        if (state.rejectRunnerUpdate.has(runner!.id)) return err(403, "not permitted to update this runner");
        const release = state.runnerRelease.get(runner!.id);
        if (!release || !release.update.available || release.update.latest !== body?.version ||
            !release.presence.online || !release.presence.self_update || release.update.pending) {
          return err(409, "runner isn't eligible");
        }
        const pending = { target: body.version, requested_at: Date.now() };
        release.update.pending = pending;
        return ok({ pending }, 202);
      }
      if (r[2] === "/token" && req.method === "POST") {
        runner!.token = `metro_rt_v2_secret${nextId++}`;
        return ok({ token: runner!.token });
      }
      if (!r[2] && req.method === "DELETE") {
        state.runners.delete(runner!.id);
        for (const [sid, rid] of [...state.attached]) if (rid === runner!.id) state.attached.delete(sid);
        return ok(undefined, 204);
      }
    }
    if (path === "/v1/provider-catalog" && req.method === "GET") return ok(CATALOG);
    if (path === "/v1/providers" && req.method === "GET") {
      return ok({ connections: [...state.connections.values()].filter((c) => c.owner === user).map(connJson) });
    }
    if (path === "/v1/providers" && req.method === "POST") {
      const provider = CATALOG.providers.find((p) => p.id === body?.provider_id);
      if (!provider) return err(400, "unknown provider");
      if (body.base_url && !CATALOG.allowed_base_urls.includes(body.base_url)) return err(400, "base_url is not allowed");
      if (provider.requires_key && !body.api_key) return err(400, "api_key is required");
      const first = ![...state.connections.values()].some((x) => x.owner === user && x.capability === provider.capability);
      const c: Conn = { id: `pc${nextId++}`, owner: user, name: body.name, provider_id: provider.id, base_url: body.base_url ?? provider.default_base_url ?? null, model: body.model ?? null, is_default: first, key: body.api_key ?? null, deviceToken: null, capability: provider.capability, share: { visibility: "personal", space_ids: [] }, models: [] };
      for (const m of (body.models ?? []) as { model_id: string; display_name?: string; thinking?: Thinking }[]) {
        if (m.thinking && !customThinking(c)) return err(422, "thinking options can be set only on self-hosted OpenAI-compatible connections");
        c.models.push({ model_id: m.model_id, display_name: m.display_name ?? null, listed_name: null, enabled: true, available: true, source: "manual", position: c.models.length, thinking: m.thinking ?? null });
      }
      settle(c);
      let models_error: string | undefined;
      if (body.fetch_models) {
        const l = listingOf(c);
        if (l.ok) merge(c, l.models);
        else models_error = l.error;
      }
      state.connections.set(c.id, c);
      return ok({ ...connJson(c), ...(models_error ? { models_error } : {}) }, 201);
    }
    if (path === "/v1/models/available" && req.method === "GET") {
      const sid = url.searchParams.get("space_id");
      if (sid !== null && !state.spaces.get(sid)?.members.has(user)) return err(404, "no such Space");
      const scope = url.searchParams.get("scope") === "any" ? undefined : sid;
      const models = [...state.connections.values()]
        .filter((c) => usable(c, user, scope) && !!c.model)
        .map((c) => {
          const owned = c.owner === user;
          const preset = !owned && c.provider_id === "device-openai-oauth";
          const execution = CATALOG.providers.find((p) => p.id === c.provider_id)?.execution ?? "server";
          return {
            connection_id: c.id, name: c.name, provider_id: c.provider_id, model: c.model, capability: c.capability, visibility: c.share.visibility, owned,
            owner_id: c.owner, execution,
            runs_on: owned ? "your_connection" : preset ? "your_own_sign_in" : "owner_connection",
            billing: owned || preset ? "you" : "owner",
            ...(preset ? { ready: [...state.connections.values()].some((x) => x.owner === user && x.provider_id === "device-openai-oauth" && !!x.model) } : {}),
            is_default: owned && c.is_default,
          };
        });
      const hasDefault = [...state.connections.values()].some((c) => c.owner === user && c.capability === "chat" && c.is_default && !!c.model);
      const operator = state.operatorModel && !hasDefault ? { model_id: state.operatorModel, label: "Server model (operator)" } : null;
      return ok({ models, operator_model: operator });
    }
    if (path === "/v1/model-routing") {
      if (state.noRouting) return err(404, "not found");
      if (req.method === "GET") return ok(state.routing.get(user) ?? DEFAULT_ROUTING);
      if (req.method === "PUT") {
        const b = body as Routing;
        const p = (n: unknown) => typeof n === "number" && n >= 0 && n <= 1;
        if (!p(b.min_directedness) || b.min_directedness < 0.5 || !p(b.min_route_probability)) return err(400, "min_directedness must be 0.5..=1, min_route_probability 0..=1");
        if ((b.cases ?? []).length > 16 || (b.cases ?? []).some((c) => !/^[a-z][a-z0-9_]{0,31}$/.test(c.key))) return err(400, "bad cases");
        if (b.classifier_id && !usableAs(b.classifier_id, "system_one", user)) return err(400, "classifier_id is not a System One connection you can use");
        if (b.embedding_id && !usableAs(b.embedding_id, "embeddings", user)) return err(400, "embedding_id is not an embeddings connection you can use");
        if (b.default_chat_id && !usableAs(b.default_chat_id, "chat", user)) return err(400, "default_chat_id is not a chat connection you can use");
        if ((b.enabled && b.cases?.length) && !b.classifier_id) return err(400, "a classifier is required");
        for (const c of b.cases ?? []) if (!usableAs(c.connection_id, "chat", user)) return err(400, `case ${c.key}: not a chat connection you can use`);
        const stored: Routing = { ...b, cases: (b.cases ?? []).map((c) => ({ ...c })) };
        state.routing.set(user, stored);
        return ok(stored);
      }
    }
    if ((r = m(/^\/v1\/providers\/([^/]+)(\/models|\/device-token|\/sharing|\/model-list\/refresh|\/model-list)?$/))) {
      const c = state.connections.get(decodeURIComponent(r[1]));
      if (!c || c.owner !== user) return err(404, "no such connection"); // only the owner sees or changes a share
      if (r[2] === "/sharing") {
        const view = () => ({ ...c.share, mode: c.provider_id === "device-openai-oauth" ? "preset" : "owner_connection", capability: c.capability });
        if (req.method === "GET") return ok(view());
        if (req.method === "PUT") {
          const v = body?.visibility;
          if (v !== "personal" && v !== "spaces" && v !== "organization") return err(400, "bad visibility");
          const ids: string[] = v === "spaces" ? [...new Set<string>(body.space_ids ?? [])] : [];
          if (v === "spaces" && ids.length === 0) return err(400, "pick at least one Space");
          if (ids.some((sid) => !state.spaces.get(sid)?.members.has(user))) return err(400, "space_ids must be Spaces you are a member of");
          if (v !== "personal" && !c.model) return err(409, "choose a model before sharing this connection");
          c.share = { visibility: v, space_ids: ids.sort() };
          return ok(view());
        }
      }
      const device = c.provider_id.startsWith("device-");
      if (r[2] === "/device-token") {
        if (!device) return err(400, "not a device connection");
        if (req.method === "POST") {
          c.deviceToken = `metro_pc_secret${nextId++}`;
          state.online.delete(c.id); // the connector must pick up the new token
          return ok({ token: c.deviceToken }, 201);
        }
        if (req.method === "GET") {
          const online = !!c.deviceToken && state.online.has(c.id);
          return ok({ paired: !!c.deviceToken, online, last_seen_secs: online ? 3 : null });
        }
        if (req.method === "DELETE") {
          c.deviceToken = null;
          state.online.delete(c.id);
          return ok(undefined, 204);
        }
      }
      if ((r[2] === "/models" || r[2] === "/model-list/refresh") && req.method === "POST") {
        const l = listingOf(c);
        if (!l.ok) return err(l.status, l.error);
        const ids = l.models.map((x) => x.id);
        if (c.capability !== "chat") return ok({ models: ids });
        const synced = merge(c, l.models);
        return ok({ models: ids, ...synced, connection: connJson(c) });
      }
      if (r[2] === "/model-list") {
        settle(c);
        if (req.method === "GET") return ok(listView(c));
        const id = req.method === "DELETE" ? url.searchParams.get("model_id") : body?.model_id;
        if (typeof id !== "string" || !id.trim()) return err(400, "model_id is required");
        const entry = c.models.find((x) => x.model_id === id);
        if (req.method === "POST") {
          if (entry) return err(409, "that connection already has this model");
          if (body.thinking && !customThinking(c)) return err(422, "thinking options can be set only on self-hosted OpenAI-compatible connections");
          c.models.push({ model_id: id, display_name: body.display_name ?? null, listed_name: null, enabled: body.enabled ?? true, available: true, source: "manual", position: c.models.length, thinking: body.thinking ?? null });
          if (body.default) c.model = id;
          settle(c);
          return ok(listView(c), 201);
        }
        if (!entry) return err(404, "that connection has no such model");
        if (req.method === "PATCH") {
          if (body.enabled === false && c.model === id) return err(409, "this is the connection's default model; choose another default model first");
          if (body.display_name !== undefined) entry.display_name = body.display_name || null;
          if (body.enabled !== undefined) entry.enabled = body.enabled;
          if (body.thinking !== undefined) {
            if (body.thinking && !customThinking(c)) return err(422, "thinking options can be set only on self-hosted OpenAI-compatible connections");
            entry.thinking = body.thinking;
          }
          if (body.default === true) {
            if (!entry.enabled) return err(409, "the default model must be enabled");
            c.model = id;
          }
          return ok(listView(c));
        }
        if (req.method === "DELETE") {
          if (c.model === id) return err(409, "this is the connection's default model; choose another default model first");
          c.models = c.models.filter((x) => x.model_id !== id);
          return ok(listView(c));
        }
      }
      if (!r[2] && req.method === "PATCH") {
        if (body.name !== undefined) c.name = body.name;
        if (body.model !== undefined) {
          c.model = body.model;
          settle(c);
        }
        if (body.api_key !== undefined) c.key = body.api_key || null;
        if (body.is_default) for (const x of state.connections.values()) if (x.owner === user && x.capability === c.capability) x.is_default = x.id === c.id;
        return ok({ connection: connJson(c) });
      }
      if (!r[2] && req.method === "DELETE") {
        state.connections.delete(c.id);
        return ok(undefined, 204);
      }
    }
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
        const asks = body.save_note !== true && body.invoke_agent !== false;
        post(key, { kind: "message", actor: user, text: body.text, audience: asks ? "agent" : "room" });
        if (asks) post(key, { kind: "agent", actor: "agent", text: `(agent) re: ${body.text}` });
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
          if (state.attached.has(space.id)) return err(409, "this Space has an attached runner; detach it first");
          space.runnerToken = `secret-runner-token-${nextId++}`;
          return ok({ token: space.runnerToken });
        }
        space.runnerToken = null;
        return ok({});
      }
      if (rest === "/runner/attach-if-empty" && req.method === "POST") {
        if (!state.attachIfEmpty || state.missingEmptyAttachRoute) return err(404, "unknown route");
        if (!owner) return err(403, "only the owner manages the runner");
        const runner = state.runners.get(body?.runner_id ?? "");
        if (!visibleRunner(runner, user)) return err(404, "no such runner");
        state.beforeEmptyAttach?.(space.id);
        if (space.runnerToken || state.attached.has(space.id)) return err(409, "this Space already has a runner");
        state.attached.set(space.id, runner!.id);
        return ok({ runner: spaceRunnerJson(space) });
      }
      if (rest === "/runner") {
        if (req.method === "GET") return ok({ runner: spaceRunnerJson(space) });
        if (!owner) return err(403, "only the owner manages the runner");
        if (req.method === "PUT") {
          const runner = state.runners.get(body?.runner_id ?? "");
          if (!visibleRunner(runner, user)) return err(404, "no such runner");
          if (space.runnerToken) return err(409, "this Space has a dedicated runner token; revoke it first");
          state.attached.set(space.id, runner!.id);
          return ok({ runner: spaceRunnerJson(space) });
        }
        if (req.method === "DELETE") {
          state.attached.delete(space.id);
          space.runnerToken = null;
          return ok(undefined, 204);
        }
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
          if (!space.runnerToken && !state.attached.has(space.id)) return err(409, "this Space has no runner");
          // One Demo is one activity (source "demo"), as the server tags it.
          const activity: Activity = { id: `demo-${nextId}`, name: "Demo", source: "demo" };
          post(key, { kind: "demo", actor: user, text: "Demo: workspace.list on the Space's runner", activity });
          post(key, { kind: "activity", actor: user, text: "", activity: { ...activity, phase: "started" } });
          post(key, { kind: "tool_call", actor: user, text: 'workspace.list {"path":"."}', activity });
          post(key, { kind: "tool_result", actor: "runner", text: "workspace.list ok\nREADME.md\nsrc/\n", activity });
          post(key, { kind: "activity", actor: user, text: "", activity: { ...activity, phase: "completed" } });
          return ok({}, 202);
        }
      }
      return err(404, "not found");
    }
    return err(404, "not found");
  }

  /**
   * Answers requests to `path` made with `token` after `ms` milliseconds,
   * through the stub's own `delay`: on a manual clock (`open(…, { clock: {} })`)
   * that answer comes only when the test moves the clock, never by wall time.
   */
  const slow = new Map<string, { ms: number; arrived: number }>();
  const answerLate = (path: string, token: string, ms: number) => {
    const entry = { ms, arrived: 0 };
    slow.set(`${path} ${token}`, entry);
    /** How many such requests have reached the server. */
    return { arrived: () => entry.arrived };
  };

  const network: Record<string, (req: Req) => unknown> = {};
  for (const method of ["GET", "POST", "PUT", "PATCH", "DELETE"]) {
    network[`${method} ${BASE}/v1/*`] = (req: Req) => {
      const u = new URL(req.url);
      const token = (header(req, "authorization") ?? "").replace(/^Bearer /, "");
      const late = slow.get(`${u.pathname}${u.search} ${token}`) ?? slow.get(`${u.pathname} ${token}`);
      if (late) {
        late.arrived++;
        return { ...(handle(req) as object), delay: late.ms };
      }
      const answer = handle(req);
      const delay = state.delays.get(u.pathname + u.search) ?? state.delays.get(u.pathname);
      return delay ? new Promise((resolve) => setTimeout(() => resolve(answer), delay)) : answer;
    };
  }
  /** Adds a connection directly (as if its owner had set it up). */
  const addConnection = (owner: string, provider_id: string, name: string, model: string | null, share: Share = { visibility: "personal", space_ids: [] }) => {
    const provider = CATALOG.providers.find((p) => p.id === provider_id)!;
    const first = ![...state.connections.values()].some((x) => x.owner === owner && x.capability === provider.capability);
    const c: Conn = { id: `pc${nextId++}`, owner, name, provider_id, base_url: provider.default_base_url ?? null, model, is_default: first, key: provider.requires_key ? "sk-fixture-not-real" : null, deviceToken: null, capability: provider.capability, share, models: [] };
    settle(c);
    state.connections.set(c.id, c);
    return c;
  };
  return { state, network, answerLate, addSpace, addRoom, addRunner, addConnection, post, react, posts: () => state.log.filter((l) => l.method !== "GET") };
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
