// The wire contract in crates/server/src/pages/{api,spec,data,generate}.rs.
import { ApiError } from "./client";
import type { RequestFn, TurnModelChoice } from "./client";

export type PageTarget = { scope: "personal"; space_id?: never } | { scope: "space"; space_id: string };
export type PageMeta = {
  id: string; scope: "personal" | "space"; space_id: string | null; creator_id: string;
  title: string; icon: string | null; members_edit: boolean; head_version: number;
  created_at: string; updated_at: string; deleted_at: string | null;
  can_edit: boolean; can_delete: boolean; can_manage: boolean; pinned: boolean; position: number | null;
};
export type VersionMeta = {
  version: number; author_id: string; kind: "create" | "manual" | "prompt" | "restore" | "template" | "rename";
  prompt: string | null; model: string | null; restored_from: number | null; created_at: string;
};
export type PageSpec = {
  format: 1; title: string; refresh: string;
  data?: { name: string; source: string; params?: Record<string, unknown> }[];
  blocks?: PageBlock[];
};
export type Expr =
  | { k: "lit"; v: unknown } | { k: "bind"; path: string[] }
  | { k: "not"; a: Expr } | { k: "coalesce"; a: Expr; b: Expr }
  | { k: "cmp"; op: "eq" | "ne" | "lt" | "gt" | "le" | "ge"; a: Expr; b: unknown };
export type PageBlock = {
  type: string; key?: string; props?: Record<string, Expr>; content?: Expr[]; children?: PageBlock[];
  action?: { kind: ActionKind };
};
export type Definition = { source: string; spec: PageSpec };
export type Page = PageMeta & Definition & { version: VersionMeta };
export type Template = { id: string; title: string; description: string; scopes: PageMeta["scope"][] };
export type PageDiagnostic = { line: number; column: number; message: string; key?: string };
export type Validation = { ok: boolean; source: string; spec: PageSpec | null; diagnostics: PageDiagnostic[] };
export type Changes = { added: string[]; removed: string[]; changed: string[]; data: string[]; page: boolean };
export type Draft = Pick<PageMeta, "id" | "scope" | "space_id" | "created_at"> & {
  page_id: string | null; base_version: number | null; block_key: string | null; prompt: string;
  status: "generating" | "ready" | "failed"; source: string | null; spec: PageSpec | null;
  diagnostics: PageDiagnostic[]; error: string | null; model: string | null; attempts: number;
  base_source: string | null; changes: Changes | null; outside_selection: string[];
};
export type PageDraftEvent = {
  draft_id: string; title: string; summary: string; kind: "new" | "edit";
  page_id: string | null; author_id: string; expires_at: string;
};
export type SourceResult = { ok: true; value: unknown } | { ok: false; error: string; message: string };
export type ActionKind = "runAgent" | "runSchedule" | "openRoom" | "openPage" | "refresh";
export type Capability = {
  enabled: boolean; reason?: string; confirm?: boolean; schedule_revision?: number;
  space_id?: string; room_id?: string; page_id?: string; chat?: boolean;
};
export type PageData = { version: number | null; data: Record<string, SourceResult>; actions: Record<string, Capability>; resolved_at: string };
export type Confirmation = {
  page_id: string; version: number; block_key: string; edited_by: string; edited_at: string; runs_as: string;
  action:
    | { kind: "runAgent"; prompt: string; agent: string | null; agent_id: string | null; room: string | null; room_id: string | null; space_id: string | null; chat: boolean }
    | { kind: "runSchedule"; prompt: string; schedule: string; schedule_id: string; schedule_revision: number };
};
export type PageClick = { version: number; block_key: string; confirm?: true; schedule_revision?: number };
type ClickResult = { kind: "runAgent"; started: boolean; error?: string } | { kind: "runSchedule"; run: { status: string; detail: string | null } };
type PageFailure = { error?: string; head_version?: number; schedule_revision?: number; confirmation?: Confirmation; outside_selection?: string[]; diagnostics?: PageDiagnostic[] };

export function pageFailure(error: unknown): PageFailure {
  return error instanceof ApiError && error.details && typeof error.details === "object" ? error.details as PageFailure : {};
}

