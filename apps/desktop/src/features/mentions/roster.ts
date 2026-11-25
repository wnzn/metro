import { signal } from "nsh";
import { activeSpaceId, inSpaceComposer, me, members, membersError, mentionVisitGuard, mentionVisitVerified, registerMentionDirectory, status } from "../../store";
import { personState, profileName } from "../profile/profileState";
import { agentList, loadSpaceAgentsForVisit } from "../space/agents";
import type { MentionDirectory } from "./draft";

type AgentVisit = {
  sid: string; current: () => boolean; refresh: () => void; verified: () => boolean;
  forSend: (live: () => boolean) => Promise<MentionDirectory["agents"] | null>;
};
const agentVisit = signal<AgentVisit | null>(null);

export function verifiedMembers(sid: string) {
  if (activeSpaceId() !== sid || !inSpaceComposer() || !mentionVisitVerified(sid) || status().phase !== "online" || membersError()) return null;
  const list = members();
  return list?.some(m => m.userId === me()) ? list : null;
}

export function verifiedAgents(sid: string) {
  const visit = agentVisit();
  if (visit?.sid !== sid || !visit.current() || !visit.verified() || !verifiedMembers(sid)) return null;
  const list = agentList(sid);
  return list?.status === "ready" && !list.error ? list.agents.filter(a => a.spaceId === sid) : null;
}

export function spaceMentionDirectory(sid: string): MentionDirectory | null {
  const people = verifiedMembers(sid), agents = verifiedAgents(sid);
  if (!people || !agents) return null;
  // A loaded profile with no name (or a permission-checked 404) is
  // different from a name that is still arriving. A partial directory could
  // silently resolve a short id that is the prefix of a longer name.
  if (people.some(({ userId }) => {
    const state = personState(userId)?.status;
    return state !== "ready" && state !== "hidden";
  })) return null;
  return { members: people.map(({ userId }) => ({ id: userId, displayName: profileName(userId) })), agents };
}

/** Verify agents independently of members, including on re-entry to a cached Space. */
export function beginMentionAgentVisit(sid: string) {
  const live = mentionVisitGuard(sid);
  const verified = signal(false);
  let request = 0;
  let running = false;
  const current = () => live() && agentVisit() === visit;
  const forSend = async (stillSending: () => boolean): Promise<MentionDirectory["agents"] | null> => {
    if (!current() || !stillSending()) return null;
    const ticket = ++request;
    running = true;
    try {
      const ok = await loadSpaceAgentsForVisit(sid, () => current() && stillSending());
      if (!current() || ticket !== request) return null;
      if (stillSending()) verified.set(ok);
      return ok && stillSending() ? agentList(sid)?.agents.filter(a => a.spaceId === sid) ?? null : null;
    } finally {
      if (ticket === request) running = false;
    }
  };
  const refresh = () => { if (!running) void forSend(() => true); };
  const visit = { sid, current, refresh, verified, forSend };
  agentVisit.set(visit);
  refresh();
  return () => {
    if (agentVisit() === visit) agentVisit.set(null);
  };
}

export function refreshMentionAgents(sid: string) {
  const visit = agentVisit();
  if (visit?.sid === sid) visit.refresh();
}

registerMentionDirectory(spaceMentionDirectory, (sid, live) => {
  const visit = agentVisit();
  return visit?.sid === sid ? visit.forSend(live) : Promise.resolve(null);
});
