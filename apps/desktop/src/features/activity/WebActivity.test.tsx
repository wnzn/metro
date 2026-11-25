import { expect, open, test } from "nsh/test";
import { BASE, TOKENS, world } from "../../../test/fakeServer";
import { launched } from "../../../test/launched";
import { parseWebTool } from "../../api/webSearch";
import { webAttemptRows, webFailureText } from "./web";

type F = ReturnType<typeof world>;
const secret = "fixture-only-refusal-value";
const attempt = (provider: string, mode: string, outcome: string, until?: string) => ({ provider, mode, outcome, until });

function event(f: F, id: string, kind: string, text: string, tool: object) {
  return f.post("acme/lobby", { kind, actor: kind === "error" ? "system" : "agent", text,
    activity: { id, name: "Metro", source: "agent", tool } } as any);
}
async function room(f: F, user: "alice" | "bob") {
  const app = await open("./WebActivity.fixture.tsx", { modules: launched(), network: f.network as any, platform: "linux", width: 1200, height: 900 });
  await app.clear("id:server-url");
  await app.type("id:server-url", BASE);
  await app.type("id:access-token", TOKENS[user]!);
  await app.press("id:connect");
  await app.waitFor("id:space:acme");
  await app.press("id:space:acme");
  await app.waitFor("text:hello from bob in acme");
  return app;
}

test("web activity folds the accepted query and shows rotation, the interrupted mode and the same-provider retry in order", async () => {
  const f = world();
  const call = event(f, "run_web", "tool_call", 'web_search "weather over Cebu" keywords: "forecast" max_results: 2', { name: "web_search" });
  const result = event(f, "run_web", "tool_result", "web_search via Firecrawl (free): 2 results\nResults not repeated in activity", {
    name: "web_search", provider: "firecrawl", mode: "keyless",
    attempts: [
      attempt("exa", "keyless", "limited", "2027-01-02T14:20:00Z"),
      attempt("parallel", "keyless", "skipped_disabled"),
      attempt("tinyfish", "keyless", "skipped_no_key"),
      attempt("tavily", "keyless", "skipped_cooldown"),
      attempt("firecrawl", "key", "credential_changed"),
      attempt("firecrawl", "keyless", "ok"),
    ],
  });
  const app = await room(f, "bob");
  await app.press("id:activity-toggle:agent:run_web");
  await app.waitFor("text:Web search · Firecrawl");
  expect(app.shows(`id:activity-web-count:${result.id}`)).toBe(true);
  expect(app.shows("text:2 results")).toBe(true);
  expect(app.shows("text:/weather over Cebu/")).toBe(false);
  await app.press(`id:activity-web-args:${call.id}`);
  expect(app.texts().join(" ")).toContain('"weather over Cebu" keywords: "forecast" max_results: 2');
  const text = app.texts().join(" | ");
  expect(text).toContain("Exa (no key) reached its rate limit · retry after 2027-01-02 14:20 UTC. Switched to Firecrawl.");
  expect(text).toContain("Firecrawl (saved key): credential changed.");
  expect(text).toContain("Firecrawl (no key): worked.");
  expect(text.indexOf("Firecrawl (saved key): credential changed.")).toBeLessThan(text.indexOf("Firecrawl (no key): worked."));
  expect(app.shows("id:activity-websearch-rotated")).toBe(true);
  expect(app.shows("id:activity-web-attempt:1")).toBe(false);
  expect(app.shows("id:activity-web-attempt:2")).toBe(true);
  expect(app.shows("id:activity-web-attempt:3")).toBe(true);
  expect(app.shows("id:activity-web-attempt:4")).toBe(true);
  expect(app.shows("id:activity-web-attempt:5")).toBe(true);
});

test("after run eviction only the server-attributed starter is offered Settings; refusals stay private", async () => {
  const f = world();
  // Only transcript records survive; the in-memory registry is empty.
  const e = event(f, "run_exhausted", "error", secret, { name: "web_search", failure: "exhausted", starter_id: "alice",
    attempts: [attempt("exa", "keyless", "out_of_credits"), attempt("tavily", "key", "key_rejected")] });
  const refuse = event(f, "run_refuse", "error", `web_fetch refused: ${secret}`, { name: "web_fetch", failure: "not_allowed" });
  const other = await room(f, "bob");
  await other.waitFor("text:No provider returned a usable result.");
  await other.press("id:activity-toggle:agent:run_exhausted");
  await other.waitFor(`id:activity-web-failure:${e.id}`);
  expect(other.shows("id:activity-details:agent:run_exhausted >> text:No provider returned a usable result.")).toBe(true);
  expect(other.shows("id:activity-open-websearch")).toBe(false);
  expect(other.texts().join(" ")).not.toContain("before configuring an API key");
  await other.press("id:activity-toggle:agent:run_refuse");
  await other.waitFor(`id:activity-web-failure:${refuse.id}`);
  expect(other.texts().join(" ")).not.toContain(secret);
  expect(other.shows(`id:activity-web-args:${refuse.id}`)).toBe(false);
  expect(other.shows("text:Read web pages was not allowed. Nothing was sent.")).toBe(true);
  const own = await room(f, "alice");
  await own.press("id:activity-toggle:agent:run_exhausted");
  await own.waitFor("id:activity-open-websearch");
  expect(own.texts().join(" ")).toContain("Check the provider’s terms before configuring an API key");
  expect(own.texts().join(" ")).not.toContain("higher limits");
  await own.press("id:activity-open-websearch");
  await own.waitFor("text:Opened Web search settings");
});

