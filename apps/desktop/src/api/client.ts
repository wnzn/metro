// The JSON HTTP contract with a Metro server, and nothing else: no state, no
// timers, no UI. Every failure is an ApiError whose `kind` the store uses to
// decide between retrying, stopping, and what to tell the user.

import { discoveryApi } from "./discovery";
import { libraryApi } from "./library";
import { monitoringApi } from "./monitoring";
import { pagesApi } from "./pages";
import type { PageDraftEvent } from "./pages";
import { profileApi } from "./profile";
import { spacesApi } from "./spaces";
import type { RawFn } from "./profile";

/** `parentRoomId`: set on a thread an agent delegated from that room. */
export type Room = { id: string; name: string; parentRoomId?: string };
/**
 * Who a message addresses: the Space's agent, or a member by their stable
 * user id. An agent mention with `agentId` names one of the Space's agents
 * (`@handle`); without it, it is the generic `@agent` (the Space's default
 * agent, or the sender's classifier pick). `handle` is only the draft's text
 * for it (`@coder`), never sent. On received events only, an agent mention may say it wasn't typed:
 * `classifier` (the sender's opt-in System One judgement started the agent
 * from an unaddressed message) or `schedule` (the creator's scheduled
 * prompt). The server sets it; the app never sends it.
 */
export type Mention = { kind: "agent"; agentId?: string; handle?: string; addressedBy?: "classifier" | "schedule" } | { kind: "user"; id: string };
/** For messages: who it was addressed to ("room" = nobody in particular). */
export type Audience = "room" | "agent" | "users" | "mixed";
/** Where a turn's activity comes from: an agent run, or the deterministic Demo (no model). */
export type ActivitySource = "agent" | "demo";
/** A lifecycle step of one activity; only `kind: "activity"` events carry it. */
export type ActivityPhase = "started" | "completed" | "failed" | "cancelled" | "interrupted";
/**
 * Optional metadata tying an event to one agent turn (or one Demo). Every
 * event of the turn — its lifecycle (`kind: "activity"`), tool calls and
 * results — carries the same `id`, `name` and `source`.
 */
export type EventActivity = {
  id: string;
  name: string;
  source: ActivitySource;
  phase?: ActivityPhase;
  route?: ActivityRoute;
  /** The Space agent working this turn, on every event of it (absent in personal Chat, Demo and older history). */
  agent?: EventAgent;
  /** Only on a Space turn's `started` marker: how its agent was chosen ("Picked by classifier: Coder"). */
  pick?: string;
  /** The small room-visible proposal card, not the draft itself. */
  page_draft?: PageDraftEvent;
};
/** A Space agent as a run's events show it: enough for its name and avatar. */
export type EventAgent = { id: string; handle: string; name: string; avatar_version?: string; color: string };

/** Where a turn's model came from (`activity.route` on its `started` marker). */
export type RouteSource = "override" | "case" | "routing_default" | "default" | "environment" | "inherited";
/** How the turn came to be addressed to the agent. */
export type AddressedBy = "explicit" | "classifier" | "schedule" | "thread";
/**
 * The route a turn took, as the server recorded it: no prompts, no
 * credentials, no connection ids shown. `reason` is the server's short,
 * public sentence (never a model's hidden reasoning).
 */
export type ActivityRoute = {
  source: RouteSource;
  addressedBy: AddressedBy;
  caseLabel: string | null;
  providerId: string | null;
  modelId: string | null;
  reason: string;
};
export type RoomEvent = {
  id: number;
  kind: string;
  /** The author's display name as it was written. */
  actor: string;
  text: string;
  mentions?: Mention[];
  audience?: Audience;
  activity?: EventActivity;
  /** A person's stable id; absent for the agent, runner and system. */
  actorId?: string;
  /** When the server stored it (RFC 3339, UTC). Absent when the server doesn't know: never guessed. */
  createdAt?: string;
  /** Library files sent with a message: metadata only, never bytes or URLs. */
  attachments?: EventAttachment[];
};
/**
 * A Library file attached to a message, or a tombstone for one deleted
 * since (no name, nothing to open). Which library holds a file is the
 * message's own place (the sender's personal library in the Chat, the
 * Space's in its rooms), never a field of the file. Opening it goes through
 * the Library's authenticated content read, which checks access again.
 */
export type AttachedFile = { removed: false; assetId: string; name: string; mimeType: string; kind: string | null; sizeBytes: number };
export type EventAttachment = AttachedFile | { removed: true };
/** Most files one message may carry (the server's limit too). */
export const MAX_MESSAGE_ATTACHMENTS = 10;
export type SpaceRole = "owner" | "member";
/** `runnerPaired`: a runner token is issued for the space. It says nothing about the runner being online. */
export type Space = { id: string; name: string; role: SpaceRole; runnerPaired: boolean };
export type Member = { userId: string; role: SpaceRole };
/** An agent run in a Space the user belongs to (GET /v1/runs, newest first). */
export type Run = { id: string; spaceId: string; roomId: string; status: string; actor: string; task: string };

/** Who is signed in. `isOrgAdmin`: may create and manage organization runners. */
export type Me = { id: string; isOrgAdmin: boolean };

/** A registered runner the user can see (GET /v1/runners). */
export type RunnerVisibility = "personal" | "organization";
export type RunnerPresence = {
  seenAt: number | null; online: boolean; version: string | null;
  install: "managed" | "unmanaged" | "container" | "desktop" | null; selfUpdate: boolean;
};
export type RunnerPending = { target: string; requestedAt: number };
export type RunnerUpdate = {
  latest: string | null; available: boolean; action: "none" | "self_update" | "container" | "with_app" | "install_script";
  containerImage: string | null; pending: RunnerPending | null;
  lastResult: { target: string; state: "updated" | "failed"; code: string | null; at: number } | null;
};
export type RegisteredRunner = {
  id: string; name: string; visibility: RunnerVisibility; canManage: boolean;
  presence?: RunnerPresence; update?: RunnerUpdate;
};

/**
 * Who serves a Space (GET /v1/spaces/{sid}/runner): a registered runner, the
 * Space's own dedicated (legacy) token, or nobody. Attached says nothing
 * about the runner being online.
 */
export type SpaceRunner = (
  | { kind: "registered"; id: string; name: string; visibility: RunnerVisibility }
  | { kind: "dedicated"; name: string }
) & { assignmentRevision?: string };

/** Where a provider's inference runs: in the user's own metro-connector, or from the server. */
export type ProviderExecution = "device" | "server";

/**
 * What a connection is for, fixed by its adapter: agent answers (`chat`),
 * typed System One decisions (`system_one`) or vectors for search
 * (`embeddings`). A connection is never used for another capability, and
 * each capability has its own default.
 */
export type Capability = "chat" | "system_one" | "embeddings";
export const CAPABILITIES: readonly Capability[] = ["chat", "system_one", "embeddings"];

/** One adapter the server's build implements (GET /v1/provider-catalog). */
export type CatalogProvider = {
  id: string;
  name: string;
  requiresKey: boolean;
  allowCustomUrl: boolean;
  defaultBaseUrl: string | null;
  execution: ProviderExecution;
  description: string | null;
  capability: Capability;
  /** The adapter can list its models; otherwise the model ID is entered by hand. */
  listsModels: boolean;
};
export type ProviderCatalog = { providers: CatalogProvider[]; allowedBaseUrls: string[] };

