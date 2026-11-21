// The JSON HTTP contract with a Metro server, and nothing else: no state, no
// timers, no UI. Every failure is an ApiError whose `kind` the store uses to
// decide between retrying, stopping, and what to tell the user.

/** `parentRoomId`: set on a thread an agent delegated from that room. */
export type Room = { id: string; name: string; parentRoomId?: string };
/** Who a message addresses: the Space's agent, or a member by their stable user id. */
export type Mention = { kind: "agent" } | { kind: "user"; id: string };
/** For messages: who it was addressed to ("room" = nobody in particular). */
export type Audience = "room" | "agent" | "users" | "mixed";
export type RoomEvent = { id: number; kind: string; actor: string; text: string; mentions?: Mention[]; audience?: Audience };
export type SpaceRole = "owner" | "member";
/** `runnerPaired`: a runner token is issued for the space. It says nothing about the runner being online. */
export type Space = { id: string; name: string; role: SpaceRole; runnerPaired: boolean };
export type Member = { userId: string; role: SpaceRole };
/** An agent run in a Space the user belongs to (GET /v1/runs, newest first). */
export type Run = { id: string; spaceId: string; roomId: string; status: string; actor: string; task: string };

export type ApiErrorKind =
  | "network" // no HTTP response: refused, DNS, TLS, offline
  | "timeout" // no response in time
  | "auth" // 401: the token is not valid (sign out)
  | "forbidden" // 403: valid token, but no access (e.g. removed from the space)
  | "not_found" // 404
  | "http" // any other non-2xx
  | "protocol" // 2xx but not the JSON the contract promises
  | "aborted"; // cancelled by us (disconnect); never shown

export class ApiError extends Error {
  readonly kind: ApiErrorKind;
  readonly status: number | null;
  constructor(kind: ApiErrorKind, message: string, status: number | null = null) {
    super(message);
    this.name = "ApiError";
    this.kind = kind;
    this.status = status;
  }
  /** Worth retrying with backoff (the server may come back). */
  get transient(): boolean {
    return (
      this.kind === "network" ||
      this.kind === "timeout" ||
      (this.kind === "http" && this.status !== null && (this.status >= 500 || this.status === 429))
    );
  }
}

export type ClientOptions = {
  baseUrl: string;
  /** Read per request; the token itself lives only in memory. */
  token: () => string;
  timeoutMs?: number;
  /** Aborts every request of this client (disconnect). */
  signal?: AbortSignal;
};

export type Client = ReturnType<typeof createClient>;

