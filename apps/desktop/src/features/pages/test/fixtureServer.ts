import { BASE, header, TOKENS, world } from "../../../../test/fakeServer";
import type { Draft, Page, PageData, PageDraftEvent, PageMeta, PageSpec } from "../../../api/pages";
import { bind, lit, sample, sampleData, stamp } from "./sample";

export const TEAM = "pg_team", PERSONAL = "pg_personal", DRAFT = "pgd_fixture";
type Req = { method: string; url: string; headers: unknown; body?: string | null };
const ok = (body: unknown, status = 200) => ({ status, body });
const fail = (status: number, error: string, details = {}) => ok({ error, ...details }, status);
const path = "/v1/pages";
export const sourceOf = (title: string) => `export default (<Page title="${title}"><Text key="body">Fixture</Text></Page>);`;
export function briefSpec(personal: boolean, blockLinked = personal): PageSpec {
  const target = personal ? { chat: true } : { room: "lobby", room_id: "lobby", agent: "agent", agent_id: "sa_fixture" };
  return { format: 1, title: "Daily brief", refresh: "15m", data: [
    { name: "answer", source: "agentAnswer", params: { ...target, ...(blockLinked ? { block: "ask" } : {}) } },
  ], blocks: [
    { type: "AgentAnswer", key: "answer", props: { from: bind("answer") } },
    { type: "Button", key: "ask", props: { label: lit("Write today’s brief") },
      action: { kind: "runAgent", ...target, prompt: "Summarize today" } },
  ] };
}
const pageOf = (id: string, title: string, author: string, sid: string | null, spec = sample): Page => ({
  id, scope: sid ? "space" : "personal", space_id: sid, creator_id: author, title, icon: null, members_edit: false,
  head_version: 1, created_at: stamp, updated_at: stamp, deleted_at: null, pinned: id !== TEAM, position: null,
  can_edit: true, can_delete: true, can_manage: true,
  version: { version: 1, author_id: author, kind: "create", prompt: null, model: null, restored_from: null, created_at: stamp },
  source: sourceOf(title), spec: { ...spec, title },
});
export function pagesFixture() {
  const f = world();
  f.addRoom("acme", { id: "thread-a", name: "Release thread", parent_room_id: "lobby" });
  const team = pageOf(TEAM, "Team dashboard", "alice", "acme");
  const personal = pageOf(PERSONAL, "Alice’s dashboard", "alice", null);
  const entries = new Map([[team.id, team], [personal.id, personal], ["pg_bob", pageOf("pg_bob", "Bob’s dashboard", "bob", null)]]);
  const versions = new Map([...entries].map(([id, p]) => [id, [p]]));
  const privateDrafts = new Map<string, { owner: string; draft: Draft }>();
  const confirmations = new Set<string>();
  const prefs = new Map<string, boolean>();
  const log: { user: string; method: string; path: string; body: any }[] = [];
  let nextId = 1;
  const state = { revision: 3, delayPage: "", delay: 0, dataDelay: 0, draftDelay: 0, previewDelay: 0,
    actionDelay: 0, createDelay: 0, restoreDelay: 0, modelDelay: 0, answer: "", linkedPage: PERSONAL,
    runStatus: "running", runReads: 0, answerStatus: "done", actionError: "", dataError: "", draftError: "", previewError: "", defaultAgent: true,
    denyDraftRead: false, liveRoom: "Renamed room", liveAgent: "renamed_agent", liveAgentName: "Renamed agent",
    stale: false, staleSource: false, outside: false, generating: 0, started: 0, applied: 0 };
  const visible = (p: PageMeta, user: string) => p.scope === "personal" ? p.creator_id === user : !!p.space_id && f.state.spaces.get(p.space_id)?.members.has(user);
  const caps = (p: Page, user: string): Page => {
    const can_manage = p.creator_id === user || (!!p.space_id && f.state.spaces.get(p.space_id)?.owner === user);
    return { ...p, can_edit: can_manage || p.members_edit, can_manage, can_delete: can_manage, pinned: prefs.get(`${user}/${p.id}`) ?? p.pinned };
  };
  function save(id: string, patch: Partial<Page>, kind: Page["version"]["kind"], user: string, restored_from: number | null = null) {
    const p = entries.get(id)!;
    const n = p.head_version + 1;
    const next = { ...p, ...patch, head_version: n, version: { ...p.version, version: n, author_id: user, kind, restored_from } };
    entries.set(id, next);
    versions.get(id)?.unshift(next);
    return next;
  }
  function makeDraft(body: any, user: string, id = `pgd_${nextId++}`) {
    const base = entries.get(body.page_id);
    const space = base ? base.space_id : body.space_id ?? null;
    const spec = { ...sample, title: "Proposed dashboard" };
    const draft: Draft = {
      id, page_id: base?.id ?? null, scope: space ? "space" : "personal", space_id: space, block_key: body.block_key ?? null,
      base_version: base?.head_version ?? null, status: "ready", prompt: body.prompt, created_at: stamp, error: null, attempts: 1, model: "Fixture",
      source: sourceOf("Proposed dashboard"), spec, diagnostics: [], base_source: base?.source ?? null,
      changes: { added: ["stat"], removed: ["old"], changed: ["answer"], data: [], page: false },
      outside_selection: state.outside && body.block_key ? ["answer"] : [],
    };
    privateDrafts.set(id, { owner: user, draft });
    return draft;
  }
  function dataOf(p: Page, user: string): PageData {
    const confirm = !confirmations.has(`${user}/${p.id}/${p.head_version}`);
    return { ...sampleData, version: p.head_version,
      data: state.answer || (p.id === PERSONAL && state.started) ? { ...sampleData.data, answer: { ok: true, value: {
        text: state.answer || (state.answerStatus === "done" ? "Private page-only answer" : ""), status: state.answerStatus,
      } } } : sampleData.data,
      actions: { ...sampleData.actions, other: { enabled: true, page_id: state.linkedPage }, ask: { enabled: true, confirm }, run: p.scope === "personal" ? { enabled: false, reason: "Space only" } : { enabled: true, confirm, schedule_revision: state.revision } } };
  }
  function handle(req: Req) {
    const url = new URL(req.url), pathname = url.pathname;
    if (!pathname.startsWith(path)) {
      const response = f.network[`${req.method} ${BASE}/v1/*`]!(req);
      if (pathname === "/v1/models/available") {
        log.push({ user: "", method: req.method, path: pathname, body: null });
        return typeof response === "object" && response !== null ? { ...response, delay: state.modelDelay } : response;
      }
      if (pathname === "/v1/spaces/acme/agents" && req.method === "GET") {
        return ok({ agents: [{ id: "sa_fixture", space_id: "acme", handle: "agent", name: "Agent",
          description: "Fixture", is_default: state.defaultAgent, instructions: "", tools: [] }] });
      }
      if (pathname === "/v1/runs/run_page_fixture" && req.method === "GET") {
        state.runReads++;
        return ok({ id: "run_page_fixture", space_id: "acme", room_id: "lobby", status: state.runStatus, actor: "alice", task: "brief" });
      }
      if (pathname === "/v1/schedules" && req.method === "GET") return ok({ tasks: [] });
      return response;
    }
    const token = header(req, "authorization") ?? "";
    const user = Object.keys(TOKENS).find(u => `Bearer ${TOKENS[u]}` === token);
    if (!user) return fail(401, "unauthenticated");
    const body = req.body ? JSON.parse(req.body) : {};
    const method = req.method;
    log.push({ user, method, path: pathname, body });
    const [, , , id, op, detail, extra] = pathname.split("/");
    const create = (p: Page) => { entries.set(p.id, p); versions.set(p.id, [p]); return ok(caps(p, user), 201); };
    if (!id) {
      if (method === "GET") {
        const deleted = url.searchParams.get("deleted") === "true";
        return ok({ pages: [...entries.values()].filter(p => visible(p, user) && !!p.deleted_at === deleted)
          .map(p => { const { source: _source, spec: _spec, version: _version, ...meta } = caps(p, user); return meta; }) });
      }
      return { ...create(pageOf(`pg_${nextId++}`, body.template === "files" ? "Recent files" : "Fixture page", user, body.space_id ?? null)),
        delay: state.createDelay };
    }
    if (id === "templates") return ok({ templates: [
      { id: "files", title: "Files", description: "Recent library files", scopes: ["personal", "space"] },
      { id: "release-watch", title: "Release watch", description: "Watch a Space", scopes: ["space"] },
    ] });
    if (id === "validate") return ok(body.source.includes("INVALID")
      ? { ok: false, source: body.source, spec: null, diagnostics: [{ line: 1, column: 1, message: "Unsupported fixture source" }] }
      : { ok: true, source: body.source, spec: team.spec, diagnostics: [] });
    if (id === "drafts") {
      if (!op && method === "POST") return ok(makeDraft(body, user), 202);
      const found = op && privateDrafts.get(op);
      if (!found || found.owner !== user || (found.draft.space_id && !f.state.spaces.get(found.draft.space_id)?.members.has(user))) return fail(404, "no such draft");
      const d = found.draft, base = d.page_id ? entries.get(d.page_id) : undefined;
      if (method === "DELETE") { privateDrafts.delete(op!); return { status: 204 }; }
      if (detail !== "apply" && state.denyDraftRead) return { ...fail(404, "no such draft"), delay: state.draftDelay };
      if (detail === "data") {
        if (state.previewError) return fail(503, state.previewError);
        const { version: _version, ...fixture } = sampleData;
        return { ...ok({ ...fixture, data: state.answer ? { ...fixture.data, answer: { ok: true, value: { text: state.answer, status: "done" } } } : fixture.data }),
          delay: state.previewDelay };
      }
      if (detail === "apply") {
        if (base && !caps(base, user).can_edit) return fail(403, "no edit rights");
        if (d.outside_selection.length && !body.confirm_outside) return fail(409, "outside selection", { outside_selection: d.outside_selection });
        if (state.stale && base) {
          const changed = save(base.id, { source: sourceOf("New head from another editor") }, "manual", "bob");
          state.stale = false;
          return fail(409, "the page changed", { head_version: changed.head_version });
        }
        if (base && base.head_version !== d.base_version) return fail(409, "the page changed", { head_version: base.head_version });
        state.applied++;
        privateDrafts.delete(op!);
        if (!base) return create(pageOf(`pg_${nextId++}`, d.spec!.title, user, d.space_id, d.spec!));
        return ok(caps(save(base.id, { source: d.source!, spec: d.spec!, title: d.spec!.title }, "prompt", user), user), 201);
      }
      if (state.generating > 0) {
        state.generating--;
        return { ...ok({ ...d, status: "generating", spec: null, source: null }), delay: state.draftDelay };
      }
      return { ...ok(state.draftError ? { ...d, status: "failed", error: state.draftError, spec: null, source: null } : d), delay: state.draftDelay };
    }
    const page = entries.get(id);
    if (!page || !visible(page, user) || (page.deleted_at && op !== "undelete")) return fail(404, "no such page");
    const view = caps(page, user);
    if (!op && method === "PATCH") {
      if (body.base_version && body.base_version !== page.head_version) return fail(409, "head changed", { head_version: page.head_version });
      return ok(caps(save(id, { title: body.title ?? page.title, icon: body.icon ?? null, members_edit: body.members_edit ?? page.members_edit }, "rename", user), user));
    }
    if (!op && method === "DELETE") { entries.set(id, { ...page, deleted_at: new Date().toISOString() }); return { status: 204 }; }
    if (!op) return { ...ok(view), delay: state.delayPage === id ? state.delay : 0 };
    if (op === "prefs") { prefs.set(`${user}/${id}`, body.pinned); return ok({ pinned: body.pinned, position: null }); }
    if (op === "undelete") {
      entries.set(id, { ...page, deleted_at: null });
      return { ...ok(caps(entries.get(id)!, user)), delay: state.restoreDelay };
    }
    if (op === "data") return { ...(state.dataError ? fail(503, state.dataError) : ok(dataOf(page, user))), delay: state.dataDelay };
    if (op === "versions") {
      if (!detail && method === "POST") {
        if (state.staleSource) {
          state.staleSource = false;
          const changed = save(id, { source: sourceOf("Another editor published this") }, "manual", "bob");
          return fail(409, "head changed", { head_version: changed.head_version });
        }
        if (body.base_version !== page.head_version) return fail(409, "head changed", { head_version: page.head_version });
        return ok(caps(save(id, { source: body.source }, "manual", user), user), 201);
      }
      const old = versions.get(id) ?? [];
      if (!detail) return ok({ head_version: page.head_version, versions: old.map(v => v.version) });
      const recorded = old.find(p => p.head_version === Number(detail));
      if (!recorded) return fail(404, "no such version");
      if (extra === "restore") {
        if (body.base_version !== page.head_version) return fail(409, "head changed", { head_version: page.head_version });
        return ok(caps(save(id, { source: recorded.source, spec: recorded.spec, title: recorded.title }, "restore", user, recorded.head_version), user), 201);
      }
      return ok({ ...recorded.version, source: recorded.source, spec: recorded.spec });
    }
    if (op === "actions") {
      if (body.version !== page.head_version) return fail(409, "head changed", { head_version: page.head_version });
      const key = `${user}/${id}/${page.head_version}`;
      const schedule = body.block_key === "run";
      if (!confirmations.has(key) && !body.confirm) return fail(428, "confirmation required", {
        confirmation: { page_id: id, version: page.head_version, block_key: body.block_key, edited_at: stamp, edited_by: page.version.author_id, runs_as: user,
          action: schedule ? { kind: "runSchedule", schedule: "Release reminder", schedule_id: "sch_fixture", schedule_revision: state.revision,
            prompt: "Send the exact release summary", space_id: page.space_id, room_id: "lobby", room: state.liveRoom }
            : { kind: "runAgent", prompt: "Write a private brief", agent: page.space_id ? state.liveAgent : null,
              agent_name: page.space_id ? state.liveAgentName : undefined, agent_id: page.space_id ? "sa_releases" : null,
              room: page.space_id ? state.liveRoom : null, room_id: page.space_id ? "lobby" : null, space_id: page.space_id, chat: !page.space_id },
        },
      });
      if (schedule && body.schedule_revision !== state.revision) return fail(409, "schedule changed", { schedule_revision: state.revision });
      confirmations.add(key);
      state.started++;
      return { ...(schedule ? ok({ kind: "runSchedule", run: { status: "done", detail: null } })
        : ok({ kind: "runAgent", started: !state.actionError, error: state.actionError || undefined, run_id: "run_page_fixture", room_id: "lobby" }, 202)),
        delay: state.actionDelay };
    }
    return fail(404, "unknown Pages fixture request");
  }
  const network = Object.fromEntries(["GET", "POST", "PUT", "PATCH", "DELETE"].map(method => [`${method} ${BASE}/v1/*`, handle]));
  function propose(author = "alice", missing = false) {
    if (!missing) makeDraft({ page_id: TEAM, prompt: "Add a release summary" }, author, DRAFT);
    const page_draft: PageDraftEvent = {
      draft_id: missing ? "pgd_missing" : DRAFT, title: missing ? "Gone draft" : "Proposed release page", summary: "Draft proposed for the person who asked to review.",
      kind: "edit", page_id: TEAM, author_id: author, expires_at: "2099-11-25T00:00:00.000Z",
    };
    const activity = { id: missing ? "run_gone" : "run_fixture", source: "agent" as const, name: "Release bot", page_draft };
    return f.post("acme/thread-a", { kind: "page_draft", actor: "agent", text: page_draft.summary, activity });
  }
  function replaceTeam(spec: PageSpec) {
    entries.set(TEAM, { ...entries.get(TEAM)!, spec });
  }
  return { f, network, entries, log, state, propose, replaceTeam, calls: (suffix: string, method = "GET") => log.filter(r => r.path === `${path}${suffix}` && r.method === method) };
}