/** Who besides its owner may use a connection. Personal (nobody) is the default. */
export type ShareVisibility = "personal" | "spaces" | "organization";
/**
 * A connection's share (GET/PUT /v1/providers/{id}/sharing, owner only).
 * Grants use, never the credential. `mode`: `owner_connection` (others run
 * on this connection, which its owner can observe and pays for) or `preset`
 * (a subscription sign-in: others get only the model name and run it on
 * their own sign-in or key).
 */
export type Sharing = { visibility: ShareVisibility; spaceIds: string[]; mode?: "owner_connection" | "preset" };

/** A personal model connection (GET /v1/providers). The key is never returned. */
export type ProviderConnection = {
  id: string;
  name: string;
  providerId: string;
  baseUrl: string | null;
  model: string | null;
  isDefault: boolean;
  hasKey: boolean;
  /** null: the server didn't say; the catalog entry of its provider decides. */
  capability: Capability | null;
  /** null: the server didn't say (an older server shares nothing). */
  visibility: ShareVisibility | null;
  /** The connection's own model list (owner only); empty on an older server. */
  models: ConnectionModel[];
  /** Its models take thinking options set by the owner (self-hosted OpenAI-compatible endpoints). */
  customThinking: boolean;
  /** Why the model list couldn't be fetched when it was created (`fetch_models`). */
  modelsError?: string | null;
};

/** How a chosen thinking level is sent to a self-hosted model. */
export type EffortWire = "none" | "reasoning_effort" | "reasoning";
/** A self-hosted model's thinking options, set by the connection's owner. */
export type ModelThinking = { efforts: string[]; wire: EffortWire };

/** One model of a connection (GET /v1/providers/{id}/model-list). */
export type ConnectionModel = {
  modelId: string;
  /** What pickers show: the owner's name, else the provider's, else the id. */
  displayName: string;
  customName: string | null;
  listedName: string | null;
  enabled: boolean;
  /** false: the provider's last listing no longer had it. */
  available: boolean;
  source: "listed" | "manual";
  position: number;
  /** The connection's default model. */
  isDefault: boolean;
  thinking: ModelThinking | null;
  reasoningEfforts: string[];
};

/** A connection's model list as the server answers a change to it. */
export type ConnectionModelList = { connectionId: string; defaultModel: string | null; customThinking: boolean; models: ConnectionModel[] };

/** What a refresh of a connection's model list did. */
export type ModelRefresh = { models: string[]; added: number; unavailable: number; connection: ProviderConnection | null };

export type NewConnectionModel = { model_id: string; display_name?: string; thinking?: ModelThinking };
export type ConnectionModelPatch = { model_id: string; display_name?: string | null; enabled?: boolean; thinking?: ModelThinking | null; position?: number; default?: true };

/** A chat model in the grouped picker (GET /v1/models/available `chat_providers`). */
export type PickerModel = { id: string; displayName: string; reasoningEfforts: string[]; isDefault: boolean; available: boolean };
/** One connection in the grouped picker; `connectionId` null is the operator's model (send no connection). */
export type PickerConnection = {
  connectionId: string | null;
  name: string;
  owned: boolean;
  ownerId: string | null;
  visibility: ShareVisibility | null;
  runsOn: string;
  billing: string;
  ready: boolean | null;
  isDefault: boolean;
  defaultModel: string | null;
  operator: boolean;
  models: PickerModel[];
};
export type PickerProvider = { providerId: string; providerName: string; connections: PickerConnection[] };
/** GET /v1/models/available in full. */
export type AvailableCatalog = { models: AvailableModel[]; chatProviders: PickerProvider[] | null; operatorModel: { modelId: string; label: string } | null };

/**
 * A model the signed-in user may pick (GET /v1/models/available): one of
 * their own connections, or one shared with them. Never carries a key, a
 * sign-in or a device token.
 */
export type AvailableModel = {
  connectionId: string;
  name: string;
  providerId: string;
  model: string | null;
  capability: Capability;
  visibility: ShareVisibility;
  /** The signed-in user owns it. */
  owned: boolean;
  /** The stable id of whoever owns it. */
  ownerId: string | null;
  execution: ProviderExecution;
  /**
   * `your_connection`; `owner_connection` (the owner's endpoint, key or
   * device: they can see these requests and may pay for them); or
   * `your_own_sign_in` (a shared preset run on your own connection).
   */
  runsOn: "your_connection" | "owner_connection" | "your_own_sign_in";
  billing: "you" | "owner";
  /** A preset: whether you have a connection of your own it can run on (null: not a preset). */
  ready: boolean | null;
  /** Your default for this capability (own connections only). */
  isDefault: boolean;
  /** An embeddings adapter that embeds documents, images, audio and video itself (never a text-only `/v1/embeddings`). */
  mediaInputs?: boolean;
  /** Absent on older servers (pinned model only); an empty list explicitly offers no chat model. */
  chatModels?: { id: string; reasoningEfforts: string[]; displayName?: string; isDefault?: boolean; available?: boolean }[];
};

/** Snapshot of the user's explicit choice for this agent turn; does not change the connection default. */
export type TurnModelChoice = Readonly<{ connectionId: string; modelId?: string; reasoningEffort?: string }>;

function turnModelBody(choice?: TurnModelChoice) {
  if (!choice) return {};
  return {
    provider_connection_id: choice.connectionId,
    ...(choice.modelId ? { model_id: choice.modelId } : {}),
    ...(choice.reasoningEffort ? { reasoning_effort: choice.reasoningEffort } : {}),
  };
}

/** One named use case and the chat connection that answers it. */
export type RoutingCase = { key: string; label: string; description: string; connectionId: string | null };
/** The signed-in user's model routing (GET/PUT /v1/model-routing). Everything is off by default. */
export type ModelRouting = {
  /** Use-case routing: System One picks which chat connection answers. */
  enabled: boolean;
  /** Opt-in: System One judges whether an unaddressed message of mine asks the agent. */
  inferAddressing: boolean;
  classifierId: string | null;
  embeddingId: string | null;
  /** Used when System One returns no match or a score below the threshold; a classifier failure calls no model (null: my default chat connection). */
  defaultChatId: string | null;
  minDirectedness: number;
  minRouteProbability: number;
  cases: RoutingCase[];
};

/** The transport other API modules (src/api/discovery.ts) build on. */
export type RequestFn = (method: "GET" | "POST" | "PUT" | "PATCH" | "DELETE", path: string, body?: unknown, signal?: AbortSignal) => Promise<unknown>;

/** A device connection's metro-connector (GET /v1/providers/{id}/device-token). */
export type DeviceStatus = { paired: boolean; online: boolean; lastSeenSecs: number | null };