export function createClient(options: ClientOptions) {
  const timeoutMs = options.timeoutMs ?? 10_000;

  async function request(method: "GET" | "POST" | "PUT" | "DELETE", path: string, body?: unknown): Promise<unknown> {
    const url = options.baseUrl + path;
    const timeout = new AbortController();
    const timer = setTimeout(() => timeout.abort(), timeoutMs);
    const onOuterAbort = () => timeout.abort();
    options.signal?.addEventListener("abort", onOuterAbort);
    const headers: Record<string, string> = {
      Accept: "application/json",
      Authorization: `Bearer ${options.token()}`,
    };
    if (body !== undefined) headers["Content-Type"] = "application/json";

    let response: Response;
    try {
      if (options.signal?.aborted) throw new ApiError("aborted", "Cancelled");
      response = await fetch(url, {
        method,
        headers,
        body: body === undefined ? undefined : JSON.stringify(body),
        signal: timeout.signal,
        redirect: "error", // never forward the bearer token to another location
      });
    } catch (error) {
      if (error instanceof ApiError) throw error;
      if (options.signal?.aborted) throw new ApiError("aborted", "Cancelled");
      if (timeout.signal.aborted) {
        throw new ApiError("timeout", `${options.baseUrl} did not answer ${method} ${path} within ${Math.round(timeoutMs / 1000)}s.`);
      }
      const detail = String((error as Error)?.message ?? error).replace(/^Network request failed:\s*/i, "");
      throw new ApiError("network", `Cannot reach ${options.baseUrl} (${detail}). Is the server running?`);
    } finally {
      clearTimeout(timer);
      options.signal?.removeEventListener("abort", onOuterAbort);
    }

    if (response.status === 401) {
      throw new ApiError("auth", "The server rejected the access token (HTTP 401). Check the token and connect again.", 401);
    }
    if (response.status === 403) {
      const detail = await safeSnippet(response);
      throw new ApiError("forbidden", `You don't have access to that (HTTP 403)${detail ? `: ${detail}` : ""}.`, 403);
    }
    if (!response.ok) {
      const detail = await safeSnippet(response);
      throw new ApiError(
        response.status === 404 ? "not_found" : "http",
        `${method} ${path} failed with HTTP ${response.status}${detail ? `: ${detail}` : ""}`,
        response.status,
      );
    }
    const text = await response.text();
    if (!text.trim()) return null;
    try {
      return JSON.parse(text);
    } catch {
      throw new ApiError("protocol", `${method} ${path} answered with something that is not JSON.`, response.status);
    }
  }

  const sp = (spaceId: string) => `/v1/spaces/${encodeURIComponent(spaceId)}`;
  const room = (spaceId: string, roomId: string) => `${sp(spaceId)}/rooms/${encodeURIComponent(roomId)}`;

  return {
    baseUrl: options.baseUrl,

    async me(): Promise<{ id: string }> {
      const data = await request("GET", "/v1/me");
      if (!isObject(data) || (typeof data.id !== "string" && typeof data.id !== "number")) {
        throw new ApiError("protocol", "GET /v1/me did not return { id }.");
      }
      return { id: String(data.id) };
    },

    async listSpaces(): Promise<Space[]> {
      const data = await request("GET", "/v1/spaces");
      const spaces = isObject(data) ? data.spaces : undefined;
      if (!Array.isArray(spaces)) throw new ApiError("protocol", "GET /v1/spaces did not return { spaces: [...] }.");
      return spaces.filter(isSpace).map(toSpace);
    },

    async createSpace(name: string): Promise<Space> {
      const data = await request("POST", "/v1/spaces", { name });
      if (!isSpace(data)) throw new ApiError("protocol", "POST /v1/spaces did not return { id, name, role, runner_paired }.");
      return toSpace(data);
    },

    async listMembers(spaceId: string): Promise<Member[]> {
      const data = await request("GET", `${sp(spaceId)}/members`);
      const members = isObject(data) ? data.members : undefined;
      if (!Array.isArray(members)) throw new ApiError("protocol", "GET …/members did not return { members: [...] }.");
      return members
        .filter((m): m is { user_id: string | number; role: SpaceRole } => isObject(m) && (typeof m.user_id === "string" || typeof m.user_id === "number") && (m.role === "owner" || m.role === "member"))
        .map((m) => ({ userId: String(m.user_id), role: m.role }));
    },

    async addMember(spaceId: string, userId: string): Promise<void> {
      await request("POST", `${sp(spaceId)}/members`, { user_id: userId });
    },

    async removeMember(spaceId: string, userId: string): Promise<void> {
      await request("DELETE", `${sp(spaceId)}/members`, { user_id: userId });
    },

    /** Issues (or rotates) the space's runner token. The caller must not keep it longer than it is shown. */
    async issueRunnerToken(spaceId: string): Promise<string> {
      const data = await request("POST", `${sp(spaceId)}/runner-token`, {});
      if (!isObject(data) || typeof data.token !== "string" || !data.token) {
        throw new ApiError("protocol", "POST …/runner-token did not return { token }.");
      }
      return data.token;
    },

    async revokeRunnerToken(spaceId: string): Promise<void> {
      await request("DELETE", `${sp(spaceId)}/runner-token`);
    },

    async listRooms(spaceId: string): Promise<Room[]> {
      const data = await request("GET", `${sp(spaceId)}/rooms`);
      const rooms = isObject(data) ? data.rooms : undefined;
      if (!Array.isArray(rooms)) throw new ApiError("protocol", "GET …/rooms did not return { rooms: [...] }.");
      return rooms.filter(isRoom).map(toRoom);
    },

    async createRoom(spaceId: string, name: string): Promise<Room> {
      const data = await request("POST", `${sp(spaceId)}/rooms`, { name });
      if (!isRoom(data)) throw new ApiError("protocol", "POST …/rooms did not return { id, name }.");
      return toRoom(data);
    },

    async listEvents(spaceId: string, roomId: string, after: number): Promise<RoomEvent[]> {
      const path = `${room(spaceId, roomId)}/events?after=${encodeURIComponent(String(after))}`;
      const data = await request("GET", path);
      const events = isObject(data) ? data.events : undefined;
      if (!Array.isArray(events)) throw new ApiError("protocol", "GET …/events did not return { events: [...] }.");
      return events.filter(isEvent).map(toEvent);
    },

    async sendMessage(spaceId: string, roomId: string, text: string, invokeAgent: boolean, mentions?: Mention[]): Promise<void> {
      const body: Record<string, unknown> = { text, invoke_agent: invokeAgent };
      if (mentions && mentions.length > 0) body.mentions = mentions;
      await request("POST", `${room(spaceId, roomId)}/messages`, body);
    },

    async runDemo(spaceId: string, roomId: string): Promise<void> {
      await request("POST", `${room(spaceId, roomId)}/demo`, {});
    },

    /** The user's personal Chat, outside every Space. */
    async listChatEvents(after: number): Promise<RoomEvent[]> {
      const data = await request("GET", `/v1/chat/events?after=${encodeURIComponent(String(after))}`);
      const events = isObject(data) ? data.events : undefined;
      if (!Array.isArray(events)) throw new ApiError("protocol", "GET /v1/chat/events did not return { events: [...] }.");
      return events.filter(isEvent).map(toEvent);
    },

    async sendChatMessage(text: string, invokeAgent: boolean): Promise<void> {
      await request("POST", "/v1/chat/messages", { text, invoke_agent: invokeAgent });
    },

    async listRuns(): Promise<Run[]> {
      const data = await request("GET", "/v1/runs");
      const runs = isObject(data) ? data.runs : undefined;
      if (!Array.isArray(runs)) throw new ApiError("protocol", "GET /v1/runs did not return { runs: [...] }.");
      return runs.filter(isRun).map((r) => ({
        id: String(r.id),
        spaceId: String(r.space_id),
        roomId: String(r.room_id),
        status: r.status,
        actor: r.actor,
        task: r.task,
      }));
    },

    /**
     * Adds or removes the user's reaction on an event. `scope` is the Chat or
     * a room; the server echoes it as a `reaction` event in the same stream.
     */
    async react(scope: { chat: true } | { spaceId: string; roomId: string }, eventId: number, emoji: string, present: boolean): Promise<void> {
      const base = "chat" in scope ? "/v1/chat" : room(scope.spaceId, scope.roomId);
      await request(present ? "PUT" : "DELETE", `${base}/events/${encodeURIComponent(String(eventId))}/reactions`, { emoji });
    },

    async stopRun(runId: string): Promise<void> {
      await request("POST", `/v1/runs/${encodeURIComponent(runId)}/stop`, {});
    },
  };
}

