// The chat models a person may choose as a default in a Space, for the
// shared grouped ModelPicker (src/ui/modelPicker.tsx): their own usable chat
// connections and those shared with them here (GET /v1/models/available
// ?space_id=), each connection's listed models grouped by provider. The
// operator's model is never listed: a Space default is always one of the
// person's connections.

import type { AvailableCatalog } from "../../api/client";
import type { ModelSelection } from "../../api/spaces";
import type { ModelOption } from "../../ui";
import { vendorOf } from "../providerGroups";

/** "No preference": the next rule decides (the Space default, routing, or the default chat connection). */
export const NO_CHOICE = "none";

const SEP = "\u0001";

export function selectionValue(sel: ModelSelection | null | undefined): string {
  if (!sel) return NO_CHOICE;
  return sel.modelId ? `${sel.connectionId}${SEP}${sel.modelId}` : sel.connectionId;
}

export function selectionOf(value: string | null): ModelSelection | null {
  if (!value || value === NO_CHOICE) return null;
  const [connectionId, modelId] = value.split(SEP);
  return modelId ? { connectionId: connectionId!, modelId } : { connectionId: connectionId! };
}

/** Who runs it and who sees it, in a few words. */
function whose(owned: boolean, runsOn: string, ownerId: string | null): string {
  if (owned) return "Your connection";
  if (runsOn === "your_own_sign_in") return "Shared preset: runs on your own sign-in";
  return `Shared by ${ownerId ?? "its owner"}: they can see these requests and may pay for them`;
}

/**
 * The options: "No preference" (described by `noChoiceDetail`), then every
 * chat model of every chat connection the person can use here.
 */
export function spaceModelOptions(c: AvailableCatalog | null, noChoiceDetail: string): ModelOption[] {
  const out: ModelOption[] = [{ value: NO_CHOICE, model: "No preference", detail: noChoiceDetail, title: `No preference — ${noChoiceDetail}` }];
  if (!c) return out;
  if (c.chatProviders) {
    for (const p of c.chatProviders) {
      const v = vendorOf({ id: p.providerId, name: p.providerName });
      for (const conn of p.connections) {
        if (!conn.connectionId || conn.operator) continue;
        const detail = `${conn.name} · ${whose(conn.owned, conn.runsOn, conn.ownerId)}`;
        for (const m of conn.models) {
          const name = m.displayName || m.id;
          const off = !m.available || conn.ready === false;
          out.push({
            value: `${conn.connectionId}${SEP}${m.id}`,
            model: name,
            modelId: m.id,
            detail: off ? `${detail} · not available right now` : detail,
            title: [name, m.id, conn.name, p.providerName].join(" — "),
            provider: { name: v.name, mark: v.mark },
            disabled: off,
          });
        }
      }
    }
    return out;
  }
  // An older server without the grouped list: each chat connection's listed (or pinned) models.
  for (const m of c.models) {
    if (m.capability !== "chat") continue;
    const v = vendorOf({ id: m.providerId, name: m.providerId });
    const detail = `${m.name} · ${whose(m.owned, m.runsOn, m.ownerId)}`;
    const listed = m.chatModels ?? (m.model ? [{ id: m.model, reasoningEfforts: [] }] : []);
    for (const x of listed) {
      out.push({
        value: `${m.connectionId}${SEP}${x.id}`,
        model: x.id,
        detail,
        title: [x.id, m.name].join(" — "),
        provider: { name: v.name, mark: v.mark },
        disabled: m.ready === false,
      });
    }
  }
  return out;
}

/** The value to show for a saved selection; one that isn't listed shows as no value (and is explained: `unlisted`). */
export function shownValue(options: readonly ModelOption[], sel: ModelSelection | null | undefined): string | null {
  const v = selectionValue(sel);
  return options.some((o) => o.value === v) ? v : null;
}

/** A saved selection that isn't among the options (no longer usable here, or saved as a connection only). */
export function unlisted(options: readonly ModelOption[], sel: ModelSelection | null | undefined): boolean {
  return !!sel && !options.some((o) => o.value === selectionValue(sel));
}