export function isPageStatus(error: unknown, status: number) {
  return error instanceof ApiError && error.status === status;
}

export function targetOf(page: Pick<PageMeta, "scope" | "space_id">): PageTarget {
  if (page.scope === "personal" && page.space_id === null) return { scope: "personal" };
  if (page.scope === "space" && typeof page.space_id === "string" && page.space_id) return { scope: "space", space_id: page.space_id };
  throw new ApiError("protocol", "Metro returned an invalid page scope.");
}

export function pagesApi(request: RequestFn) {
  const root = "/v1/pages";
  const page = (id: string) => `${root}/${encodeURIComponent(id)}`;
  const draft = (id: string) => `${root}/drafts/${encodeURIComponent(id)}`;
  // The transport parses JSON; the renderer separately treats the tree, data
  // paths and action capabilities as untrusted values.
  async function json<T>(method: Parameters<RequestFn>[0], path: string, body?: unknown): Promise<T> {
    const value = await request(method, path, body);
    if (!value || typeof value !== "object" || Array.isArray(value)) {
      throw new ApiError("protocol", "Metro did not return a Pages response.");
    }
    return value as T;
  }
  const list = async <K extends string, T>(key: K, path: string): Promise<T[]> => {
    const value = (await json<Record<K, T[]>>("GET", path))[key];
    if (!Array.isArray(value)) throw new ApiError("protocol", `Metro did not return a list of ${key}.`);
    return value;
  };
  return {
    listPages: (deleted = false) => list<"pages", PageMeta>("pages", root + (deleted ? "?deleted=true" : "")),
    pageTemplates: () => list<"templates", Template>("templates", `${root}/templates`),
    getPage: (id: string) => json<Page>("GET", page(id)),
    createPage: (body: PageTarget & ({ template: string; room?: string; agent?: string } | { source: string })) =>
      json<Page>("POST", root, body),
    validatePage: (target: PageTarget, source: string) => json<Validation>("POST", `${root}/validate`, { ...target, source }),
    patchPage: (id: string, body: { title?: string; base_version?: number; icon?: string | null; members_edit?: boolean }) =>
      json<Page>("PATCH", page(id), body),
    deletePage: async (id: string) => { await request("DELETE", page(id)); },
    undeletePage: (id: string) => json<Page>("POST", `${page(id)}/undelete`, {}),
    pageVersions: (id: string) => json<{ head_version: number; versions: VersionMeta[] }>("GET", `${page(id)}/versions`),
    pageVersion: (id: string, version: number) => json<VersionMeta & Definition>("GET", `${page(id)}/versions/${version}`),
    savePage: (id: string, base_version: number, source: string) => json<Page>("POST", `${page(id)}/versions`, { base_version, source }),
    restorePageVersion: (id: string, version: number, base_version: number) =>
      json<Page>("POST", `${page(id)}/versions/${version}/restore`, { base_version }),
    pageData: (id: string) => json<PageData>("POST", `${page(id)}/data`, {}),
    pageAction: (id: string, click: PageClick) => json<ClickResult>("POST", `${page(id)}/actions`, click),
    pinPage: (id: string, pinned: boolean, position?: number) =>
      json<Pick<PageMeta, "pinned" | "position">>("PUT", `${page(id)}/prefs`, { pinned, position }),
    createPageDraft: (target: PageTarget | { page_id: string; block_key?: string }, prompt: string, choice?: TurnModelChoice) =>
      json<Draft>("POST", `${root}/drafts`, {
        ...target, prompt, provider_connection_id: choice?.connectionId,
        model_id: choice?.modelId, reasoning_effort: choice?.reasoningEffort,
      }),
    getPageDraft: (id: string) => json<Draft>("GET", draft(id)),
    discardPageDraft: async (id: string) => { await request("DELETE", draft(id)); },
    applyPageDraft: (id: string, confirm_outside = false) => json<Page>("POST", `${draft(id)}/apply`, { confirm_outside }),
    pageDraftData: (id: string) => json<PageData>("POST", `${draft(id)}/data`, {}),
  };
}