function isObject(v: unknown): v is Record<string, unknown> {
  return typeof v === "object" && v !== null && !Array.isArray(v);
}

function isSpace(v: unknown): v is { id: string | number; name: string; role: SpaceRole; runner_paired: boolean } {
  return (
    isObject(v) &&
    (typeof v.id === "string" || typeof v.id === "number") &&
    typeof v.name === "string" &&
    (v.role === "owner" || v.role === "member") &&
    typeof v.runner_paired === "boolean"
  );
}

function toSpace(v: { id: string | number; name: string; role: SpaceRole; runner_paired: boolean }): Space {
  return { id: String(v.id), name: v.name, role: v.role, runnerPaired: v.runner_paired };
}

const isId = (v: unknown): v is string | number => typeof v === "string" || typeof v === "number";

function isRoom(v: unknown): v is { id: string | number; name: string; parent_room_id?: string | number | null } {
  return isObject(v) && isId(v.id) && typeof v.name === "string" && (v.parent_room_id == null || isId(v.parent_room_id));
}

function toRoom(v: { id: string | number; name: string; parent_room_id?: string | number | null }): Room {
  return v.parent_room_id == null ? { id: String(v.id), name: v.name } : { id: String(v.id), name: v.name, parentRoomId: String(v.parent_room_id) };
}

