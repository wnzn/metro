// A Space's agents in this session: the list every member may read (cached
// per Space, loaded again after any edit and now and then), their pictures,
// and the data the room's @ mention list and the Ask-agent picker need.
//
// - Only the server's permission-checked endpoints are used. A 403/404 means
//   the user can't see the Space (any more): the cache is dropped, nothing is
//   shown, no error.
// - Pictures are bytes fetched with the user's credential and shown through
//   `blob:` URLs, keyed by Space, agent and picture version, revoked when they
//   change and at the end of the session. Never a remote URL, never on disk.
// - Everything belongs to one session (`onSessionEnd` resets it).

import { batch, signal, untracked } from "nsh";
import { ApiError } from "../../api/client";
import type { SpaceAgent } from "../../api/spaces";
import { apiClient, describe, disconnect, isAborted, onSessionEnd, sessionGuard } from "../../store";

/** How long a loaded list is trusted before `ensureSpaceAgents` asks again. */
export const AGENTS_RECHECK_MS = 60_000;

export const TOOL_LABELS: Readonly<Record<string, { label: string; description: string; group: "workspace" | "web" | "pages" }>> = {
  "workspace.list": { label: "List files", description: "List files in this Space's folder.", group: "workspace" },
  "workspace.read": { label: "Read files", description: "Read files in this Space's folder.", group: "workspace" },
  "workspace.write": { label: "Write files", description: "Create or replace files in this Space's folder.", group: "workspace" },
  "workspace.shell": { label: "Run shell commands", description: "Run commands in this Space's folder, as the runner's shell policy allows.", group: "workspace" },
  "workspace.check": { label: "Run checks", description: "Run checks defined by the runner operator.", group: "workspace" },
  "workspace.mcp_list": { label: "List MCP tools", description: "See the operator's MCP tools.", group: "workspace" },
  "workspace.mcp": { label: "Use MCP tools", description: "Call the operator's MCP tools.", group: "workspace" },
  threads: { label: "Threads", description: "Start and reply in threads in this Space.", group: "workspace" },
  web_search: { label: "Search the web", description: "Send search queries to the starter's web search providers. Off by default.", group: "web" },
  web_fetch: { label: "Read web pages", description: "Send addresses shared by people or returned by a search in the same run to those providers. Off by default.", group: "web" },
  pages_list: { label: "List Pages", description: "List Pages the starter can see in this Space. Off by default.", group: "pages" },
  page_read: { label: "Read Page data", description: "Read room-safe data the starter can access. Off by default.", group: "pages" },
  page_propose: { label: "Propose Page changes", description: "Draft a new Page or an edit; nothing changes until the starter applies it with edit rights. Off by default.", group: "pages" },
};

export function toolInfo(id: string) {
  return Object.prototype.hasOwnProperty.call(TOOL_LABELS, id) ? TOOL_LABELS[id] : undefined;
}

export function isOptInTool(id: string) {
  const group = toolInfo(id)?.group;
  return group === "web" || group === "pages";
}

export type AgentList = {
  status: "loading" | "ready" | "hidden" | "error";
  agents: readonly SpaceAgent[];
  /** Every tool an agent's allowlist may name (the server's catalogue). */
  tools: readonly string[];
  /** Selected initially only when creating a new agent. */
  defaultTools: readonly string[];
  error: string | null;
};

const lists = signal<Record<string, AgentList>>({});
const pictures = signal<Record<string, string>>({});

const checkedAt = new Map<string, number>();
const inflight = new Map<string, Promise<boolean>>();
const loadTickets = new Map<string, object>();
const pictureInflight = new Set<string>();
const failedPictures = new Set<string>();
let generation = 0;

const pictureKey = (sid: string, aid: string, version: string) => `${sid}\u0001${aid}\u0001${version}`;
const EMPTY: readonly SpaceAgent[] = [];

/** The Space's agents as last loaded (empty until `ensureSpaceAgents` ran). Reactive. */
export function spaceAgents(spaceId: string | null | undefined): readonly SpaceAgent[] {
  if (!spaceId) return EMPTY;
  return lists()[spaceId]?.agents ?? EMPTY;
}

/** The whole state of a Space's list, or undefined before anything asked. Reactive. */
export function agentList(spaceId: string | null | undefined): AgentList | undefined {
  return spaceId ? lists()[spaceId] : undefined;
}

/** The Space's default agent (the one plain `@agent` and Ask agent start). */
export function defaultAgent(spaceId: string | null | undefined): SpaceAgent | null {
  return spaceAgents(spaceId).find((a) => a.isDefault) ?? null;
}

export function agentById(spaceId: string | null | undefined, agentId: string | null | undefined): SpaceAgent | null {
  if (!agentId) return null;
  return spaceAgents(spaceId).find((a) => a.id === agentId) ?? null;
}

