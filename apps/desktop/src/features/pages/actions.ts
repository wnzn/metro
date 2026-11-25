import type { ActionKind, Confirmation, Page, PageClick } from "../../api/pages";
import { own } from "./renderPlan";

export const revision = (value: unknown): value is number => typeof value === "number" && Number.isSafeInteger(value) && value >= 0;

/** Only a challenge for the currently displayed button and signed-in person
 * can supply consent. The exact destination and prompt come from the server,
 * and are never sent back by the client. */
export function checkedConfirmation(value: unknown, page: Page, key: string, kind: ActionKind, user: string | null): Confirmation | null {
  if (!user || own(value, "page_id") !== page.id || own(value, "version") !== page.head_version
    || own(value, "block_key") !== key || own(value, "runs_as") !== user
    || typeof own(value, "edited_by") !== "string" || typeof own(value, "edited_at") !== "string") return null;
  const action = own(value, "action");
  const prompt = own(action, "prompt");
  if (own(action, "kind") !== kind || typeof prompt !== "string" || !prompt.trim()) return null;
  if (kind === "runSchedule") {
    if (!page.space_id || typeof own(action, "schedule") !== "string" || typeof own(action, "schedule_id") !== "string"
      || own(action, "space_id") !== page.space_id || typeof own(action, "room_id") !== "string" || typeof own(action, "room") !== "string"
      || !revision(own(action, "schedule_revision"))) return null;
  } else if (kind === "runAgent") {
    if (page.scope === "personal") {
      if (own(action, "chat") !== true || own(action, "space_id") !== null
        || own(action, "room_id") != null || own(action, "agent_id") != null) return null;
    } else if (own(action, "chat") !== false || own(action, "space_id") !== page.space_id
      || typeof own(action, "room") !== "string" || typeof own(action, "agent") !== "string"
      || typeof own(action, "room_id") !== "string" || typeof own(action, "agent_id") !== "string") return null;
  } else return null;
  return value as Confirmation;
}

export function actionBody(page: Page, key: string, kind: ActionKind, scheduleRevision: unknown, confirmed?: Confirmation): PageClick | null {
  if (!revision(page.head_version) || page.version?.version !== page.head_version || !key
    || (kind !== "runAgent" && kind !== "runSchedule")) return null;
  if (kind === "runSchedule" && (page.scope !== "space" || !page.space_id || !revision(scheduleRevision))) return null;
  const click: PageClick = { version: page.head_version, block_key: key };
  if (kind === "runSchedule") click.schedule_revision = scheduleRevision as number;
  if (confirmed) {
    if (confirmed.version !== click.version || confirmed.block_key !== key || confirmed.page_id !== page.id || confirmed.action.kind !== kind) return null;
    if (confirmed.action.kind === "runSchedule" && confirmed.action.schedule_revision !== click.schedule_revision) return null;
    click.confirm = true;
  }
  return click;
}
