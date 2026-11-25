// A Space's own settings (crates/server/src/{spaces,space_agents,space_prefs}.rs):
//
// - The Space: GET (members), PATCH name/description and DELETE with the
//   exact name typed (owner), POST …/leave (members other than the owner).
// - Its agents: every member reads them, only the owner writes them. Each
//   agent has a stable id (`ag-…`), a unique @handle, a one-line description
//   the classifier chooses by, instructions every member can read, a tool
//   allowlist, a colour from a fixed palette and an optional picture.
// - The signed-in person's own preferences in that Space: only they read or
//   write them.
//
// Pictures travel only as bytes on the authenticated transport (like
// profiles: ./profile.ts), never as a URL with a credential in it.

import { ApiError } from "./client";
import type { RequestFn } from "./client";
import { sniffAvatar } from "./profile";
import type { AvatarMime, RawFn } from "./profile";

/** The colours an agent without a picture is drawn on (space_agents.rs `COLORS`). */
export const AGENT_COLORS = ["#4f46e5", "#0891b2", "#059669", "#ca8a04", "#ea580c", "#dc2626", "#db2777", "#7c3aed"] as const;
/** Server caps (space_agents.rs, spaces.rs); the server is the judge and its message is shown as-is. */
export const MAX_AGENT_NAME = 64;
export const MAX_AGENT_HANDLE = 32;
export const MAX_AGENT_DESCRIPTION = 160;
export const MAX_AGENT_INSTRUCTIONS = 4000;
export const MAX_SPACE_DESCRIPTION = 500;
export const MAX_SPACE_AGENTS = 16;
/** The handle only the Space's default agent may have (the generic `@agent`). */
export const RESERVED_HANDLE = "agent";

export type SpaceDetails = { id: string; name: string; description: string; role: "owner" | "member" };

export type SpaceAgent = {
  id: string;
  spaceId: string;
  name: string;
  handle: string;
  description: string;
  instructions: string;
  tools: string[];
  isDefault: boolean;
  /** Changes on every picture upload; null without a picture. */
  avatarVersion: string | null;
  color: string;
  initial: string;
};

export type SpaceAgents = { agents: SpaceAgent[]; tools: string[]; defaultTools: string[] };

// Only the original defaults are safe to assume on servers that don't
// advertise defaults. A tool's presence in the catalogue is not consent.
const LEGACY_AGENT_DEFAULTS = new Set([
  "workspace.list", "workspace.read", "workspace.check", "workspace.mcp_list", "workspace.mcp", "threads",
]);

export type NewAgent = {
  name: string;
  handle: string;
  description: string;
  instructions?: string;
  tools?: string[];
  color?: string;
  isDefault?: boolean;
};
export type AgentPatch = Partial<Omit<NewAgent, "isDefault">> & { isDefault?: true };

/** A chat model choice: a connection, optionally one of its models and a reasoning effort. */
export type ModelSelection = { connectionId: string; modelId?: string; reasoningEffort?: string };

export type SpacePrefs = {
  classifyUnaddressed: boolean;
  defaultModel: ModelSelection | null;
  /** Agent id → selection. */
  agentModels: Record<string, ModelSelection>;
  routing: boolean;
};
export type SpacePrefsView = SpacePrefs & { classifierAvailable: boolean };

const enc = encodeURIComponent;
const isObject = (v: unknown): v is Record<string, unknown> => typeof v === "object" && v !== null && !Array.isArray(v);
const str = (v: unknown) => (typeof v === "string" ? v : "");

// ---------------------------------------------------------------- parsing

export function parseSpaceDetails(v: unknown): SpaceDetails | null {
  if (!isObject(v) || typeof v.id !== "string" || !v.id || typeof v.name !== "string") return null;
  return { id: v.id, name: v.name, description: str(v.description), role: v.role === "owner" ? "owner" : "member" };
}

export function parseAgent(v: unknown): SpaceAgent | null {
  if (!isObject(v) || typeof v.id !== "string" || !v.id || typeof v.name !== "string" || typeof v.handle !== "string") return null;
  const version = v.avatar_version == null ? null : String(v.avatar_version).trim() || null;
  const name = v.name;
  return {
    id: v.id,
    spaceId: str(v.space_id),
    name,
    handle: v.handle,
    description: str(v.description),
    instructions: str(v.instructions),
    tools: Array.isArray(v.tools) ? v.tools.filter((t): t is string => typeof t === "string") : [],
    isDefault: v.is_default === true,
    avatarVersion: version,
    color: typeof v.color === "string" && v.color ? v.color : AGENT_COLORS[0],
    initial: typeof v.initial === "string" && v.initial ? v.initial : initialOf(name),
  };
}