/** Loads the list once per `AGENTS_RECHECK_MS` (or now with `force`); concurrent calls share one request. */
export async function ensureSpaceAgents(spaceId: string, force = false): Promise<void> {
  if (!spaceId) return;
  const running = inflight.get(spaceId);
  if (running) { await running; return; }
  const last = checkedAt.get(spaceId);
  if (!force && last !== undefined && Date.now() - last < AGENTS_RECHECK_MS) return;
  await startAgentLoad(spaceId, () => true);
}

/** A mention visit needs its own authorized roster response, not an older in-flight read. */
export function loadSpaceAgentsForVisit(spaceId: string, isCurrentVisit: () => boolean): Promise<boolean> {
  return startAgentLoad(spaceId, isCurrentVisit);
}

function startAgentLoad(spaceId: string, valid: () => boolean): Promise<boolean> {
  const ticket = {};
  loadTickets.set(spaceId, ticket);
  const work = loadAgents(spaceId, () => loadTickets.get(spaceId) === ticket && valid()).finally(() => {
    if (inflight.get(spaceId) === work) inflight.delete(spaceId);
  });
  inflight.set(spaceId, work);
  return work;
}

function supersedeAgentLoad(spaceId: string) {
  loadTickets.delete(spaceId);
  inflight.delete(spaceId);
}

/** Drops the cached list (after an edit, or a membership change): the next read loads it again. */
export function invalidateSpaceAgents(spaceId: string) {
  checkedAt.delete(spaceId);
}

async function loadAgents(spaceId: string, valid: () => boolean): Promise<boolean> {
  const api = apiClient();
  if (!api || !valid()) return false;
  const gen = generation;
  const ok = sessionGuard(spaceId);
  const live = () => ok() && gen === generation && valid();
  const current = untracked(lists)[spaceId];
  if (!current) setList(spaceId, { status: "loading", agents: EMPTY, tools: [], defaultTools: [], error: null });
  try {
    const { agents, tools, defaultTools } = await api.spaceAgents(spaceId);
    if (!live()) return false;
    checkedAt.set(spaceId, Date.now());
    setList(spaceId, { status: "ready", agents, tools, defaultTools, error: null });
    return true;
  } catch (error) {
    if (!live() || isAborted(error)) return false;
    if (error instanceof ApiError && error.kind === "auth") {
      disconnect(error.message);
      return false;
    }
    if (error instanceof ApiError && (error.kind === "forbidden" || error.kind === "not_found")) {
      forgetSpaceAgents(spaceId);
      setList(spaceId, { status: "hidden", agents: EMPTY, tools: [], defaultTools: [], error: null });
      return false;
    }
    const before = untracked(lists)[spaceId];
    setList(spaceId, { status: before?.status === "ready" ? "ready" : "error", agents: before?.agents ?? EMPTY, tools: before?.tools ?? [], defaultTools: before?.defaultTools ?? [], error: describe(error, "Couldn't load this Space's agents") });
    return false;
  }
}

function setList(spaceId: string, next: AgentList) {
  lists.set({ ...untracked(lists), [spaceId]: next });
}

/** Puts an agent the server just returned into the cached list (create or edit). */
export function adoptAgent(agent: SpaceAgent) {
  const sid = agent.spaceId;
  supersedeAgentLoad(sid);
  const current = untracked(lists)[sid];
  if (!current) return;
  const others = current.agents.filter((a) => a.id !== agent.id);
  // A new default clears the flag on the others, as the server does.
  const agents = [...(agent.isDefault ? others.map((a) => (a.isDefault ? { ...a, isDefault: false } : a)) : others), agent];
  const order = new Map(current.agents.map((a, i) => [a.id, i]));
  agents.sort((a, b) => (order.get(a.id) ?? Infinity) - (order.get(b.id) ?? Infinity));
  setList(sid, { ...current, agents });
}

/** Drops an agent the owner just deleted; `defaultAgentId` is the server's default afterwards. */
export function dropAgent(spaceId: string, agentId: string, defaultAgentId: string | null) {
  supersedeAgentLoad(spaceId);
  const current = untracked(lists)[spaceId];
  if (!current) return;
  const agents = current.agents.filter((a) => a.id !== agentId).map((a) => (defaultAgentId ? { ...a, isDefault: a.id === defaultAgentId } : a));
  setList(spaceId, { ...current, agents });
  revokePictures(spaceId, agentId);
}

/** Forgets one Space's agents and pictures (deleted or left). */
export function forgetSpaceAgents(spaceId: string) {
  supersedeAgentLoad(spaceId);
  checkedAt.delete(spaceId);
  const { [spaceId]: _gone, ...rest } = untracked(lists);
  lists.set(rest);
  revokePictures(spaceId);
}

