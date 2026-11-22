// Test-only: Documents & media routes layered over the shared fake server
// (test/fakeServer.ts), as crates/server/src/library.rs and
// library_search.rs answer them. Membership is the fake server's own. A test
// double, not the server: search results are what a test seeds, filtered by
// access the way the server filters them.

import { BASE, header, TOKENS, world } from "../../../../test/fakeServer";

type Req = { method: string; url: string; headers: unknown; body?: string | null };
export type FakeAsset = { id: string; name: string; mime_type: string; kind: string; size_bytes: number; space_id: string | null; uploader_id: string; sha256: string; created_at: number; bytes: string };
export type FakeHit = { asset_id: string; locator: Record<string, unknown>; excerpt?: string; score: number; score_kind: "classifier" | "cosine" };

const EXT: Record<string, { kind: string; mimes: string[] }> = {
  txt: { kind: "text", mimes: ["text/plain"] },
  md: { kind: "markdown", mimes: ["text/markdown", "text/x-markdown"] },
  pdf: { kind: "pdf", mimes: ["application/pdf"] },
  png: { kind: "png", mimes: ["image/png"] },
  jpg: { kind: "jpeg", mimes: ["image/jpeg"] },
  wav: { kind: "wav", mimes: ["audio/wav"] },
  mp3: { kind: "mp3", mimes: ["audio/mpeg"] },
  mp4: { kind: "mp4", mimes: ["video/mp4"] },
  webm: { kind: "webm", mimes: ["video/webm"] },
};

