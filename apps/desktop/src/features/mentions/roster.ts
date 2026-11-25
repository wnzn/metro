import { signal } from "nsh";
import { activeSpaceId, inSpaceComposer, me, members, membersError, mentionVisitGuard, mentionVisitVerified, registerMentionDirectory, status } from "../../store";
import { personState, profileName } from "../profile/profileState";
import { agentList, loadSpaceAgentsForVisit } from "../space/agents";
import type { MentionDirectory } from "./draft";

type AgentVisit = { sid: string; current: () => boolean; refresh: () => void; verified: () => boolean };
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
  let running = false;
  const current = () => live() && agentVisit() === visit;
  const refresh = () => {
    if (!current() || running) return;
    running = true;
    void loadSpaceAgentsForVisit(sid, current).then(ok => {
      if (current()) verified.set(ok);
    }).finally(() => { running = false; });
  };
  const visit = { sid, current, refresh, verified };
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

registerMentionDirectory(spaceMentionDirectory);