function isRun(v: unknown): v is { id: string | number; space_id: string | number; room_id: string | number; status: string; actor: string; task: string } {
  return (
    isObject(v) &&
    isId(v.id) &&
    isId(v.space_id) &&
    isId(v.room_id) &&
    typeof v.status === "string" &&
    typeof v.actor === "string" &&
    typeof v.task === "string"
  );
}

function toEvent(v: RoomEvent): RoomEvent {
  const raw = v as unknown as Record<string, unknown>;
  const event: RoomEvent = { id: v.id, kind: v.kind, actor: v.actor, text: v.text };
  if (Array.isArray(raw.mentions)) {
    const mentions: Mention[] = [];
    for (const m of raw.mentions) {
      if (isObject(m) && m.kind === "agent") mentions.push({ kind: "agent" });
      else if (isObject(m) && m.kind === "user" && isId(m.id)) mentions.push({ kind: "user", id: String(m.id) });
    }
    if (mentions.length) event.mentions = mentions;
  }
  if (raw.audience === "room" || raw.audience === "agent" || raw.audience === "users" || raw.audience === "mixed") event.audience = raw.audience;
  return event;
}

function isEvent(v: unknown): v is RoomEvent {
  return (
    isObject(v) &&
    typeof v.id === "number" &&
    Number.isFinite(v.id) &&
    typeof v.kind === "string" &&
    typeof v.actor === "string" &&
    typeof v.text === "string"
  );
}

async function safeSnippet(response: Response): Promise<string> {
  try {
    const text = (await response.text()).trim();
    if (!text) return "";
    try {
      const json = JSON.parse(text);
      if (isObject(json)) {
        const msg = json.error ?? json.message;
        if (typeof msg === "string") return msg.slice(0, 200);
        if (isObject(msg) && typeof msg.message === "string") return msg.message.slice(0, 200);
      }
    } catch {
      /* not JSON */
    }
    return text.slice(0, 200);
  } catch {
    return "";
  }
}

/** The JSON payload of a `reaction` event's text. */
export type ReactionChange = { targetEventId: number; emoji: string; present: boolean; reactorKind: "user" | "agent"; reactorId: string };

export function parseReaction(event: RoomEvent): ReactionChange | null {
  if (event.kind !== "reaction") return null;
  try {
    const v = JSON.parse(event.text);
    if (
      isObject(v) &&
      typeof v.target_event_id === "number" &&
      Number.isFinite(v.target_event_id) &&
      typeof v.emoji === "string" &&
      v.emoji.length > 0 &&
      v.emoji.length <= 16 &&
      typeof v.present === "boolean" &&
      (v.reactor_kind === "user" || v.reactor_kind === "agent") &&
      isId(v.reactor_id)
    ) {
      return { targetEventId: v.target_event_id, emoji: v.emoji, present: v.present, reactorKind: v.reactor_kind, reactorId: String(v.reactor_id) };
    }
  } catch {
    /* malformed: ignore */
  }
  return null;
}
