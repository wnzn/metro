import { activeSpaceId, inSpaceComposer, me, members, membersError, mentionVisitVerified, registerMentionDirectory, status } from "../../store";
import { personState, profileName } from "../profile/profileState";
import { agentList } from "../space/agents";
import type { MentionDirectory } from "./draft";

export function verifiedMembers(sid: string) {
  if (activeSpaceId() !== sid || !inSpaceComposer() || !mentionVisitVerified(sid) || status().phase !== "online" || membersError()) return null;
  const list = members();
  return list?.some(m => m.userId === me()) ? list : null;
}

export function verifiedAgents(sid: string) {
  if (!verifiedMembers(sid)) return null;
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

registerMentionDirectory(spaceMentionDirectory);