test("exhaustion doesn't imply contact when the attempt list is empty or only contains skips", () => {
  for (const attempts of [[], [attempt("exa", "keyless", "skipped_disabled"), attempt("tinyfish", "key", "skipped_no_key")]]) {
    const note = parseWebTool({ name: "web_search", failure: "exhausted", attempts })!;
    expect(webFailureText(note, false)).toBe("No provider returned a usable result.");
  }
});

test("offline names the starter to other members but says your for the same stable account", async () => {
  const f = world();
  event(f, "run_offline", "error", "Web search couldn't run: Metro isn't running on Alice Example's computer, so it can't search for them. Searches never run on the server or someone else's computer.",
    { name: "web_search", failure: "offline", starter_id: "alice" });
  const nonStarter = await room(f, "bob");
  await nonStarter.waitFor("text:/Alice Example's Metro app, which is offline/");
  const app = await room(f, "alice");
  await app.waitFor("text:/your Metro app, which is offline/");
  expect(app.shows("text:/Nothing was sent/")).toBe(true);
});

test("history without server attribution gets no starter action", async () => {
  const f = world();
  f.state.runs.push({ id: "run_legacy", actor: "alice", status: "completed", space_id: "acme", room_id: "lobby", task: "Find weather" });
  const error = event(f, "run_legacy", "error", "alice", { name: "web_search", failure: "exhausted" });
  const app = await room(f, "alice");
  await app.press("id:activity-toggle:agent:run_legacy");
  await app.waitFor(`id:activity-web-failure:${error.id}`);
  expect(app.shows("id:activity-open-websearch")).toBe(false);
});

test("web attempt presentation distinguishes provider failures, skips and repeat interruptions", () => {
  const note = parseWebTool({ name: "web_search", attempts: [
    attempt("exa", "key", "credential_changed"),
    attempt("exa", "keyless", "credential_changed"),
    attempt("parallel", "keyless", "blocked"),
    attempt("firecrawl", "key", "skipped_cooldown"),
    attempt("tavily", "keyless", "timeout"),
    attempt("tinyfish", "keyless", "skipped_no_key"),
  ] })!;
  expect(webAttemptRows(note).map((row) => row.text)).toEqual([
    "Exa (saved key): credential changed.", "Exa (no key): credential changed.",
    "Parallel (no key): blocked requests from this network.", "Firecrawl (saved key): paused after an earlier attempt.",
    "Tavily (no key): timed out.", "TinyFish: requires an API key.",
  ]);
  const bad = parseWebTool({ name: "web_fetch", provider: secret, failure: secret, attempts: [attempt("exa", secret, "ok")] })!;
  expect(bad.provider).toBe(null);
  expect(bad.failure).toBe("failed");
  expect(bad.attempts).toEqual([]);
});

test("activity shows cached key rejection as a skip and points rotation at the next known contact", async () => {
  const f = world();
  event(f, "run_cached", "tool_call", 'web_search "weather"', { name: "web_search" });
  event(f, "run_cached", "tool_result", "web_search via Tavily (free): 1 result", {
    name: "web_search", provider: "tavily", mode: "keyless", attempts: [
      attempt("firecrawl", "key", "credential_changed"),
      attempt("firecrawl", "keyless", "credential_changed"),
      attempt("exa", "keyless", "limited"),
      { ...attempt("parallel", "key", "key_rejected"), contacted: false },
      attempt("tavily", "keyless", "ok"),
    ],
  });
  const app = await room(f, "bob");
  await app.press("id:activity-toggle:agent:run_cached");
  await app.waitFor("id:activity-websearch-rotated");
  expect(app.texts().filter((s) => s.includes("credential changed"))).toEqual([
    "Firecrawl (saved key): credential changed.", "Firecrawl (no key): credential changed.",
  ]);
  expect(app.shows("text:Exa (no key) reached its rate limit. Switched to Tavily.")).toBe(true);
  expect(app.shows("text:Parallel (saved key): skipped (saved key was already rejected).")).toBe(true);
});

test("legacy key rejections don't assert an immediate switch or known provider contact", () => {
  const rejection = attempt("parallel", "key", "key_rejected");
  const rows = (contacted?: boolean) => webAttemptRows(parseWebTool({ name: "web_search", attempts: [
    attempt("exa", "keyless", "limited"), { ...rejection, contacted }, attempt("tavily", "keyless", "ok"),
  ] })!);
  expect(rows()[0]!.switched).toBe(false);
  expect(rows()[1]!.text).toBe("Parallel (saved key): key rejected (contact not recorded).");
  expect(rows(true)[0]!.text).toContain("Switched to Parallel.");
  expect(rows(true)[1]!.text).toBe("Parallel (saved key): rejected the key.");
  expect(rows(false)[0]!.text).toContain("Switched to Tavily.");
});
