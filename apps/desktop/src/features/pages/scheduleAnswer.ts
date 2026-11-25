import type { PageSpec } from "../../api/pages";
import { MAX_DEPTH, own, safeName, str } from "./renderPlan";

/** A schedule can only invoke the Space's default agent in a normal room.
 * Find the matching literal button for a form prefill, not an execution. */
export function answerSchedule(spec: PageSpec, key: string) {
  let left = 400;
  const nodes: unknown[] = [];
  const visit = (items: unknown, depth: number) => {
    if (!Array.isArray(items) || depth > MAX_DEPTH) return;
    for (const node of items) {
      if (--left < 0) break;
      nodes.push(node);
      visit(own(node, "children"), depth + 1);
    }
  };
  visit(spec.blocks, 2);
  const answer = nodes.find(n => own(n, "key") === key && own(n, "type") === "AgentAnswer");
  const from = own(own(answer, "props"), "from");
  const path = own(from, "path");
  if (own(from, "k") !== "bind" || !Array.isArray(path) || path.length !== 1 || !safeName(path[0])) return;
  const source = (Array.isArray(spec.data) ? spec.data.slice(0, 16) : []).find(s => own(s, "name") === path[0] && own(s, "source") === "agentAnswer");
  const target = own(source, "params");
  const roomId = str(own(target, "room_id"));
  if (!roomId || own(target, "chat") || own(target, "thread") || own(target, "space_id")) return;
  const linked = str(own(target, "block"));
  const candidates = nodes.flatMap(n => {
    const action = own(n, "action");
    const k = own(n, "key");
    if (own(n, "type") !== "Button" || !safeName(k) || (linked && linked !== k)
      || own(action, "kind") !== "runAgent" || own(action, "room_id") !== roomId || own(action, "space_id") || own(action, "chat") || own(action, "thread")
      || (own(target, "agent_id") && own(action, "agent_id") !== own(target, "agent_id"))) return [];
    const agent = str(own(action, "agent_id")), prompt = str(own(action, "prompt"));
    return agent && prompt ? [{ key: k, agent, prompt, roomId, linked: !!linked }] : [];
  });
  if (candidates.length === 1) return candidates[0];
}