export type NewProviderConnection = {
  name: string;
  provider_id: string;
  base_url?: string;
  api_key?: string;
  model?: string;
  /** List the provider's models right after saving; every listed model starts enabled. */
  fetch_models?: boolean;
  /** Models added by hand (self-hosted endpoints, or any provider). */
  models?: NewConnectionModel[];
};
export type ProviderPatch = { name?: string; model?: string; is_default?: boolean; api_key?: string };

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
  constructor(kind: ApiErrorKind, message: string, status: number | null = null, readonly details?: unknown) {
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

  const request: RequestFn = async (method, path, body, signal) => {
    const url = options.baseUrl + path;
    const timeout = new AbortController();
    const timer = setTimeout(() => timeout.abort(), timeoutMs);
    const onOuterAbort = () => timeout.abort();
    options.signal?.addEventListener("abort", onOuterAbort);
    signal?.addEventListener("abort", onOuterAbort);
    const cancelled = () => options.signal?.aborted || signal?.aborted;
    const checkCancelled = () => { if (cancelled()) throw new ApiError("aborted", "Cancelled"); };
    const headers: Record<string, string> = {
      Accept: "application/json",
      Authorization: `Bearer ${options.token()}`,
    };
    if (body !== undefined) headers["Content-Type"] = "application/json";

    try {
      checkCancelled();
      const response = await fetch(url, {
        method,
        headers,
        body: body === undefined ? undefined : JSON.stringify(body),
        signal: timeout.signal,
        redirect: "error", // never forward the bearer token to another location
      });
      checkCancelled();
      await checkStatus(response, method, path, path.startsWith("/v1/pages"));
      const text = await response.text();
      checkCancelled();
      if (!text.trim()) return null;
      try {
        return JSON.parse(text);
      } catch {
        throw new ApiError("protocol", `${method} ${path} answered with something that is not JSON.`, response.status);
      }
    } catch (error) {
      checkCancelled();
      if (error instanceof ApiError) throw error;
      if (timeout.signal.aborted) {
        throw new ApiError("timeout", `${options.baseUrl} did not answer ${method} ${path} within ${Math.round(timeoutMs / 1000)}s.`);
      }
      const detail = String((error as Error)?.message ?? error).replace(/^Network request failed:\s*/i, "");
      throw new ApiError("network", `Cannot reach ${options.baseUrl} (${detail}). Is the server running?`);
    } finally {
      clearTimeout(timer);
      options.signal?.removeEventListener("abort", onOuterAbort);
      signal?.removeEventListener("abort", onOuterAbort);
    }
  };

  /**
   * Raw bytes both ways (library uploads and file content): the same
   * timeout, abort and error mapping as `request`, the bearer only in the
   * Authorization header, never in a URL, and never sent to a redirect.
   */
  const binary: RawFn = async (method, path, init) => {
    const url = options.baseUrl + path;
    const limit = init.timeoutMs ?? timeoutMs;
    const timeout = new AbortController();
    const timer = setTimeout(() => timeout.abort(), limit);
    const onAbort = () => timeout.abort();
    options.signal?.addEventListener("abort", onAbort);
    init.signal?.addEventListener("abort", onAbort);
    const cancelled = () => options.signal?.aborted === true || init.signal?.aborted === true;
    const headers: Record<string, string> = { Accept: init.accept ?? "*/*", Authorization: `Bearer ${options.token()}` };
    if (init.body !== undefined) headers["Content-Type"] = init.contentType ?? "application/octet-stream";
    // A single byte range (`bytes=A-B`), only for reads; never anything else in this header.
    if (init.range !== undefined && method === "GET" && /^bytes=\d+-\d+$/.test(init.range)) headers.Range = init.range;
    try {
      if (cancelled()) throw new ApiError("aborted", "Cancelled");
      let response: Response;
      try {
        response = await fetch(url, { method, headers, body: init.body as BodyInit | undefined, signal: timeout.signal, redirect: "error" });
      } catch (error) {
        if (error instanceof ApiError) throw error;
        if (cancelled()) throw new ApiError("aborted", "Cancelled");
        if (timeout.signal.aborted) throw new ApiError("timeout", `${options.baseUrl} did not answer ${method} ${path} within ${Math.round(limit / 1000)}s.`);
        const detail = String((error as Error)?.message ?? error).replace(/^Network request failed:\s*/i, "");
        throw new ApiError("network", `Cannot reach ${options.baseUrl} (${detail}). Is the server running?`);
      }
      await checkStatus(response, method, path);
      let bytes: Uint8Array;
      try {
        bytes = new Uint8Array(await response.arrayBuffer());
      } catch (error) {
        if (cancelled()) throw new ApiError("aborted", "Cancelled");
        throw new ApiError("network", `The answer to ${method} ${path} was cut off (${String((error as Error)?.message ?? error)}).`);
      }
      if (cancelled()) throw new ApiError("aborted", "Cancelled");
      return { status: response.status, contentType: response.headers.get("content-type") ?? "", bytes, contentRange: response.headers.get("content-range") };
    } finally {
      clearTimeout(timer);
      options.signal?.removeEventListener("abort", onAbort);
      init.signal?.removeEventListener("abort", onAbort);
    }
  };

  const sp = (spaceId: string) => `/v1/spaces/${encodeURIComponent(spaceId)}`;
  const room = (spaceId: string, roomId: string) => `${sp(spaceId)}/rooms/${encodeURIComponent(roomId)}`;

  /** GET /v1/models/available in full: the flat list, the grouped chat picker and the operator's model, if it applies to me. */
  async function availableCatalog(scope?: string | null): Promise<AvailableCatalog> {
    const query = scope === null ? "" : scope === undefined ? "?scope=any" : `?space_id=${encodeURIComponent(scope)}`;
    const data = await request("GET", `/v1/models/available${query}`);
    const models = isObject(data) ? data.models : undefined;
    if (!isObject(data) || !Array.isArray(models)) throw new ApiError("protocol", "GET /v1/models/available did not return { models: [...] }.");
    const seen = new Set<string>();
    const out: AvailableModel[] = [];
    for (const m of models) {
      const model = toAvailableModel(m);
      if (model && !seen.has(model.connectionId)) {
        seen.add(model.connectionId);
        out.push(model);
      }
    }
    const op = isObject(data.operator_model) && typeof data.operator_model.model_id === "string" && data.operator_model.model_id ? data.operator_model : null;
    return {
      models: out,
      chatProviders: Array.isArray(data.chat_providers) ? data.chat_providers.flatMap(toPickerProvider) : null,
      operatorModel: op ? { modelId: String(op.model_id), label: typeof op.label === "string" && op.label ? op.label : "Server model (operator)" } : null,
    };
  }

  async function listRunnerInventory() {
    const data = await request("GET", "/v1/runners");
    if (!isObject(data) || !Array.isArray(data.runners)) {
      throw new ApiError("protocol", "GET /v1/runners did not return { runners: [...] }.");
    }
    return { runners: data.runners.filter(isRegisteredRunner).map(toRegisteredRunner), attachIfEmpty: data.attach_if_empty === true };
  }

  const isAssignmentRevision = (value: unknown): value is string =>
    typeof value === "string" && /^(0|[1-9][0-9]{0,18})$/.test(value);

  async function getSpaceRunnerAssignment(spaceId: string) {
    const data = await request("GET", `${sp(spaceId)}/runner`);
    if (!isObject(data) || !("runner" in data)) throw new ApiError("protocol", "GET …/runner did not return { runner }.");
    const runner = toSpaceRunner(data.runner);
    return {
      runner: runner && isAssignmentRevision(data.assignment_revision) ? { ...runner, assignmentRevision: data.assignment_revision } : runner,
      guardedTokenChanges: data.guarded_token_changes === true,
    };
  }

  return {
    baseUrl: options.baseUrl,
    // Search and scheduled prompts (src/api/discovery.ts), on the same transport.
    ...discoveryApi(request),
    // Documents & media (src/api/library.ts): JSON on `request`, bytes on `binary`.
    ...libraryApi(request, binary),
    ...pagesApi(request),
    // Profiles (src/api/profile.ts): JSON on `request`, pictures on `binary`.
    ...profileApi(request, binary),
    // A Space's settings, agents and the person's own preferences there (src/api/spaces.ts).
    ...spacesApi(request, binary),
    // Monitoring events (src/api/monitoring.ts): one Space, My Chat or the instance; never an aggregate.
    ...monitoringApi(request),

    async me(): Promise<Me> {
      const data = await request("GET", "/v1/me");
      if (!isObject(data) || (typeof data.id !== "string" && typeof data.id !== "number")) {
        throw new ApiError("protocol", "GET /v1/me did not return { id }.");
      }
      // An older server has no organization: nobody is its admin.
      return { id: String(data.id), isOrgAdmin: data.is_org_admin === true };
    },

    // ---- registered runners (personal or organization), reusable across Spaces

    listRunnerInventory,
    async listRunners(): Promise<RegisteredRunner[]> { return (await listRunnerInventory()).runners; },

    /** Registers a runner. The token is shown once; the caller must not keep it longer than it is shown. */
    async createRunner(name: string, visibility: RunnerVisibility): Promise<{ runner: RegisteredRunner; token: string }> {
      const data = await request("POST", "/v1/runners", { name, visibility });
      if (!isObject(data) || !isRegisteredRunner(data.runner) || !isToken(data.token)) {
        throw new ApiError("protocol", "POST /v1/runners did not return { runner, token }.");
      }
      return { runner: toRegisteredRunner(data.runner), token: data.token };
    },

    /** Rotates a registered runner's token: the old one stops working in every Space it serves. */
    async rotateRunnerToken(runnerId: string): Promise<string> {
      const data = await request("POST", `/v1/runners/${encodeURIComponent(runnerId)}/token`, {});
      if (!isObject(data) || !isToken(data.token)) throw new ApiError("protocol", "POST …/token did not return { token }.");
      return data.token;
    },

    async deleteRunner(runnerId: string): Promise<void> {
      await request("DELETE", `/v1/runners/${encodeURIComponent(runnerId)}`);
    },

    getSpaceRunnerAssignment,
    async getSpaceRunner(spaceId: string): Promise<SpaceRunner | null> { return (await getSpaceRunnerAssignment(spaceId)).runner; },

    async attachRunner(spaceId: string, runnerId: string): Promise<SpaceRunner | null> {
      const data = await request("PUT", `${sp(spaceId)}/runner`, { runner_id: runnerId });
      return isObject(data) && "runner" in data ? toSpaceRunner(data.runner) : null;
    },

    /** No fallback to PUT: on an older server this must fail without changing anything. */
    async attachRunnerIfEmpty(spaceId: string, runnerId: string) {
      const data = await request("POST", `${sp(spaceId)}/runner/attach-if-empty`, { runner_id: runnerId });
      const attached = isObject(data) && "runner" in data ? toSpaceRunner(data.runner) : null;
      if (attached?.kind !== "registered" || attached.id !== runnerId ||
          !isObject(data) || !isAssignmentRevision(data.assignment_revision)) {
        throw new ApiError("protocol", "The server did not confirm the exact runner assignment.");
      }
      return { ...attached, assignmentRevision: data.assignment_revision };
    },

    async requestRunnerUpdate(runnerId: string, version: string): Promise<RunnerPending> {
      const data = await request("POST", `/v1/runners/${encodeURIComponent(runnerId)}/update`, { version });
      const pending = isObject(data) ? toRunnerPending(data.pending) : null;
      if (!pending || pending.target !== version) throw new ApiError("protocol", "The server did not confirm the update request.");
      return pending;
    },

    /** Removes only the assignment the owner confirmed. Never falls back to DELETE. */
    async detachRunnerIfCurrent(spaceId: string, revision: string): Promise<void> {
      await request("POST", `${sp(spaceId)}/runner/detach-if-current`, { assignment_revision: revision });
    },

    // ---- personal model connections

    async providerCatalog(): Promise<ProviderCatalog> {
      const data = await request("GET", "/v1/provider-catalog");
      const providers = isObject(data) ? data.providers : undefined;
      if (!isObject(data) || !Array.isArray(providers)) throw new ApiError("protocol", "GET /v1/provider-catalog did not return { providers: [...] }.");
      const allowed = Array.isArray(data.allowed_base_urls) ? data.allowed_base_urls.filter((u): u is string => typeof u === "string") : [];
      return { providers: providers.filter(isCatalogProvider).map(toCatalogProvider), allowedBaseUrls: allowed };
    },

    async listProviders(): Promise<ProviderConnection[]> {
      const data = await request("GET", "/v1/providers");
      const list = isObject(data) ? data.connections : undefined;
      if (!Array.isArray(list)) throw new ApiError("protocol", "GET /v1/providers did not return { connections: [...] }.");
      return list.filter(isConnection).map(toConnection);
    },

    async createProvider(body: NewProviderConnection): Promise<ProviderConnection> {
      const data = await request("POST", "/v1/providers", body);
      const connection = isObject(data) && isObject(data.connection) ? data.connection : data;
      if (!isConnection(connection)) throw new ApiError("protocol", "POST /v1/providers did not return the connection.");
      return toConnection(connection);
    },

    /** A connection's own model list (owner only). */
    async connectionModels(id: string): Promise<ConnectionModelList> {
      return toModelList(await request("GET", `/v1/providers/${encodeURIComponent(id)}/model-list`), id, "GET …/model-list");
    },

    /** Adds a model by hand, enabled. */
    async addConnectionModel(id: string, model: NewConnectionModel): Promise<ConnectionModelList> {
      return toModelList(await request("POST", `/v1/providers/${encodeURIComponent(id)}/model-list`, model), id, "POST …/model-list");
    },

    /** Renames, switches on or off, sets thinking options or makes a model the default. */
    async patchConnectionModel(id: string, patch: ConnectionModelPatch): Promise<ConnectionModelList> {
      return toModelList(await request("PATCH", `/v1/providers/${encodeURIComponent(id)}/model-list`, patch), id, "PATCH …/model-list");
    },

    async removeConnectionModel(id: string, modelId: string): Promise<ConnectionModelList> {
      const data = await request("DELETE", `/v1/providers/${encodeURIComponent(id)}/model-list?model_id=${encodeURIComponent(modelId)}`);
      return toModelList(data, id, "DELETE …/model-list");
    },

    /** Lists the provider's models again and merges them into the connection's list. */
    async refreshConnectionModels(id: string): Promise<ModelRefresh> {
      const data = await request("POST", `/v1/providers/${encodeURIComponent(id)}/model-list/refresh`, {});
      const models = isObject(data) ? data.models : undefined;
      if (!isObject(data) || !Array.isArray(models)) throw new ApiError("protocol", "POST …/model-list/refresh did not return { models: [...] }.");
      return {
        models: [...new Set(models.filter((m): m is string => typeof m === "string" && m.trim() !== "").map((m) => m.trim()))],
        added: typeof data.added === "number" ? data.added : 0,
        unavailable: typeof data.unavailable === "number" ? data.unavailable : 0,
        connection: isConnection(data.connection) ? toConnection(data.connection) : null,
      };
    },

    async updateProvider(id: string, patch: ProviderPatch): Promise<void> {
      await request("PATCH", `/v1/providers/${encodeURIComponent(id)}`, patch);
    },

    async deleteProvider(id: string): Promise<void> {
      await request("DELETE", `/v1/providers/${encodeURIComponent(id)}`);
    },

    /** Asks the connection's endpoint (or the user's metro-connector) for its model IDs. */
    async listProviderModels(id: string): Promise<string[]> {
      const data = await request("POST", `/v1/providers/${encodeURIComponent(id)}/models`, {});
      const models = isObject(data) ? data.models : undefined;
      if (!Array.isArray(models)) throw new ApiError("protocol", "POST …/models did not return { models: [...] }.");
      return [...new Set(models.filter((m): m is string => typeof m === "string" && m.trim() !== "").map((m) => m.trim()))];
    },

    /** Whether a device token is issued (paired) and whether its metro-connector is polling (online). */
    async deviceStatus(id: string): Promise<DeviceStatus> {
      const data = await request("GET", `/v1/providers/${encodeURIComponent(id)}/device-token`);
      if (!isObject(data) || typeof data.paired !== "boolean") throw new ApiError("protocol", "GET …/device-token did not return { paired, online }.");
      const seen = typeof data.last_seen_secs === "number" && Number.isFinite(data.last_seen_secs) && data.last_seen_secs >= 0 ? data.last_seen_secs : null;
      return { paired: data.paired, online: data.paired && data.online === true, lastSeenSecs: seen };
    },

    /** Revokes the device token: the connector is unpaired and stops serving this connection. */
    async revokeDeviceToken(id: string): Promise<void> {
      await request("DELETE", `/v1/providers/${encodeURIComponent(id)}/device-token`);
    },

    /** Issues (or rotates) a device connection's token for the local metro-connector. Shown once. */
    async issueDeviceToken(id: string): Promise<string> {
      const data = await request("POST", `/v1/providers/${encodeURIComponent(id)}/device-token`, {});
      if (!isObject(data) || !isToken(data.token)) throw new ApiError("protocol", "POST …/device-token did not return { token }.");
      return data.token;
    },

    /** Who besides the owner may use a connection (owner only). */
    async getSharing(id: string): Promise<Sharing> {
      return toSharing(await request("GET", `/v1/providers/${encodeURIComponent(id)}/sharing`), "GET …/sharing");
    },

    /** Replaces a connection's share; `personal` revokes it. Answers with the share as stored. */
    async putSharing(id: string, sharing: Sharing): Promise<Sharing> {
      const body = { visibility: sharing.visibility, space_ids: sharing.visibility === "spaces" ? sharing.spaceIds : [] };
      const data = await request("PUT", `/v1/providers/${encodeURIComponent(id)}/sharing`, body);
      return data === null ? { visibility: body.visibility, spaceIds: body.space_ids } : toSharing(data, "PUT …/sharing");
    },

    /** null = personal Chat, a string = exact Space, omitted = Settings across all usable scopes. */
    async availableModels(scope?: string | null): Promise<AvailableModel[]> {
      return (await availableCatalog(scope)).models;
    },

    /** The same answer in full: the flat list, the grouped chat picker and the operator's model, if it applies to me. */
    availableCatalog,

    async getModelRouting(): Promise<ModelRouting> {
      return toModelRouting(await request("GET", "/v1/model-routing"), "GET /v1/model-routing");
    },

    /** Saves the whole routing. Answers with what the server stored. */
    async putModelRouting(routing: ModelRouting): Promise<ModelRouting> {
      const body = {
        enabled: routing.enabled,
        infer_addressing: routing.inferAddressing,
        classifier_id: routing.classifierId,
        embedding_id: routing.embeddingId,
        default_chat_id: routing.defaultChatId,
        min_directedness: routing.minDirectedness,
        min_route_probability: routing.minRouteProbability,
        cases: routing.cases.map((c) => ({ key: c.key, label: c.label, description: c.description, connection_id: c.connectionId })),
      };
      const data = await request("PUT", "/v1/model-routing", body);
      return data === null ? routing : toModelRouting(data, "PUT /v1/model-routing");
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

    /** Guarded only: issue to an empty Space or rotate the dedicated revision shown. */
    async changeDedicatedToken(spaceId: string, change: { kind: "issue" } | { kind: "rotate"; revision: string }) {
      const route = change.kind === "issue" ? "issue-if-empty" : "rotate-if-current";
      const data = await request("POST", `${sp(spaceId)}/runner-token/${route}`,
        change.kind === "issue" ? {} : { assignment_revision: change.revision });
      if (!isObject(data) || !isToken(data.token) || !isAssignmentRevision(data.assignment_revision)) {
        throw new ApiError("protocol", "The server did not confirm the dedicated token change.");
      }
      return { token: data.token, revision: data.assignment_revision };
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

    /** `agentId` (with `invokeAgent`): Ask agent asks that agent of the Space instead of the default (or the classifier's pick). */
    async sendMessage(
      spaceId: string,
      roomId: string,
      text: string,
      invokeAgent: boolean,
      mentions?: Mention[],
      assetIds?: readonly string[],
      choice?: TurnModelChoice,
      agentId?: string,
    ): Promise<void> {
      const body: Record<string, unknown> = { text, invoke_agent: invokeAgent, ...turnModelBody(choice) };
      if (invokeAgent && agentId) body.agent_id = agentId;
      // Only what a person can type: never `addressed_by`, which only the server sets.
      if (mentions && mentions.length > 0)
        body.mentions = mentions.map((m) => (m.kind === "agent" ? (m.agentId ? { kind: "agent", agent_id: m.agentId } : { kind: "agent" }) : { kind: "user", id: m.id }));
      const ids = assetIdsBody(assetIds);
      if (ids) body.asset_ids = ids;
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

    async sendChatMessage(text: string, assetIds?: readonly string[], choice?: TurnModelChoice): Promise<void> {
      // Text sends also ask the agent on servers that require the legacy flag.
      const body: Record<string, unknown> = { text, invoke_agent: true, ...turnModelBody(choice) };
      const ids = assetIdsBody(assetIds);
      if (ids) body.asset_ids = ids;
      await request("POST", "/v1/chat/messages", body);
    },

    async saveChatNote(text: string, assetIds?: readonly string[]): Promise<void> {
      const ids = assetIdsBody(assetIds);
      try {
        await request("POST", "/v1/chat/messages", { text, save_note: true, ...(ids ? { asset_ids: ids } : {}) });
      } catch (error) {
        // Older servers reject the unknown field while decoding the request.
        // Never fall back to invoke_agent: false: a mention could still start work.
        if (error instanceof ApiError && error.status === 422) {
          throw new ApiError("http", "Update Metro Server to use Save note. Your draft was kept and was not sent to the agent.", 422);
        }
        throw error;
      }
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
const isToken = (v: unknown): v is string => typeof v === "string" && v.length > 0 && !/\s/.test(v);
const isVisibility = (v: unknown): v is RunnerVisibility => v === "personal" || v === "organization";

function isRegisteredRunner(v: unknown): v is { id: string | number; name: string; visibility: RunnerVisibility; can_manage?: unknown } {
  return isObject(v) && isId(v.id) && typeof v.name === "string" && isVisibility(v.visibility);
}

function toRegisteredRunner(v: { id: string | number; name: string; visibility: RunnerVisibility; can_manage?: unknown; presence?: unknown; update?: unknown }): RegisteredRunner {
  const presence = isObject(v.presence) ? v.presence : undefined;
  const update = isObject(v.update) ? v.update : undefined;
  const install = presence?.install;
  const action = update?.action;
  const last = update?.last_result;
  const target = isObject(last) ? runnerVersion(last.target) : null;
  const at = isObject(last) ? runnerTimestamp(last.at) : null;
  return {
    id: String(v.id), name: v.name, visibility: v.visibility, canManage: v.can_manage === true,
    presence: presence && {
      seenAt: runnerTimestamp(presence.seen_at), online: presence.online === true, version: runnerVersion(presence.version),
      install: install === "managed" || install === "unmanaged" || install === "container" || install === "desktop" ? install : null,
      selfUpdate: presence.self_update === true,
    },
    update: update && {
      latest: runnerVersion(update.latest), available: update.available === true,
      action: action === "self_update" || action === "container" || action === "with_app" || action === "install_script" ? action : "none",
      containerImage: typeof update.container_image === "string" && update.container_image.length <= 160 ? update.container_image : null,
      pending: toRunnerPending(update.pending),
      lastResult: target && at !== null && isObject(last) && (last.state === "updated" || last.state === "failed")
        ? { target, at, state: last.state, code: typeof last.code === "string" && /^[a-z_]{1,40}$/.test(last.code) ? last.code : null }
        : null,
    },
  };
}

function runnerVersion(v: unknown): string | null {
  return typeof v === "string" && v === v.trim() && /^(0|[1-9]\d{0,9})(\.(0|[1-9]\d{0,9})){2}$/.test(v)
    && v.split(".").every((part) => Number(part) <= 0xffffffff) ? v : null;
}

function runnerTimestamp(v: unknown): number | null {
  return typeof v === "number" && Number.isSafeInteger(v) && v >= 0 ? v : null;
}

function toRunnerPending(v: unknown): RunnerPending | null {
  if (!isObject(v)) return null;
  const target = runnerVersion(v.target);
  const requestedAt = runnerTimestamp(v.requested_at);
  return target && requestedAt !== null ? { target, requestedAt } : null;
}

/** null: nobody serves the Space. Unknown shapes are a protocol error, never "no runner". */
function toSpaceRunner(v: unknown): SpaceRunner | null {
  if (v === null) return null;
  if (isObject(v) && (v.visibility === "dedicated" || v.kind === "dedicated")) {
    return { kind: "dedicated", name: typeof v.name === "string" && v.name ? v.name : "Dedicated Space runner" };
  }
  if (isRegisteredRunner(v)) return { kind: "registered", id: String(v.id), name: v.name, visibility: v.visibility };
  throw new ApiError("protocol", "The Space's runner is in a shape this app doesn't know.");
}

function isCatalogProvider(v: unknown): v is Record<string, unknown> & { id: string; name: string } {
  return isObject(v) && typeof v.id === "string" && v.id !== "" && typeof v.name === "string";
}

function toCatalogProvider(v: Record<string, unknown> & { id: string; name: string }): CatalogProvider {
  // Device adapters run in the user's metro-connector; the id prefix is the contract's fallback.
  const execution: ProviderExecution = v.execution === "device" || (v.execution === undefined && v.id.startsWith("device-")) ? "device" : "server";
  return {
    id: v.id,
    name: v.name,
    requiresKey: execution === "server" && v.requires_key === true,
    allowCustomUrl: execution === "server" && v.allow_custom_url === true,
    defaultBaseUrl: typeof v.default_base_url === "string" && v.default_base_url ? v.default_base_url : null,
    execution,
    description: typeof v.description === "string" && v.description ? v.description : null,
    capability: isCapability(v.capability) ? v.capability : "chat", // an older server has chat adapters only
    listsModels: v.lists_models !== false,
  };
}

const isCapability = (v: unknown): v is Capability => v === "chat" || v === "system_one" || v === "embeddings";
const isShareVisibility = (v: unknown): v is ShareVisibility => v === "personal" || v === "spaces" || v === "organization";

function toSharing(v: unknown, what: string): Sharing {
  if (!isObject(v) || !isShareVisibility(v.visibility)) throw new ApiError("protocol", `${what} did not return { visibility, space_ids }.`);
  const ids = Array.isArray(v.space_ids) ? v.space_ids.filter(isId).map(String) : [];
  const sharing: Sharing = { visibility: v.visibility, spaceIds: v.visibility === "spaces" ? [...new Set(ids)] : [] };
  if (v.mode === "preset" || v.mode === "owner_connection") sharing.mode = v.mode;
  return sharing;
}

function toAvailableModel(v: unknown): AvailableModel | null {
  if (!isObject(v) || !isId(v.connection_id) || typeof v.name !== "string" || !isCapability(v.capability)) return null;
  return {
    connectionId: String(v.connection_id),
    name: v.name,
    providerId: typeof v.provider_id === "string" ? v.provider_id : "",
    model: typeof v.model === "string" && v.model ? v.model : null,
    capability: v.capability,
    visibility: isShareVisibility(v.visibility) ? v.visibility : "personal",
    owned: v.owned === true,
    ownerId: isId(v.owner_id) ? String(v.owner_id) : null,
    execution: v.execution === "device" || (v.execution === undefined && typeof v.provider_id === "string" && v.provider_id.startsWith("device-")) ? "device" : "server",
    // Unknown: assume the safer reading for the person using it (someone else's endpoint, their bill).
    runsOn: v.runs_on === "your_connection" || v.runs_on === "your_own_sign_in" ? v.runs_on : v.owned === true ? "your_connection" : "owner_connection",
    billing: v.billing === "you" || (v.billing === undefined && v.owned === true) ? "you" : "owner",
    ready: typeof v.ready === "boolean" ? v.ready : null,
    isDefault: v.is_default === true,
    mediaInputs: v.capability === "embeddings" && v.media_inputs === true,
    ...(v.capability === "chat" && Object.prototype.hasOwnProperty.call(v, "chat_models") ? { chatModels: parseChatModels(v.chat_models) } : {}),
  };
}

function parseChatModels(value: unknown): NonNullable<AvailableModel["chatModels"]> {
  if (!Array.isArray(value)) return [];
  const result: NonNullable<AvailableModel["chatModels"]> = [];
  const seen = new Set<string>();
  for (const row of value) {
    if (!isObject(row) || typeof row.id !== "string" || !row.id.trim() || seen.has(row.id)) continue;
    seen.add(row.id);
    result.push({
      id: row.id,
      reasoningEfforts: efforts(row.reasoning_efforts),
      ...(typeof row.display_name === "string" && row.display_name.trim() ? { displayName: row.display_name.trim() } : {}),
      ...(typeof row.is_default === "boolean" ? { isDefault: row.is_default } : {}),
      ...(typeof row.available === "boolean" ? { available: row.available } : {}),
    });
  }
  return result;
}

/**
 * Thinking levels as the server advertises them: Metro's documented ones, or
 * the values a self-hosted model's owner set (1–32 of a-z, 0-9, _ and -).
 */
function efforts(value: unknown): string[] {
  const levels = Array.isArray(value) ? value : [];
  return [...new Set(levels.filter((x): x is string => typeof x === "string" && /^[a-z0-9_-]{1,32}$/.test(x)))];
}

const EFFORT_WIRES: readonly EffortWire[] = ["none", "reasoning_effort", "reasoning"];

function toThinking(v: unknown): ModelThinking | null {
  if (!isObject(v)) return null;
  const wire = EFFORT_WIRES.includes(v.wire as EffortWire) ? (v.wire as EffortWire) : "none";
  return { efforts: efforts(v.efforts), wire };
}

function toConnectionModels(value: unknown): ConnectionModel[] {
  if (!Array.isArray(value)) return [];
  const out: ConnectionModel[] = [];
  const seen = new Set<string>();
  for (const m of value) {
    if (!isObject(m) || typeof m.model_id !== "string" || !m.model_id || seen.has(m.model_id)) continue;
    seen.add(m.model_id);
    const custom = typeof m.custom_name === "string" && m.custom_name ? m.custom_name : null;
    const listed = typeof m.listed_name === "string" && m.listed_name ? m.listed_name : null;
    out.push({
      modelId: m.model_id,
      displayName: typeof m.display_name === "string" && m.display_name ? m.display_name : custom ?? listed ?? m.model_id,
      customName: custom,
      listedName: listed,
      enabled: m.enabled !== false,
      available: m.available !== false,
      source: m.source === "listed" ? "listed" : "manual",
      position: typeof m.position === "number" ? m.position : out.length,
      isDefault: m.is_default === true,
      thinking: toThinking(m.thinking),
      reasoningEfforts: efforts(m.reasoning_efforts),
    });
  }
  return out;
}

function toModelList(v: unknown, id: string, what: string): ConnectionModelList {
  if (!isObject(v) || !Array.isArray(v.models)) throw new ApiError("protocol", `${what} did not return { models: [...] }.`);
  return {
    connectionId: isId(v.connection_id) ? String(v.connection_id) : id,
    defaultModel: typeof v.default_model === "string" && v.default_model ? v.default_model : null,
    customThinking: v.custom_thinking === true,
    models: toConnectionModels(v.models),
  };
}

function toPickerProvider(v: unknown): PickerProvider[] {
  if (!isObject(v) || typeof v.provider_id !== "string" || !Array.isArray(v.connections)) return [];
  const connections: PickerConnection[] = [];
  for (const c of v.connections) {
    if (!isObject(c)) continue;
    const operator = c.operator === true;
    if (!operator && !isId(c.connection_id)) continue;
    const models: PickerModel[] = [];
    for (const m of Array.isArray(c.models) ? c.models : []) {
      if (!isObject(m) || typeof m.id !== "string" || !m.id.trim() || models.some((x) => x.id === m.id)) continue;
      models.push({
        id: m.id,
        displayName: typeof m.display_name === "string" && m.display_name.trim() ? m.display_name.trim() : m.id,
        reasoningEfforts: efforts(m.reasoning_efforts),
        isDefault: m.is_default === true,
        available: m.available !== false,
      });
    }
    connections.push({
      connectionId: operator ? null : String(c.connection_id),
      name: typeof c.name === "string" ? c.name : "",
      owned: c.owned === true,
      ownerId: isId(c.owner_id) ? String(c.owner_id) : null,
      visibility: isShareVisibility(c.visibility) ? c.visibility : null,
      runsOn: typeof c.runs_on === "string" ? c.runs_on : c.owned === true ? "your_connection" : "owner_connection",
      billing: typeof c.billing === "string" ? c.billing : c.owned === true ? "you" : "owner",
      ready: typeof c.ready === "boolean" ? c.ready : null,
      isDefault: c.is_default === true,
      defaultModel: typeof c.default_model === "string" && c.default_model ? c.default_model : null,
      operator,
      models,
    });
  }
  return connections.length ? [{ providerId: v.provider_id, providerName: typeof v.provider_name === "string" && v.provider_name ? v.provider_name : v.provider_id, connections }] : [];
}

const optionalId = (v: unknown): string | null => (isId(v) && String(v) !== "" ? String(v) : null);
const probability = (v: unknown, fallback: number): number => (typeof v === "number" && Number.isFinite(v) && v >= 0 && v <= 1 ? v : fallback);

function toModelRouting(v: unknown, what: string): ModelRouting {
  if (!isObject(v) || !Array.isArray(v.cases)) throw new ApiError("protocol", `${what} did not return the routing settings.`);
  const cases: RoutingCase[] = [];
  for (const c of v.cases) {
    if (!isObject(c) || typeof c.key !== "string" || !c.key || typeof c.label !== "string") continue;
    cases.push({ key: c.key, label: c.label, description: typeof c.description === "string" ? c.description : "", connectionId: optionalId(c.connection_id) });
  }
  return {
    enabled: v.enabled === true,
    inferAddressing: v.infer_addressing === true,
    classifierId: optionalId(v.classifier_id),
    embeddingId: optionalId(v.embedding_id),
    defaultChatId: optionalId(v.default_chat_id),
    // The server's defaults, for an answer that leaves them out.
    minDirectedness: probability(v.min_directedness, 0.85),
    minRouteProbability: probability(v.min_route_probability, 0.65),
    cases,
  };
}

function isConnection(v: unknown): v is Record<string, unknown> & { id: string | number; name: string; provider_id: string } {
  return isObject(v) && isId(v.id) && typeof v.name === "string" && typeof v.provider_id === "string";
}

function toConnection(v: Record<string, unknown> & { id: string | number; name: string; provider_id: string }): ProviderConnection {
  return {
    id: String(v.id),
    name: v.name,
    providerId: v.provider_id,
    baseUrl: typeof v.base_url === "string" && v.base_url ? v.base_url : null,
    model: typeof v.model === "string" && v.model ? v.model : null,
    isDefault: v.is_default === true,
    hasKey: v.has_key === true,
    capability: isCapability(v.capability) ? v.capability : null,
    visibility: isShareVisibility(v.visibility) ? v.visibility : null,
    models: toConnectionModels(v.models),
    customThinking: v.custom_thinking === true,
    modelsError: typeof v.models_error === "string" && v.models_error ? v.models_error : null,
  };
}

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
      if (isObject(m) && m.kind === "agent") {
        const mention: Mention = { kind: "agent" };
        if (typeof m.agent_id === "string" && m.agent_id.trim()) mention.agentId = m.agent_id.trim();
        if (m.addressed_by === "classifier" || m.addressed_by === "schedule") mention.addressedBy = m.addressed_by;
        mentions.push(mention);
      }
      else if (isObject(m) && m.kind === "user" && isId(m.id)) mentions.push({ kind: "user", id: String(m.id) });
    }
    if (mentions.length) event.mentions = mentions;
  }
  if (raw.audience === "room" || raw.audience === "agent" || raw.audience === "users" || raw.audience === "mixed") event.audience = raw.audience;
  const activity = toActivity(raw.activity);
  if (activity) {
    if (event.kind === "page_draft" && event.actor === "agent" && activity.source === "agent" && isObject(raw.activity)) {
      const draft = toPageDraftCard(raw.activity.page_draft);
      if (draft) activity.page_draft = draft;
    }
    event.activity = activity;
  }
  if (isId(raw.actor_id) && String(raw.actor_id).trim()) event.actorId = String(raw.actor_id);
  const createdAt = toCreatedAt(raw.created_at);
  if (createdAt) event.createdAt = createdAt;
  const attachments = toAttachments(raw.attachments);
  if (attachments.length) event.attachments = attachments;
  return event;
}

function toPageDraftCard(v: unknown): PageDraftEvent | undefined {
  if (!isObject(v) || typeof v.draft_id !== "string" || !/^pgd_[A-Za-z0-9_-]{1,96}$/.test(v.draft_id)
    || typeof v.author_id !== "string" || !v.author_id || typeof v.title !== "string" || typeof v.summary !== "string"
    || (v.kind !== "new" && v.kind !== "edit")) return undefined;
  const expires_at = toCreatedAt(v.expires_at);
  const page_id = typeof v.page_id === "string" && /^pg_[A-Za-z0-9_-]{1,96}$/.test(v.page_id) ? v.page_id : null;
  if (!expires_at || (v.kind === "edit" && !page_id) || (v.kind === "new" && v.page_id !== null)) return undefined;
  return { draft_id: v.draft_id, title: v.title.slice(0, 120), summary: v.summary.slice(0, 240), kind: v.kind, page_id, author_id: v.author_id, expires_at };
}

/** `asset_ids` for a message body: distinct, at most MAX_MESSAGE_ATTACHMENTS; undefined when there are none. */
export function assetIdsBody(ids: readonly string[] | undefined): string[] | undefined {
  if (!ids || ids.length === 0) return undefined;
  const distinct = [...new Set(ids.map((id) => id.trim()).filter(Boolean))];
  if (distinct.length > MAX_MESSAGE_ATTACHMENTS) throw new ApiError("protocol", `A message carries at most ${MAX_MESSAGE_ATTACHMENTS} files.`);
  return distinct.length ? distinct : undefined;
}

/**
 * An event's `attachments`: files and `{ removed: true }` tombstones,
 * well-formed only, files distinct by id, at most MAX_MESSAGE_ATTACHMENTS.
 * Nothing else in them (a URL, a Space) is read.
 */
export function toAttachments(v: unknown): EventAttachment[] {
  if (!Array.isArray(v)) return [];
  const out: EventAttachment[] = [];
  for (const a of v) {
    if (out.length >= MAX_MESSAGE_ATTACHMENTS) break;
    if (!isObject(a)) continue;
    if (a.removed === true) {
      out.push({ removed: true });
      continue;
    }
    if (!isId(a.asset_id) || typeof a.name !== "string" || !a.name.trim()) continue;
    const assetId = String(a.asset_id);
    if (out.some((x) => !x.removed && x.assetId === assetId)) continue;
    const size = typeof a.size_bytes === "number" && Number.isFinite(a.size_bytes) && a.size_bytes >= 0 ? Math.floor(a.size_bytes) : 0;
    out.push({
      removed: false,
      assetId,
      name: a.name.trim().slice(0, 255),
      mimeType: typeof a.mime_type === "string" ? a.mime_type : "application/octet-stream",
      kind: typeof a.kind === "string" ? a.kind : null,
      sizeBytes: size,
    });
  }
  return out;
}

const RFC3339 = /^\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}(\.\d{1,9})?(Z|[+-]\d{2}:\d{2})$/i;

/**
 * The server's `created_at` (RFC 3339 in UTC, or null for history whose time
 * is unknown) as a UTC ISO string; undefined when missing, null or malformed.
 * A number is never read as a time: the wire contract is a string.
 */
export function toCreatedAt(v: unknown): string | undefined {
  if (typeof v !== "string") return undefined;
  const s = v.trim();
  if (!RFC3339.test(s)) return undefined;
  const ms = Date.parse(s);
  // Epoch 0 is how some migrated rows without a time were stored.
  if (!Number.isFinite(ms) || ms <= 0) return undefined;
  return new Date(ms).toISOString();
}

const ACTIVITY_PHASES: readonly string[] = ["started", "completed", "failed", "cancelled", "interrupted"];

/** The event's activity metadata, or undefined when it is missing or malformed (then the event is uncorrelated). */
export function toActivity(v: unknown): EventActivity | undefined {
  if (!isObject(v)) return undefined;
  const id = typeof v.id === "string" ? v.id.trim() : typeof v.id === "number" && Number.isFinite(v.id) ? String(v.id) : "";
  if (!id) return undefined;
  if (v.source !== "agent" && v.source !== "demo") return undefined;
  const activity: EventActivity = { id, name: typeof v.name === "string" ? v.name.trim() : "", source: v.source };
  if (typeof v.phase === "string" && ACTIVITY_PHASES.includes(v.phase)) activity.phase = v.phase as ActivityPhase;
  const route = toRoute(v.route);
  if (route) activity.route = route;
  const agent = toEventAgent(v.agent);
  if (agent) activity.agent = agent;
  const pick = shortText(v.pick, 200);
  if (pick) activity.pick = pick;
  return activity;
}

/** A run's agent badge (`activity.agent`), or undefined when missing or malformed. */
export function toEventAgent(v: unknown): EventAgent | undefined {
  if (!isObject(v) || typeof v.id !== "string" || !v.id.trim() || typeof v.name !== "string" || !v.name.trim()) return undefined;
  const agent: EventAgent = {
    id: v.id.trim(),
    handle: typeof v.handle === "string" ? v.handle.trim() : "",
    name: v.name.trim().slice(0, 64),
    color: typeof v.color === "string" && /^#[0-9a-f]{6}$/i.test(v.color) ? v.color : "#4f46e5",
  };
  if (typeof v.avatar_version === "string" && v.avatar_version.trim()) agent.avatar_version = v.avatar_version.trim();
  return agent;
}

const ROUTE_SOURCES: readonly string[] = ["override", "case", "routing_default", "default", "environment", "inherited"];
const ADDRESSED_BY: readonly string[] = ["explicit", "classifier", "schedule", "thread"];
const shortText = (v: unknown, max: number): string | null => (typeof v === "string" && v.trim() ? v.trim().slice(0, max) : null);

/** A turn's route, or undefined when missing or malformed (then nothing is shown). */
export function toRoute(v: unknown): ActivityRoute | undefined {
  if (!isObject(v) || typeof v.source !== "string" || !ROUTE_SOURCES.includes(v.source)) return undefined;
  return {
    source: v.source as RouteSource,
    addressedBy: typeof v.addressed_by === "string" && ADDRESSED_BY.includes(v.addressed_by) ? (v.addressed_by as AddressedBy) : "explicit",
    caseLabel: shortText(v.case_label, 64),
    providerId: shortText(v.provider_id, 64),
    modelId: shortText(v.model_id, 256),
    reason: shortText(v.reason, 300) ?? "",
  };
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

/** Maps a non-2xx response to its ApiError (the body read only for a short server message). */
async function checkStatus(response: Response, method: string, path: string, pageDetails = false): Promise<void> {
  if (response.status === 401) {
    throw new ApiError("auth", "The server rejected the access token (HTTP 401). Check the token and connect again.", 401);
  }
  if (response.status === 403) {
    const detail = await safeSnippet(response);
    throw new ApiError("forbidden", `You don't have access to that (HTTP 403)${detail ? `: ${detail}` : ""}.`, 403);
  }
  if (!response.ok) {
    // Only Pages needs structured error bodies (diagnostics, conflicts and
    // action challenges). Do not turn any of those fields into error text.
    if (pageDetails && [409, 422, 428].includes(response.status)) {
      const details: unknown = await response.json().catch(() => undefined);
      const error = isObject(details) && typeof details.error === "string" ? details.error.slice(0, 200) : "";
      throw new ApiError("http", error || `${method} ${path} failed with HTTP ${response.status}`, response.status, details);
    }
    const detail = await safeSnippet(response);
    throw new ApiError(
      response.status === 404 ? "not_found" : "http",
      `${method} ${path} failed with HTTP ${response.status}${detail ? `: ${detail}` : ""}`,
      response.status,
    );
  }
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
