import { expect, open, test } from "nsh/test";
import { BASE, TOKENS, world } from "../../test/fakeServer";
import { launched } from "../../test/launched";
import { spaceWorld } from "./space/test/fixtures";

type F = ReturnType<typeof world>;
const wildcard = (method: string) => `${method} ${BASE}/v1/*`;

async function pick(app: Awaited<ReturnType<typeof signedIn>>, id: string, option: string) {
  await app.press(`id:${id}`);
  await app.waitFor(`id:${id}:${option}`);
  await app.press(`id:${id}:${option}`);
}

async function signedIn(f: Pick<F, "network">, user = "alice") {
  const app = await open("../../App.tsx", { modules: launched(), network: f.network as any, platform: "linux", width: 1280, height: 900 });
  await app.clear("id:server-url");
  await app.type("id:server-url", BASE);
  await app.type("id:access-token", TOKENS[user]!);
  await app.press("id:connect");
  await app.waitFor("id:nav-chat");
  return app;
}

function registeredPolicy(f: F, visibility: "personal" | "organization", builtIn = false) {
  const runner = f.addRunner("Build box", visibility, "alice");
  f.addSpace("project", "Project", "alice");
  f.state.attached.set("project", runner.id);
  if (builtIn) {
    f.addSpace("other", "Other", "alice");
    f.state.attached.set("other", runner.id);
  }
  const endpoint = `/v1/runners/${runner.id}`;
  const base = `${endpoint}/shell-policy`;
  const override = `${endpoint}/spaces/project/shell-policy`;
  const writes: { path: string; body: unknown }[] = [];
  let defaults = { shell_mode: builtIn ? "allow" : "approve", approver: "owner" };
  let space: { shell_mode: string | null; approver: string | null } = { shell_mode: null, approver: null };
  const result = () => {
    const current = space.shell_mode ?? defaults.shell_mode;
    const downgraded = builtIn && current === "allow";
    return { status: 200, body: {
      runner_id: runner.id, runner_name: runner.name, visibility, built_in: builtIn, default: defaults,
      spaces: [
        { space_id: "project", space_name: "Project", ...space, effective_mode: downgraded ? "approve" : current,
          effective_approver: space.approver ?? defaults.approver, downgraded_reason: downgraded ? "built_in_multiple_spaces" : null },
        ...(builtIn ? [{ space_id: "other", space_name: "Other", shell_mode: null, approver: null,
          effective_mode: "approve", effective_approver: defaults.approver, downgraded_reason: "built_in_multiple_spaces" }] : []),
      ],
    } };
  };
  const get = f.network[wildcard("GET")]!, put = f.network[wildcard("PUT")]!;
  f.network[wildcard("GET")] = req => {
    const path = new URL(req.url).pathname;
    if (path === base) return result();
    if (path !== "/v1/runners" || !builtIn) return get(req);
    const reply = get(req) as { status: number; body: { runners: { id: string }[] } };
    return { ...reply, body: { runners: reply.body.runners.map(row => ({ ...row, built_in: row.id === runner.id })) } };
  };
  f.network[wildcard("PUT")] = req => {
    const path = new URL(req.url).pathname;
    if (path !== base && path !== override) return put(req);
    const body = JSON.parse(req.body ?? "{}");
    writes.push({ path, body });
    if (visibility === "organization" && body.shell_mode === "classify") return { status: 400, body: { error: "Organization runners cannot use System One." } };
    if (path === base) defaults = body;
    else space = body;
    return result();
  };
  return { runner, writes, base, override };
}

test("a managed registered runner saves its default and can return a Space override to inheritance", async () => {
  const f = world();
  const { runner, writes, base, override } = registeredPolicy(f, "personal");
  const app = await signedIn(f);
  await app.press("id:nav-settings");
  await app.press("id:nav-settings:runners");
  await app.press(`id:runner-shell:${runner.id}:toggle`);
  const s = `${runner.id}:project`;
  await app.press(`id:runner-shell-approver:${runner.id}`);
  await app.press("text:Any Space member");
  await pick(app, `runner-shell-mode:${runner.id}`, "off");
  await app.press(`id:runner-shell-save:${runner.id}`);
  await app.waitUntil(() => writes.length === 1);
  expect(writes[0]).toEqual({ path: base, body: { shell_mode: "off", approver: "members" } });
  await app.waitFor(`id:runner-shell-notice:${runner.id}`);
  await pick(app, `runner-shell-space-mode:${s}`, "approve");
  await app.press(`id:runner-shell-space-save:${s}`);
  await app.waitUntil(() => writes.length === 2);
  expect(writes[1]).toEqual({ path: override, body: { shell_mode: "approve", approver: null } });
  await app.waitFor(`id:runner-shell-notice:${runner.id}`);
  await pick(app, `runner-shell-space-mode:${s}`, "inherit");
  await app.press(`id:runner-shell-space-save:${s}`);
  await app.waitUntil(() => writes.length === 3);
  expect(writes[2]).toEqual({ path: override, body: { shell_mode: null, approver: null } });
});

