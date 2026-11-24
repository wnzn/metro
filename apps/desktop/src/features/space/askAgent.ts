// Which agent the room's Ask agent asks, per Space, for this session.
//
// - With one agent (or before the list loads) Ask agent sends no agent id:
//   the server starts the Space's default agent (or, when the person opted
//   in, their classifier's pick), as it always did.
// - With several, the default agent is preselected and its id is sent; the
//   person may pick another, or "Let the classifier choose" when their own
//   preferences here allow it (classifier available and classification on):
//   then no id is sent, and the server's System One Choice picks.
// - A choice that no longer exists (the agent was deleted) falls back to
//   the default. The person's preferences are read with their own
//   credential; a failure just hides the classifier option.

import { batch, signal, untracked } from "nsh";
import { ApiError } from "../../api/client";
import { apiClient, disconnect, isAborted, onSessionEnd, sessionGuard } from "../../store";
import { defaultAgent, spaceAgents } from "./agents";

/** The value meaning "Let the classifier choose". */
export const CLASSIFIER_CHOICE = "classifier";

const chosen = signal<Record<string, string>>({});
/** Space id → whether this person's classifier may choose there (from their Space preferences). */
const classifierBySpace = signal<Record<string, boolean>>({});
const prefsAsked = new Set<string>();
let generation = 0;

/** Whether Ask agent offers a choice in this Space (it has more than one agent). Reactive. */
export function hasAgentChoice(spaceId: string | null | undefined): boolean {
  return spaceAgents(spaceId).length > 1;
}

/** Whether "Let the classifier choose" is offered here. Reactive; loads the person's preferences once. */
export function classifierOffered(spaceId: string | null | undefined): boolean {
  if (!spaceId) return false;
  const known = classifierBySpace()[spaceId];
  if (known === undefined && !prefsAsked.has(spaceId)) untracked(() => void loadClassifier(spaceId));
  return known === true;
}

/** The current choice: an agent id, CLASSIFIER_CHOICE, or null when there is nothing to choose. Reactive. */
export function askChoice(spaceId: string | null | undefined): string | null {
  if (!spaceId || !hasAgentChoice(spaceId)) return null;
  const c = chosen()[spaceId];
  if (c === CLASSIFIER_CHOICE && classifierOffered(spaceId)) return c;
  if (c && spaceAgents(spaceId).some((a) => a.id === c)) return c;
  return defaultAgent(spaceId)?.id ?? spaceAgents(spaceId)[0]?.id ?? null;
}

export function chooseAskAgent(spaceId: string, choice: string) {
  chosen.set({ ...untracked(chosen), [spaceId]: choice });
}

/** The agent id Ask agent sends: undefined for the classifier, or when there is no choice. */
export function askAgentId(spaceId: string | null | undefined): string | undefined {
  const c = askChoice(spaceId);
  return c && c !== CLASSIFIER_CHOICE ? c : undefined;
}

/** Forgets what is known of a Space's classifier preference (after the person changed it in Space settings). */
export function forgetAskPrefs(spaceId: string) {
  prefsAsked.delete(spaceId);
  const { [spaceId]: _gone, ...rest } = untracked(classifierBySpace);
  classifierBySpace.set(rest);
}

/** Records the person's preference as Space settings just saved it. */
export function noteClassifierPrefs(spaceId: string, offered: boolean) {
  prefsAsked.add(spaceId);
  classifierBySpace.set({ ...untracked(classifierBySpace), [spaceId]: offered });
}

async function loadClassifier(spaceId: string): Promise<void> {
  const api = apiClient();
  if (!api) return;
  prefsAsked.add(spaceId);
  const gen = generation;
  const ok = sessionGuard();
  try {
    const p = await api.spacePrefs(spaceId);
    if (!ok() || gen !== generation) return;
    noteClassifierPrefs(spaceId, p.classifierAvailable && p.classifyUnaddressed);
  } catch (error) {
    if (!ok() || gen !== generation || isAborted(error)) return;
    if (error instanceof ApiError && error.kind === "auth") return disconnect(error.message);
    noteClassifierPrefs(spaceId, false);
  }
}

onSessionEnd(() => {
  generation++;
  prefsAsked.clear();
  batch(() => {
    chosen.set({});
    classifierBySpace.set({});
  });
});