/** The first letter or digit of a name, upper-cased; "?" when there is none (as the server does). */
export function initialOf(name: string): string {
  for (const c of name) if (/[\p{L}\p{N}]/u.test(c)) return c.toUpperCase();
  return "?";
}

function parseSelection(v: unknown): ModelSelection | null {
  if (!isObject(v) || typeof v.provider_connection_id !== "string" || !v.provider_connection_id) return null;
  const out: ModelSelection = { connectionId: v.provider_connection_id };
  if (typeof v.model_id === "string" && v.model_id) out.modelId = v.model_id;
  if (typeof v.reasoning_effort === "string" && v.reasoning_effort) out.reasoningEffort = v.reasoning_effort;
  return out;
}

function selectionBody(s: ModelSelection) {
  const out: Record<string, string> = { provider_connection_id: s.connectionId };
  if (s.modelId) out.model_id = s.modelId;
  if (s.reasoningEffort) out.reasoning_effort = s.reasoningEffort;
  return out;
}

export function parsePrefs(v: unknown): SpacePrefsView | null {
  if (!isObject(v)) return null;
  const agentModels: Record<string, ModelSelection> = {};
  if (isObject(v.agent_models)) {
    for (const [id, sel] of Object.entries(v.agent_models)) {
      const s = parseSelection(sel);
      if (s) agentModels[id] = s;
    }
  }
  return {
    classifyUnaddressed: v.classify_unaddressed === true,
    defaultModel: parseSelection(v.default_model),
    agentModels,
    routing: v.routing !== false,
    classifierAvailable: v.classifier_available === true,
  };
}

export function prefsBody(p: SpacePrefs) {
  const agent_models: Record<string, Record<string, string>> = {};
  for (const [id, sel] of Object.entries(p.agentModels)) agent_models[id] = selectionBody(sel);
  return {
    classify_unaddressed: p.classifyUnaddressed,
    default_model: p.defaultModel ? selectionBody(p.defaultModel) : null,
    agent_models,
    routing: p.routing,
  };
}

function agentBody(a: Omit<AgentPatch, "isDefault"> & { isDefault?: boolean }) {
  const out: Record<string, unknown> = {};
  if (a.name !== undefined) out.name = a.name;
  if (a.handle !== undefined) out.handle = a.handle;
  if (a.description !== undefined) out.description = a.description;
  if (a.instructions !== undefined) out.instructions = a.instructions;
  if (a.tools !== undefined) out.tools = a.tools;
  if (a.color !== undefined) out.color = a.color;
  if (a.isDefault) out.is_default = true;
  return out;
}

// ---------------------------------------------------------------- validation (client side, for live hints)

/** Why a handle can't be used, or null. `current`: the agent's handle now (it may keep "agent"). */
export function handleProblem(handle: string, taken: readonly string[], current?: string): string | null {
  if (!handle) return "Enter a handle.";
  if (handle.length > MAX_AGENT_HANDLE) return `Use at most ${MAX_AGENT_HANDLE} characters.`;
  if (!/^[a-z0-9_-]+$/.test(handle)) return "Use only lowercase letters, digits, - and _.";
  if (handle === RESERVED_HANDLE && current !== RESERVED_HANDLE) return "@agent is reserved for the Space's default agent.";
  if (handle !== current && taken.includes(handle)) return `@${handle} is already taken in this Space.`;
  return null;
}

/** A handle suggested from a name ("Code Reviewer" → "code-reviewer"). */
export function suggestHandle(name: string): string {
  return name
    .normalize("NFKD")
    .toLowerCase()
    .replace(/[^a-z0-9_ -]/g, "")
    .trim()
    .replace(/\s+/g, "-")
    .slice(0, MAX_AGENT_HANDLE);
}

// ---------------------------------------------------------------- transport

export type SpacesApi = ReturnType<typeof spacesApi>;