test("organization managers do not get a System One option", async () => {
  const f = world();
  const { runner, writes } = registeredPolicy(f, "organization");
  const app = await signedIn(f);
  await app.press("id:nav-settings");
  await app.press("id:nav-settings:runners");
  await app.press(`id:runner-shell:${runner.id}:toggle`);
  for (const picker of [`runner-shell-mode:${runner.id}`, `runner-shell-space-mode:${runner.id}:project`]) {
    await app.press(`id:${picker}`);
    const classify = `id:${picker}:classify`;
    await app.waitFor(classify);
    expect(app.enabled(classify)).toBe(false);
    await app.keyboard.press("escape");
  }
  // For an organization runner, "owner" is any org admin.
  await app.waitFor(`id:runner-shell-effective:${runner.id}:project >> text:Ask every time · Org admin approves`);
  await app.press(`id:runner-shell-approver:${runner.id}`);
  await app.waitFor("text:Any Space member");
  expect(app.texts()).toContain("Org admin");
  expect(app.texts()).not.toContain("Runner owner");
  await app.keyboard.press("escape");
  expect(writes).toEqual([]);
});

test("an organization runner remains visible without exposing policy controls to non-managers", async () => {
  const f = world();
  const { runner } = registeredPolicy(f, "organization");
  const app = await signedIn(f, "bob");
  await app.press("id:nav-settings");
  await app.press("id:nav-settings:runners");
  await app.waitFor(`id:runner:${runner.id}`);
  expect(app.shows(`id:runner-shell:${runner.id}:toggle`)).toBe(false);
});

test("a built-in runner shows the server's effective mode and downgrade", async () => {
  const f = world();
  const { runner, writes, base } = registeredPolicy(f, "personal", true);
  const app = await signedIn(f);
  await app.press("id:nav-settings");
  await app.press("id:nav-settings:runners");
  await app.press(`id:runner-shell:${runner.id}:toggle`);
  for (const space of ["project", "other"]) {
    await app.waitFor(`id:runner-shell-effective:${runner.id}:${space} >> text:/Ask every time/`);
    await app.waitFor(`id:runner-shell-downgrade:${runner.id}:${space}`);
  }
  expect(app.texts()).toContain("A built-in runner allows commands only while it serves exactly one Space, so each command asks.");
  await app.press(`id:runner-shell-approver:${runner.id}`);
  await app.press("text:Any Space member");
  await app.press(`id:runner-shell-save:${runner.id}`);
  await app.waitFor(`id:runner-shell-notice:${runner.id}`);
  expect(writes).toEqual([{ path: base, body: { shell_mode: "allow", approver: "members" } }]);
});

for (const user of ["alice", "bob"] as const) {
  test(`Space Runner shows the effective policy to its ${user === "alice" ? "owner" : "member"}`, async () => {
    const f = spaceWorld();
    const writes: unknown[] = [];
    const path = "/v1/spaces/acme/shell-policy";
    const runnerPath = "/v1/spaces/acme/runner";
    const get = f.network[wildcard("GET")]!, put = f.network[wildcard("PUT")]!;
    let mode = "approve";
    const shell = () => ({
      shell_mode: mode, approver: "owner", effective_mode: mode,
      downgraded_reason: null, runner_name: "Dedicated Space runner", source: "dedicated",
    });
    f.network[wildcard("GET")] = req => {
      if (new URL(req.url).pathname !== runnerPath) return get(req);
      const result = get(req) as { status: number; body: Record<string, unknown> };
      return { ...result, body: { ...result.body, shell: shell() } };
    };
    f.network[wildcard("PUT")] = req => {
      if (new URL(req.url).pathname !== path) return put(req);
      const body = JSON.parse(req.body ?? "{}");
      writes.push(body);
      mode = body.shell_mode;
      return { status: 200, body: { shell: shell() } };
    };
    const app = await signedIn(f, user);
    await app.press("id:space:acme");
    await app.press("id:space-settings-open");
    await app.waitFor("id:space-settings");
    await app.press("id:space-settings-tab:runner");
    await app.waitFor("id:space-shell-effective >> text:/Ask every time/");
    if (user === "alice") {
      await pick(app, "space-shell-mode", "off");
      await app.press("id:space-shell-save");
      await app.waitUntil(() => writes.length === 1);
      expect(writes).toEqual([{ shell_mode: "off", approver: "owner" }]);
    } else {
      expect(app.shows("id:space-shell-save")).toBe(false);
      expect(app.shows("id:space-shell-mode")).toBe(false);
      expect(writes).toEqual([]);
    }
  });
}