export function libraryWorld() {
  const f = world();
  let next = 1;
  const uuid = () => `00000000-0000-4000-8000-${String(next++).padStart(12, "0")}`;
  const l = {
    assets: [] as FakeAsset[],
    hits: [] as FakeHit[],
    ranking: "mixed" as "classifier" | "embeddings" | "mixed",
    settings: new Map<string, string | null>(),
    /** Server-configured file limit (bytes). */
    maxFileBytes: 8 * 1024 * 1024,
    /** Delay (ms) for content reads, to test late answers. */
    contentDelay: 0,
    log: [] as { user: string; method: string; url: string; path: string; headers: Record<string, string>; body: string | null }[],
  };

  const userOf = (req: Req) => {
    const auth = header(req, "authorization") ?? "";
    return Object.keys(TOKENS).find((u) => auth === `Bearer ${TOKENS[u]}`);
  };
  const ok = (body: unknown, status = 200) => ({ status, body });
  const err = (status: number, error: string) => ({ status, body: { error } });
  const member = (user: string, sid: string) => !!f.state.spaces.get(sid)?.members.has(user);
  const visible = (user: string, a: FakeAsset) => (a.space_id === null ? a.uploader_id === user : member(user, a.space_id));
  const view = ({ bytes: _b, ...a }: FakeAsset) => a;

  /** Adds a file directly (as if its uploader had uploaded it). */
  const addAsset = (uploader: string, space_id: string | null, name: string, bytes: string) => {
    const ext = name.split(".").pop()!.toLowerCase();
    const e = EXT[ext]!;
    const a: FakeAsset = { id: uuid(), name, mime_type: e.mimes[0]!, kind: e.kind, size_bytes: bytes.length, space_id, uploader_id: uploader, sha256: "ab".repeat(32), created_at: 1_700_000_000_000 + next, bytes };
    l.assets.unshift(a);
    return a;
  };

  /** A media-capable embeddings connection (embeddinggemma-2 / device-embeddinggemma-2). */
  const addMediaConnection = (owner: string, provider_id: "embeddinggemma-2" | "device-embeddinggemma-2", name: string, share: { visibility: "personal" | "spaces" | "organization"; space_ids: string[] } = { visibility: "personal", space_ids: [] }) => {
    const id = `media-${owner}-${next++}`;
    f.state.connections.set(id, { id, owner, name, provider_id, base_url: null, model: "google/embeddinggemma-2", is_default: false, key: null, deviceToken: null, capability: "embeddings", share } as any);
    return id;
  };

  function handle(req: Req): unknown | undefined {
    const url = new URL(req.url);
    const path = url.pathname;
    if (!path.startsWith("/v1/library")) return undefined;
    const user = userOf(req);
    const headers: Record<string, string> = {};
    const h = req.headers as any;
    if (h && !Array.isArray(h)) for (const k of Object.keys(h)) headers[k.toLowerCase()] = String(h[k]);
    l.log.push({ user: user ?? "", method: req.method, url: req.url, path, headers, body: req.body ?? null });
    if (!user) return err(401, "invalid token");
    const q = url.searchParams;
    const body = () => (req.body ? JSON.parse(req.body) : null);

    if (path === "/v1/library/assets" && req.method === "GET") {
      const scope = q.get("scope") ?? "all";
      const sid = q.get("space_id");
      if (scope === "space" && (!sid || !member(user, sid))) return err(404, "space not found");
      const list = l.assets.filter((a) => visible(user, a) && (scope === "all" || (scope === "personal" ? a.space_id === null : a.space_id === sid)));
      return ok({ assets: list.map(view), next_cursor: null, max_file_bytes: l.maxFileBytes });
    }
    if (path === "/v1/library/assets" && req.method === "POST") {
      const scope = q.get("scope");
      const sid = q.get("space_id");
      const name = q.get("name") ?? "";
      if (scope !== "personal" && scope !== "space") return err(400, "upload to scope=personal or scope=space");
      if (scope === "space" && (!sid || !member(user, sid))) return err(404, "space not found");
      const ext = name.split(".").pop()!.toLowerCase();
      const type = (header(req, "content-type") ?? "").split(";")[0]!.trim();
      const e = EXT[ext];
      if (!e || !e.mimes.includes(type)) return err(415, "the file's type, extension and contents don't agree");
      const bytes = req.body ?? "";
      if (bytes.length > l.maxFileBytes) return err(413, `files are limited to ${l.maxFileBytes} bytes`);
      if (!bytes.length) return err(415, "the file is empty");
      const a = addAsset(user, scope === "space" ? sid : null, name, bytes);
      a.mime_type = e.mimes[0]!;
      return ok(view(a), 201);
    }
    const m = /^\/v1\/library\/assets\/([^/]+)(\/content)?$/.exec(path);
    if (m) {
      const a = l.assets.find((x) => x.id === decodeURIComponent(m[1]!));
      if (!a || !visible(user, a)) return err(404, "asset not found");
      if (m[2] && req.method === "GET") {
        const answer = { status: 200, headers: { "content-type": a.mime_type, "cache-control": "no-store" }, body: a.bytes };
        return l.contentDelay ? new Promise((resolve) => setTimeout(() => resolve(visible(user, a) ? answer : err(404, "asset not found")), l.contentDelay)) : answer;
      }
      if (req.method === "GET") return ok(view(a));
      if (req.method === "DELETE") {
        const owner = a.space_id !== null && f.state.spaces.get(a.space_id)?.owner === user;
        if (a.uploader_id !== user && !owner) return err(403, "only the uploader or the Space owner can delete this file");
        l.assets = l.assets.filter((x) => x.id !== a.id);
        return { status: 204, body: "" };
      }
    }
    if (path === "/v1/library/settings") {
      if (req.method === "GET") return ok({ embedding_id: l.settings.get(user) ?? null });
      if (req.method === "PUT") {
        const id = body()?.embedding_id ?? null;
        const c = id ? (f.state.connections.get(id) as any) : null;
        if (id && (!c || !String(c.provider_id).includes("embeddinggemma-2"))) return err(422, "that connection cannot embed documents, images, audio or video");
        l.settings.set(user, id);
        return ok({ embedding_id: id });
      }
    }
    if (path === "/v1/library/index" && req.method === "POST") {
      if (!l.settings.get(user)) return err(409, "choose a media embedding model in Settings to search your files");
      const id = body()?.asset_id;
      const a = l.assets.find((x) => x.id === id);
      if (!a || !visible(user, a)) return err(404, "asset not found");
      return ok({ indexed_segments: 3, partial: false, items: [{ asset_id: a.id, status: "indexed", indexed_segments: 3, partial: false }] });
    }
    if (path === "/v1/library/search" && req.method === "POST") {
      const b = body();
      if (b.scope === "space" && !member(user, b.space_id)) return err(404, "space not found");
      if (!l.settings.get(user)) return err(409, "choose a media embedding model in Settings to search your files");
      const results = l.hits
        .map((hit) => ({ hit, a: l.assets.find((x) => x.id === hit.asset_id) }))
        .filter(({ a }) => !!a && visible(user, a) && (b.scope === "all" || (b.scope === "personal" ? a.space_id === null : a.space_id === b.space_id)))
        .map(({ hit, a }) => ({ ...hit, name: a!.name, mime_type: a!.mime_type, kind: a!.mime_type.startsWith("image/") ? "image" : a!.mime_type.startsWith("video/") ? "video" : a!.mime_type.startsWith("audio/") ? "audio" : "document", space_id: a!.space_id }));
      return ok({ results, ranking: l.ranking, partial: false });
    }
    return err(404, "not found");
  }

  const network: Record<string, (req: Req) => unknown> = {};
  for (const method of ["GET", "POST", "PUT", "PATCH", "DELETE"]) {
    const base = f.network[`${method} ${BASE}/v1/*`]!;
    network[`${method} ${BASE}/v1/*`] = (req: Req) => {
      const mine = handle(req);
      if (mine !== undefined) return mine;
      const answer = base(req) as any;
      // The server's catalog says which embeddings adapters take media.
      if (new URL(req.url).pathname === "/v1/models/available" && answer?.body?.models) {
        answer.body.models = answer.body.models.map((m: any) => ({
          ...m,
          media_inputs: String(m.provider_id).includes("embeddinggemma-2"),
          execution: String(m.provider_id).startsWith("device-") ? "device" : m.execution,
        }));
      }
      return answer;
    };
  }
  return { f, l, network, addAsset, addMediaConnection };
}