function revokePictures(spaceId: string, agentId?: string) {
  const prefix = agentId ? `${spaceId}\u0001${agentId}\u0001` : `${spaceId}\u0001`;
  const next: Record<string, string> = {};
  for (const [key, url] of Object.entries(untracked(pictures))) {
    if (key.startsWith(prefix)) URL.revokeObjectURL(url);
    else next[key] = url;
  }
  pictures.set(next);
}

// ---------------------------------------------------------------- pictures

/**
 * The `blob:` URL of an agent's picture at `version`, or null (none, not
 * loaded yet, or it failed). Asking starts the load once. Reactive.
 */
export function agentPictureUrl(spaceId: string, agentId: string, version: string | null | undefined): string | null {
  if (!spaceId || !agentId || !version) return null;
  const key = pictureKey(spaceId, agentId, version);
  const url = pictures()[key];
  if (url) return url;
  if (!pictureInflight.has(key) && !failedPictures.has(key)) untracked(() => void loadPicture(spaceId, agentId, version));
  return null;
}

async function loadPicture(spaceId: string, agentId: string, version: string): Promise<void> {
  const api = apiClient();
  if (!api) return;
  const key = pictureKey(spaceId, agentId, version);
  pictureInflight.add(key);
  const gen = generation;
  const ok = sessionGuard();
  try {
    const picture = await api.agentAvatar(spaceId, agentId);
    if (!ok() || gen !== generation) return;
    if (!picture) {
      failedPictures.add(key);
      return;
    }
    showPicture(spaceId, agentId, version, picture.bytes, picture.mime);
  } catch (error) {
    if (!ok() || gen !== generation || isAborted(error)) return;
    if (error instanceof ApiError && error.kind === "auth") return disconnect(error.message);
    failedPictures.add(key); // the initial stays
  } finally {
    pictureInflight.delete(key);
  }
}

/** Shows bytes as the agent's picture at `version` (just uploaded, or just fetched); older versions are revoked. */
export function showPicture(spaceId: string, agentId: string, version: string, bytes: Uint8Array, mime: string) {
  revokePictures(spaceId, agentId);
  const url = URL.createObjectURL(new Blob([bytes as BlobPart], { type: mime }));
  pictures.set({ ...untracked(pictures), [pictureKey(spaceId, agentId, version)]: url });
}

/** The picture couldn't be drawn: drop it, the initial shows instead. */
export function agentPictureFailed(spaceId: string, agentId: string, version: string | null | undefined) {
  if (!version) return;
  const key = pictureKey(spaceId, agentId, version);
  failedPictures.add(key);
  const url = untracked(pictures)[key];
  if (url) {
    URL.revokeObjectURL(url);
    const { [key]: _gone, ...rest } = untracked(pictures);
    pictures.set(rest);
  }
}

// ---------------------------------------------------------------- mentions and Ask agent

/** One agent in the room's @ list or the Ask-agent picker, with the mention it sends. */
export type AgentMentionOption = {
  key: string;
  agent: SpaceAgent;
  /** The wire mention: `{"kind":"agent","agent_id":"ag-…"}`. */
  mention: { kind: "agent"; agentId: string };
  /** What the draft gets: `@handle`. */
  literal: string;
  title: string;
  detail: string;
  testID: string;
  label: string;
};

/**
 * The Space's agents whose handle or name matches what follows "@" (the
 * default one first), for the mention list and the Ask-agent picker. Call
 * `ensureSpaceAgents(spaceId)` when the list opens.
 */
export function agentMentionOptions(spaceId: string | null | undefined, query: string): AgentMentionOption[] {
  const q = query.trim().toLowerCase();
  const agents = [...spaceAgents(spaceId)].sort((a, b) => Number(b.isDefault) - Number(a.isDefault));
  return agents
    .filter((a) => !q || a.handle.startsWith(q) || a.name.toLowerCase().includes(q))
    .map((a) => ({
      key: `agent:${a.id}`,
      agent: a,
      mention: { kind: "agent" as const, agentId: a.id },
      literal: `@${a.handle}`,
      title: a.name,
      detail: a.isDefault ? `@${a.handle} · default` : `@${a.handle}`,
      testID: `pick-agent:${a.id}`,
      label: `Mention ${a.name} (@${a.handle}): ${a.description}`,
    }));
}

/** The wire form of an agent mention, as POST …/messages takes it. */
export function agentMentionWire(agentId: string): { kind: "agent"; agent_id: string } {
  return { kind: "agent", agent_id: agentId };
}

// ---------------------------------------------------------------- session

export function resetSpaceAgents() {
  generation++;
  for (const url of Object.values(untracked(pictures))) URL.revokeObjectURL(url);
  checkedAt.clear();
  inflight.clear();
  loadTickets.clear();
  pictureInflight.clear();
  failedPictures.clear();
  batch(() => {
    lists.set({});
    pictures.set({});
  });
}

onSessionEnd(resetSpaceAgents);
