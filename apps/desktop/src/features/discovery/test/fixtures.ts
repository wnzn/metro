// Test-only: search and schedule routes layered over the shared fake server
// (test/fakeServer.ts), as crates/server/src/search/API.md and schedules.rs
// describe them. Membership is the fake server's own. A test double, not the
// server: next firings are fixed values a test sets, not computed.

import { BASE, header, TOKENS, world } from "../../../../test/fakeServer";

type Req = { method: string; url: string; headers: unknown; body?: string | null };
export type Hit = { owner?: string; space_id: string | null; room_id: string | null; event_id: number; actor: string; text: string; score: number };
type Task = Record<string, unknown> & { id: string; owner: string; enabled: boolean; name: string };
type RunRow = { id: number; due_at: string; status: string; detail: string | null; claimed_at: string; finished_at: string | null };

export function discoveryWorld() {
  const f = world();
  let next = 1;
  const d = {
    /** Personal Chat hits carry `owner`; Space hits are visible to that Space's members. */
    corpus: [] as Hit[],
    ranking: "classifier" as "classifier" | "embeddings" | "mixed",
    partial: false,
    notice: null as string | null,
    /** Answer every search with this status and error, when set. */
    fail: null as { status: number; error: string } | null,
    remaining: 0,
    tasks: [] as Task[],
    runs: new Map<string, RunRow[]>(),
    /** What the server would compute as the next firing, per task id. */
    nextCron: { utc: "2026-11-24T01:00:00Z", local: "2026-11-24T09:00:00+08:00" },
    log: [] as { user: string; method: string; path: string; body: any }[],
  };

  const userOf = (req: Req) => {
    const auth = header(req, "authorization") ?? "";
    return Object.keys(TOKENS).find((u) => auth === `Bearer ${TOKENS[u]}`);
  };
  const ok = (body: unknown, status = 200) => ({ status, body });
  const err = (status: number, error: string) => ({ status, body: { error } });
  const member = (user: string, sid: string) => !!f.state.spaces.get(sid)?.members.has(user);
  const sources = (user: string, scope: string, sid: string | undefined) =>
    d.corpus.filter((h) => {
      const chat = h.space_id === null;
      if (chat) return h.owner === user && (scope === "chat" || scope === "all");
      if (!member(user, h.space_id!)) return false;
      return scope === "all" || (scope === "space" && h.space_id === sid);
    });

  const view = (t: Task) => {
    const { owner: _o, ...rest } = t;
    return rest;
  };

  function handle(req: Req): unknown | undefined {
    const url = new URL(req.url);
    const path = url.pathname;
    if (!path.startsWith("/v1/search") && !path.startsWith("/v1/schedules")) return undefined;
    const user = userOf(req);
    if (!user) return err(401, "invalid token");
    const body = req.body ? JSON.parse(req.body) : null;
    d.log.push({ user, method: req.method, path, body });

    if (path === "/v1/search" || path === "/v1/search/index") {
      if (req.method !== "POST") return err(405, "method not allowed");
      const scope = body?.scope;
      if (scope !== "chat" && scope !== "space" && scope !== "all") return err(400, "scope must be chat, space or all");
      if (scope === "space" ? !body.space_id : body.space_id !== undefined) return err(400, "space_id goes with scope space only");
      if (scope === "space" && !member(user, body.space_id)) return err(404, "space not found");
      if (path === "/v1/search/index") {
        const indexed = Math.min(d.remaining, 64);
        d.remaining -= indexed;
        return ok({ indexed, skipped: 0, remaining: d.remaining, done: d.remaining === 0 });
      }
      if (d.fail) return err(d.fail.status, d.fail.error);
      if (!String(body.query ?? "").trim()) return err(400, "query must not be empty");
      const results = sources(user, scope, body.space_id)
        .sort((a, b) => b.score - a.score)
        .slice(0, body.limit ?? 10)
        .map(({ owner: _o, ...hit }) => hit);
      return ok({ results, ranking: d.ranking, partial: d.partial, ...(d.notice ? { notice: d.notice } : {}) });
    }

    if (path === "/v1/schedules" && req.method === "GET") return ok({ tasks: d.tasks.filter((t) => t.owner === user).map(view) });
    if (path === "/v1/schedules" && req.method === "POST") {
      const checked = check(user, body);
      if ("error" in checked) return err(400, checked.error);
      const task = { id: `st${next++}`, owner: user, ...checked.task } as Task;
      d.tasks.push(task);
      return ok(view(task), 201);
    }
    const m = /^\/v1\/schedules\/([^/]+)(\/history)?$/.exec(path);
    if (m) {
      const task = d.tasks.find((t) => t.id === decodeURIComponent(m[1]) && t.owner === user);
      if (!task) return err(404, "no such scheduled task");
      if (m[2] && req.method === "GET") return ok({ runs: d.runs.get(task.id) ?? [] });
      if (req.method === "DELETE") {
        d.tasks = d.tasks.filter((t) => t !== task);
        return { status: 204, body: "" };
      }
      if (req.method === "PATCH") {
        const merged = { ...task, ...body, schedule: body.schedule ?? task.schedule };
        const checked = check(user, merged);
        if ("error" in checked) return err(400, checked.error);
        Object.assign(task, checked.task);
        return ok(view(task));
      }
    }
    return err(404, "not found");
  }

  function check(user: string, b: any): { task: Record<string, unknown> } | { error: string } {
    if (!b?.name?.trim()) return { error: "name must be 1..=100 printable characters" };
    if (!b?.prompt?.trim()) return { error: "prompt must be 1..=4000 characters" };
    if ((b.space_id == null) !== (b.room_id == null)) return { error: "space_id and room_id go together" };
    if (b.space_id != null && !member(user, b.space_id)) return { error: "you are not a member of that Space" };
    const s = b.schedule;
    let next_run_at: string | null;
    let next_run_local: string | null = null;
    let schedule: Record<string, unknown>;
    if (s?.kind === "once") {
      const at = Date.parse(s.at);
      if (!/[zZ]|[+-]\d{2}:\d{2}$/.test(s.at ?? "") || Number.isNaN(at)) return { error: "schedule.at must be an RFC 3339 timestamp with an offset, such as 2026-11-23T09:00:00+08:00" };
      const utc = new Date(at).toISOString().replace(/\.\d{3}Z$/, "Z");
      schedule = { kind: "once", at: utc };
      next_run_at = utc;
    } else if (s?.kind === "cron") {
      if (!/^[A-Za-z_]+(\/[A-Za-z_]+)*$/.test(s.timezone ?? "") || s.timezone === "Mars/Base") return { error: `unknown time zone \`${s.timezone}\`; use an IANA zone such as Asia/Manila` };
      if (String(s.expression ?? "").trim().split(/\s+/).length !== 5) return { error: "a cron expression has 5 fields" };
      schedule = { kind: "cron", expression: s.expression, timezone: s.timezone };
      next_run_at = d.nextCron.utc;
      next_run_local = d.nextCron.local;
    } else return { error: "schedule.kind must be `once` or `cron`" };
    const enabled = b.enabled !== false;
    return {
      task: {
        name: b.name.trim(),
        prompt: b.prompt.trim(),
        space_id: b.space_id ?? null,
        room_id: b.room_id ?? null,
        schedule,
        enabled,
        next_run_at: enabled ? next_run_at : null,
        next_run_local: enabled ? next_run_local : null,
        last_status: b.last_status ?? null,
        last_run_at: b.last_run_at ?? null,
        last_detail: b.last_detail ?? null,
      },
    };
  }

  const network: Record<string, (req: Req) => unknown> = {};
  for (const method of ["GET", "POST", "PUT", "PATCH", "DELETE"]) {
    const base = f.network[`${method} ${BASE}/v1/*`];
    network[`${method} ${BASE}/v1/*`] = (req: Req) => handle(req) ?? base(req);
  }
  return { f, d, network };
}