export function spacesApi(request: RequestFn, raw: RawFn) {
  const sp = (sid: string) => `/v1/spaces/${enc(sid)}`;
  const ag = (sid: string, aid: string) => `${sp(sid)}/agents/${enc(aid)}`;
  const expectAgent = (data: unknown, what: string) => {
    const a = parseAgent(data);
    if (!a) throw new ApiError("protocol", `${what} did not return an agent.`);
    return a;
  };
  return {
    async spaceDetails(sid: string): Promise<SpaceDetails> {
      const s = parseSpaceDetails(await request("GET", sp(sid)));
      if (!s) throw new ApiError("protocol", "GET /v1/spaces/{id} did not return { id, name, description }.");
      return s;
    },
    async updateSpace(sid: string, patch: { name?: string; description?: string }): Promise<SpaceDetails> {
      const s = parseSpaceDetails(await request("PATCH", sp(sid), patch));
      if (!s) throw new ApiError("protocol", "PATCH /v1/spaces/{id} did not return the Space.");
      return s;
    },
    /** Deletes the Space for good; `confirmName` must be exactly its name. */
    async deleteSpace(sid: string, confirmName: string): Promise<void> {
      await request("DELETE", sp(sid), { confirm_name: confirmName });
    },
    async leaveSpace(sid: string): Promise<{ stoppedRuns: number }> {
      const data = await request("POST", `${sp(sid)}/leave`, {});
      return { stoppedRuns: isObject(data) && typeof data.stopped_runs === "number" ? data.stopped_runs : 0 };
    },

    async spaceAgents(sid: string): Promise<SpaceAgents> {
      const data = await request("GET", `${sp(sid)}/agents`);
      if (!isObject(data) || !Array.isArray(data.agents)) throw new ApiError("protocol", "GET …/agents did not return { agents: [...] }.");
      const tools = Array.isArray(data.tools) ? data.tools.filter((t): t is string => typeof t === "string") : [];
      const defaults = Array.isArray(data.default_tools) ? new Set(data.default_tools) : LEGACY_AGENT_DEFAULTS;
      return {
        agents: data.agents.flatMap((a) => parseAgent(a) ?? []),
        tools,
        defaultTools: tools.filter((tool) => defaults.has(tool)),
      };
    },
    async createAgent(sid: string, agent: NewAgent): Promise<SpaceAgent> {
      return expectAgent(await request("POST", `${sp(sid)}/agents`, agentBody(agent)), "POST …/agents");
    },
    async updateAgent(sid: string, aid: string, patch: AgentPatch): Promise<SpaceAgent> {
      return expectAgent(await request("PATCH", ag(sid, aid), agentBody(patch)), "PATCH …/agents/{id}");
    },
    async deleteAgent(sid: string, aid: string): Promise<{ defaultAgentId: string | null }> {
      const data = await request("DELETE", ag(sid, aid));
      return { defaultAgentId: isObject(data) && typeof data.default_agent_id === "string" ? data.default_agent_id : null };
    },
    /** The agent's picture bytes; null when it has none (404). */
    async agentAvatar(sid: string, aid: string, signal?: AbortSignal): Promise<{ mime: AvatarMime; bytes: Uint8Array } | null> {
      let res;
      try {
        res = await raw("GET", `${ag(sid, aid)}/avatar`, { accept: "image/png, image/jpeg", signal });
      } catch (error) {
        if (error instanceof ApiError && error.kind === "not_found") return null;
        throw error;
      }
      const mime = sniffAvatar(res.bytes);
      if (!mime) throw new ApiError("protocol", "The picture the server sent isn't a PNG or JPEG.");
      return { mime, bytes: res.bytes };
    },
    /** Sets the agent's picture (owner). The bytes must already be a PNG or JPEG. */
    async setAgentAvatar(sid: string, aid: string, bytes: Uint8Array, mime: AvatarMime): Promise<SpaceAgent | null> {
      const res = await raw("PUT", `${ag(sid, aid)}/avatar`, { body: bytes, contentType: mime, accept: "application/json", timeoutMs: 30_000 });
      if (!res.bytes.length) return null;
      try {
        return parseAgent(JSON.parse(new TextDecoder().decode(res.bytes)));
      } catch {
        return null;
      }
    },
    async removeAgentAvatar(sid: string, aid: string): Promise<void> {
      await request("DELETE", `${ag(sid, aid)}/avatar`);
    },

    async spacePrefs(sid: string): Promise<SpacePrefsView> {
      const p = parsePrefs(await request("GET", `${sp(sid)}/prefs`));
      if (!p) throw new ApiError("protocol", "GET …/prefs did not return your preferences.");
      return p;
    },
    async putSpacePrefs(sid: string, prefs: SpacePrefs): Promise<SpacePrefsView> {
      const p = parsePrefs(await request("PUT", `${sp(sid)}/prefs`, prefsBody(prefs)));
      if (!p) throw new ApiError("protocol", "PUT …/prefs did not return your preferences.");
      return p;
    },
  };
}
