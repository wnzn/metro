import type { PageSpec, PageValues } from "../../api/pages";
import { own, safeName, str } from "./renderPlan";

export function linkedAnswerNames(spec: PageSpec, button: string) {
  return (Array.isArray(spec.data) ? spec.data.slice(0, 16) : [])
    .filter(source => own(source, "source") === "agentAnswer" && own(own(source, "params"), "block") === button)
    .map(source => own(source, "name")).filter(safeName);
}

export function answerState(values: PageValues | null, name: string) {
  const result = own(values?.data, name), value = own(result, "value");
  if (own(result, "ok") !== true || !value) return;
  const status = str(own(value, "status"));
  return { status, stamp: JSON.stringify([status, own(value, "at"), str(own(value, "text")), str(own(value, "error"))]) };
}

export const terminalAnswer = (status: string) => ["done", "failed", "cancelled", "interrupted"].includes(status);
