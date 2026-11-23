// Browser test for the admin panel (index.html, admin.css, admin.js).
//
// Serves the three files the way metro-server mounts them (/, /admin,
// /admin/assets/admin.{js,css}) next to a mocked admin API, then drives a
// headless Chromium over the DevTools protocol. No npm dependencies.
// Tokens are random per run and never printed.
//
//   CHROME=/path/to/chrome node crates/server/src/admin_web/admin.browser-test.mjs [SHOT_DIR]
//
// CHROME defaults to the newest Playwright Chromium in ~/.cache/ms-playwright.
import { spawn } from "node:child_process";
import { randomBytes } from "node:crypto";
import { once } from "node:events";
import { existsSync, mkdtempSync, readdirSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { createServer } from "node:http";
import { homedir, tmpdir } from "node:os";
import { dirname, join } from "node:path";
import { setTimeout as delay } from "node:timers/promises";
import { fileURLToPath } from "node:url";

const here = dirname(fileURLToPath(import.meta.url));
const shotDir = process.argv[2] || null;

function findChrome() {
  if (process.env.CHROME) return process.env.CHROME;
  const base = join(homedir(), ".cache/ms-playwright");
  const dirs = existsSync(base) ? readdirSync(base).filter((d) => /^chromium-\d+$/.test(d)).sort().reverse() : [];
  for (const d of dirs) {
    for (const sub of ["chrome-linux64/chrome", "chrome-linux/chrome"]) {
      if (existsSync(join(base, d, sub))) return join(base, d, sub);
    }
  }
  throw new Error("set CHROME to a Chromium binary");
}

// ------------------------------------------------------------------ mock server

const tok = () => "metro_" + randomBytes(18).toString("hex");
const ADMIN = tok();
const PLAIN = tok();
const issued = new Set();
let seq = 0;
const mock = {
  users: [
    { id: "u-alice", name: "Alice", has_token: true },
    { id: "u/slash bob", name: '<img src=x onerror="window.__xss=1">Bob', has_token: false },
  ],
  failUsers: false,
  slowUsers: 0,
  log: [],
};
const CSP = "default-src 'none'; script-src 'self'; style-src 'self'; connect-src 'self'; img-src 'self'; base-uri 'none'; form-action 'self'";
const files = {
  "/admin/assets/admin.js": ["admin.js", "text/javascript"],
  "/admin/assets/admin.css": ["admin.css", "text/css"],
};

function send(res, status, body, type = "application/json") {
  res.writeHead(status, { "content-type": type, "cache-control": "no-store", "content-security-policy": CSP });
  res.end(body === undefined ? undefined : typeof body === "string" ? body : JSON.stringify(body));
}

const server = createServer(async (req, res) => {
  const url = new URL(req.url, "http://x");
  const auth = req.headers.authorization || "";
  mock.log.push({ method: req.method, url: req.url, auth: auth ? auth.replace(/\S+$/, "<redacted>") : "",
    authOk: auth === "Bearer " + ADMIN || auth === "Bearer " + PLAIN || [...issued].some((t) => auth === "Bearer " + t),
    cookie: !!req.headers.cookie, origin: req.headers.origin || "", referer: req.headers.referer || "" });
  const p = url.pathname;
  if (p === "/" || p === "/admin" || p === "/admin/") return send(res, 200, readFileSync(join(here, "index.html"), "utf8"), "text/html; charset=utf-8");
  if (files[p]) return send(res, 200, readFileSync(join(here, files[p][0]), "utf8"), files[p][1]);
  if (p === "/frame.html") {
    res.writeHead(200, { "content-type": "text/html" });
    return res.end('<!doctype html><iframe id="f" src="/admin" width="800" height="600"></iframe>');
  }
  if (!p.startsWith("/v1/")) return send(res, 404, { error: "not found" });
  const who = auth === "Bearer " + ADMIN ? "admin" : auth === "Bearer " + PLAIN ? "plain" : null;
  if (!who) { res.setHeader("www-authenticate", "Bearer"); return send(res, 401, { error: "invalid token" }); }
  if (p === "/v1/me") return send(res, 200, { id: who === "admin" ? "jm" : "pat", actor: who === "admin" ? "JM" : "Pat", is_org_admin: who === "admin" });
  if (who !== "admin") return send(res, 403, { error: "only an organization admin can manage users" });
  let body = "";
  for await (const c of req) body += c;
  if (p === "/v1/admin/users" && req.method === "GET") {
    if (mock.slowUsers) await new Promise((r) => setTimeout(r, mock.slowUsers));
    if (mock.failUsers) return send(res, 500, { error: "database is locked" });
    return send(res, 200, { users: mock.users });
  }
  if (p === "/v1/admin/users" && req.method === "POST") {
    const { name } = JSON.parse(body);
    const u = { id: "u-" + ++seq, name, has_token: true };
    mock.users.push(u);
    const t = tok(); issued.add(t); mock.lastToken = t;
    return send(res, 201, { id: u.id, name, token: t });
  }
  const m = /^\/v1\/admin\/users\/([^/]+)\/token$/.exec(p);
  if (m) {
    const id = decodeURIComponent(m[1]);
    const u = mock.users.find((x) => x.id === id);
    if (!u) return send(res, 404, { error: "no such user" });
    if (req.method === "POST") { const t = tok(); issued.add(t); mock.lastToken = t; u.has_token = true; return send(res, 200, { token: t }); }
    if (req.method === "DELETE") { u.has_token = false; res.writeHead(204); return res.end(); }
  }
  send(res, 404, { error: "not found" });
});
await new Promise((r) => server.listen(0, "127.0.0.1", r));
const port = server.address().port;

// ------------------------------------------------------------------ chrome + CDP

let profile, chrome, chromeClosed, ws;
let nextId = 0;

async function startChrome() {
  const executable = findChrome();
  profile = mkdtempSync(join(tmpdir(), "metro-admin-test-"));
  chrome = spawn(executable, [
    "--headless=new", "--no-sandbox", "--disable-gpu", "--no-first-run", "--no-default-browser-check",
    `--user-data-dir=${profile}`, "--remote-debugging-port=0",
    "--host-resolver-rules=MAP metro.lan 127.0.0.1", "about:blank",
  ], { stdio: "ignore" });
  chromeClosed = new Promise((resolve) => { chrome.once("close", resolve); });

  const startup = new AbortController();
  const { signal } = startup;
  const abort = (message) => startup.abort(new Error(message));
  chrome.on("error", (error) => abort(`could not start Chrome (${error.code || "spawn error"})`));
  chrome.once("exit", (code, signal) => abort(`Chrome exited during startup (${signal || `code ${code}`})`));
  const timer = setTimeout(() => abort("Chrome startup timed out after 60s waiting for DevTools"), 60_000);
  try {
    for (;;) {
      let contents = "";
      try {
        contents = readFileSync(join(profile, "DevToolsActivePort"), "utf8").trim();
      } catch (error) {
        if (error.code !== "ENOENT") throw new Error("could not read Chrome DevToolsActivePort");
      }
      const [port, path] = contents.split(/\r?\n/);
      // Chrome may not have finished writing both fields yet.
      if (/^\d{1,5}$/.test(port) && +port > 0 && +port <= 65535 &&
          /^\/devtools\/browser\/[a-f0-9]{8}(?:-[a-f0-9]{4}){3}-[a-f0-9]{12}$/i.test(path)) {
        ws = new WebSocket(`ws://127.0.0.1:${port}${path}`);
        ws.addEventListener("error", () => abort("could not open Chrome DevTools WebSocket"), { once: true });
        await once(ws, "open", { signal });
        return;
      }
      await delay(100, undefined, { signal });
    }
  } catch (error) {
    signal.throwIfAborted();
    throw error;
  } finally {
    clearTimeout(timer);
  }
}

async function cleanup() {
  let killTimer;
  if (chrome?.pid && chrome.exitCode === null && chrome.signalCode === null) {
    if (ws?.readyState === WebSocket.OPEN) {
      // Let Chrome stop its helpers and finish writing the profile before removing it.
      ws.send(JSON.stringify({ id: ++nextId, method: "Browser.close" }));
    } else {
      chrome.kill();
    }
    killTimer = setTimeout(() => chrome.kill("SIGKILL"), 5_000);
  }
  await chromeClosed;
  clearTimeout(killTimer);
  if (ws && ws.readyState < WebSocket.CLOSING) ws.close();
  await new Promise((resolve) => {
    server.close(resolve);
    server.closeAllConnections();
  });
  if (profile) rmSync(profile, { recursive: true, force: true, maxRetries: 3 });
}

async function cleanupAndThrow(error) {
  await cleanup();
  throw error;
}

await startChrome().catch(cleanupAndThrow);
const pending = new Map();
const consoleMsgs = [];
ws.onclose = () => {
  for (const { reject } of pending.values()) reject(new Error("Chrome DevTools WebSocket closed"));
  pending.clear();
};
ws.onmessage = (ev) => {
  const msg = JSON.parse(ev.data);
  if (msg.id && pending.has(msg.id)) {
    const { resolve, reject } = pending.get(msg.id);
    pending.delete(msg.id);
    msg.error ? reject(new Error(msg.error.message)) : resolve(msg.result);
  } else if (msg.method === "Runtime.consoleAPICalled" || msg.method === "Log.entryAdded" || msg.method === "Runtime.exceptionThrown") {
    consoleMsgs.push(msg);
  }
};
const cdp = (method, params = {}, sessionId) => new Promise((resolve, reject) => {
  if (ws.readyState !== WebSocket.OPEN) return reject(new Error("Chrome DevTools WebSocket is not open"));
  const id = ++nextId;
  pending.set(id, { resolve, reject });
  ws.send(JSON.stringify({ id, method, params, sessionId }));
});

const { targetId } = await cdp("Target.createTarget", { url: "about:blank" }).catch(cleanupAndThrow);
const { sessionId } = await cdp("Target.attachToTarget", { targetId, flatten: true }).catch(cleanupAndThrow);
const page = (m, p) => cdp(m, p, sessionId);
await page("Runtime.enable").catch(cleanupAndThrow);
await page("Log.enable").catch(cleanupAndThrow);
await page("Page.enable").catch(cleanupAndThrow);

async function evaluate(expr) {
  const r = await page("Runtime.evaluate", { expression: expr, awaitPromise: true, returnByValue: true });
  if (r.exceptionDetails) throw new Error("eval failed: " + (r.exceptionDetails.exception?.description || r.exceptionDetails.text));
  return r.result.value;
}
async function waitFor(expr, ms = 5000) {
  const end = Date.now() + ms;
  for (;;) {
    if (await evaluate(`!!(${expr})`)) return;
    if (Date.now() > end) throw new Error("timed out waiting for: " + expr);
    await new Promise((r) => setTimeout(r, 40));
  }
}
async function goto(url) {
  await page("Page.navigate", { url });
  await waitFor("document.readyState === 'complete' && document.getElementById('signin')");
  await new Promise((r) => setTimeout(r, 50));
}
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
const vis = (id) => `!document.getElementById(${JSON.stringify(id)}).hidden`;
const text = (id) => evaluate(`document.getElementById(${JSON.stringify(id)}).textContent`);
const click = (sel) => evaluate(`document.querySelector(${JSON.stringify(sel)}).click()`);
async function type(id, value) {
  await evaluate(`(() => { const e = document.getElementById(${JSON.stringify(id)}); e.focus(); e.value = ${JSON.stringify(value)}; e.dispatchEvent(new Event('input', {bubbles:true})); })()`);
}
const submit = (formId) => evaluate(`document.getElementById(${JSON.stringify(formId)}).requestSubmit()`);
async function shot(name, width, height) {
  if (!shotDir) return;
  await page("Emulation.setDeviceMetricsOverride", { width, height, deviceScaleFactor: 1, mobile: width < 600 });
  await sleep(100);
  const { data } = await page("Page.captureScreenshot", { format: "png", captureBeyondViewport: true });
  writeFileSync(join(shotDir, name), Buffer.from(data, "base64"));
}

// ------------------------------------------------------------------ tests

let passed = 0;
const failures = [];
async function test(name, fn) {
  try { await fn(); passed++; process.stdout.write(`ok   ${name}\n`); }
  catch (e) {
    failures.push(name);
    process.stdout.write(`FAIL ${name}: ${String(e.message).replace(/metro_[a-f0-9]{36}/g, "<redacted>")}\n`);
  }
}
function assert(cond, msg) { if (!cond) throw new Error(msg); }
const base = `http://127.0.0.1:${port}`;
const apiCalls = () => mock.log.filter((r) => r.url.startsWith("/v1/"));

async function signIn(token) {
  await type("token", token);
  await submit("signin-form");
}

await test("root and /admin serve the sign-in form; assets load under CSP", async () => {
  for (const path of ["/", "/admin"]) {
    await goto(base + path);
    assert(await evaluate(vis("signin")), "sign-in hidden at " + path);
    assert(!(await evaluate(vis("panel"))), "panel visible before sign-in");
    assert(await evaluate("getComputedStyle(document.body).fontFamily.includes('system-ui')"), "CSS not applied");
  }
  assert(apiCalls().length === 0, "API called before sign-in");
  assert(await evaluate("document.activeElement.id") === "token", "token field not focused");
});

await test("empty and malformed tokens are refused without a request", async () => {
  await goto(base + "/admin");
  await signIn("   ");
  assert(await evaluate(vis("signin-error")), "no error for empty");
  await signIn("has space inside");
  assert((await text("signin-error")).includes("doesn't look like"), "no error for malformed");
  assert(apiCalls().length === 0, "request was sent");
});

await test("a rejected token shows an error and no panel", async () => {
  await goto(base + "/admin");
  await signIn(tok());
  await waitFor(vis("signin-error"));
  assert(!(await evaluate(vis("panel"))), "panel shown");
  assert(await evaluate("document.getElementById('token').value") === "", "token left in the field");
});

await test("a non-admin is signed out and never reaches admin routes", async () => {
  mock.log.length = 0;
  await goto(base + "/admin");
  await signIn(PLAIN);
  await waitFor(vis("denied"));
  assert((await text("denied-id")) === "pat", "wrong id");
  assert(!(await evaluate(vis("panel"))), "panel shown to non-admin");
  assert(!apiCalls().some((r) => r.url.startsWith("/v1/admin")), "admin route called");
  await click("#denied-back");
  assert(await evaluate(vis("signin")), "back did not return to sign-in");
});

await test("admin signs in; names render as text", async () => {
  mock.log.length = 0;
  await goto(base + "/admin");
  await signIn("Bearer " + ADMIN);
  await waitFor(vis("panel") + " && document.querySelectorAll('#users-body tr').length === 2");
  assert((await text("who-name")) === "JM", "who-name");
  assert(await evaluate("document.querySelectorAll('#users-body img').length") === 0, "markup injected");
  assert(await evaluate("window.__xss") === undefined, "script ran");
  assert(await evaluate("[...document.querySelectorAll('#users-body td.name')].some(t => t.textContent.startsWith('<img'))"), "name not shown as text");
  for (const r of apiCalls()) {
    assert(r.authOk, "bad auth on " + r.url);
    assert(!r.cookie, "cookie sent");
    assert(!/token|metro_/.test(r.url), "credential in URL " + r.url);
    assert(!r.referer, "referer sent");
  }
  assert(await evaluate("document.getElementById('token').value") === "", "token left in the field");
  assert(!(await evaluate("location.href")).includes("metro_"), "token in URL");
});

await test("every button has an accessible name; inputs have labels", async () => {
  const bad = await evaluate(`[...document.querySelectorAll('button')].filter(b => !(b.getAttribute('aria-label') || b.textContent.trim())).length`);
  assert(bad === 0, bad + " unnamed buttons");
  const unlabeled = await evaluate(`[...document.querySelectorAll('input')].filter(i => !document.querySelector('label[for="' + i.id + '"]')).length`);
  assert(unlabeled === 0, unlabeled + " unlabeled inputs");
});

await test("brand, text, control edges and focus meet contrast targets in light and dark", async () => {
  try {
    for (const scheme of ["light", "dark"]) {
      await page("Emulation.setEmulatedMedia", { features: [{ name: "prefers-color-scheme", value: scheme }] });
      // Check actual CSS, including resolved color-mix() badge backgrounds.
      const checks = await evaluate(`(() => {
        const style = s => getComputedStyle(document.querySelector(s));
        const canvas = document.createElement("canvas");
        canvas.width = canvas.height = 1;
        const ctx = canvas.getContext("2d", { willReadFrequently: true });
        const luminance = color => {
          ctx.fillStyle = color;
          ctx.fillRect(0, 0, 1, 1);
          const rgb = ctx.getImageData(0, 0, 1, 1).data;
          return [0.2126, 0.7152, 0.0722].reduce((sum, weight, i) => {
            const c = rgb[i] / 255;
            return sum + weight * (c <= 0.04045 ? c / 12.92 : ((c + 0.055) / 1.055) ** 2.4);
          }, 0);
        };
        const results = [];
        const check = (name, front, back, min) => {
          const [a, b] = [luminance(front), luminance(back)].sort((a, b) => b - a);
          results.push([name, (a + 0.05) / (b + 0.05), min]);
        };
        const bg = s => style(s).backgroundColor;
        const ink = (s, background = s, min = 4.5) => check(s, style(s).color, bg(background), min);
        const edge = (s, background) => check(s + " border", style(s).borderTopColor, bg(background), 3);
        const brandSvg = style(".brand-mark path");
        check("train", brandSvg.fill, bg(".topbar"), 3);
        ink(".brand-lockup", ".topbar");
        ink(".brand-descriptor", ".topbar");
        ink("body");
        ink("#add-title", "#panel > section");
        ink("#add-hint", "#panel > section");
        ink("#add-name");
        ink("#add-submit");
        ink("#refresh");
        ink(".btn-ghost-danger:not(:disabled)", "#users-card");
        ink(".badge-ok");
        ink(".badge-none");
        ink(".notice-danger");
        ink(".notice-warning");
        ink(".alert-danger");
        ink(".alert-warning");
        check("primary button edge", bg("#add-submit"), bg("#panel > section"), 3);
        edge("#add-name", "#panel > section");
        edge("#refresh", "#users-card");
        edge(".btn-ghost-danger:not(:disabled)", "#users-card");
        edge(".secret", "body");
        document.getElementById("add-name").focus();
        const input = style("#add-name");
        if (input.outlineStyle !== "solid" || parseFloat(input.outlineWidth) < 2) {
          throw new Error("missing focus-visible outline on text field");
        }
        check("focus outline", input.outlineColor, bg("#panel > section"), 3);
        return results;
      })()`);
      for (const [name, ratio, min] of checks) {
        assert(ratio >= min, `${scheme} ${name}: ${ratio.toFixed(2)}:1, minimum ${min}:1`);
      }
    }
  } finally {
    await page("Emulation.setEmulatedMedia", { features: [] });
  }
});

await test("keyboard focus and the train remain visible in forced colors", async () => {
  try {
    await page("Emulation.setEmulatedMedia", { features: [{ name: "forced-colors", value: "active" }] });
    await evaluate("document.getElementById('add-name').focus()");
    for (const type of ["keyDown", "keyUp"]) {
      await page("Input.dispatchKeyEvent", { type, key: "Tab", code: "Tab", windowsVirtualKeyCode: 9 });
    }
    const styles = await evaluate(`(() => {
      const button = document.getElementById("add-submit");
      const { outlineStyle, outlineWidth } = getComputedStyle(button);
      const mark = document.querySelector(".brand-mark");
      return {
        buttonFocused: document.activeElement === button && button.matches(":focus-visible"),
        outlineStyle, outlineWidth,
        markVisible: mark.getBoundingClientRect().width > 0 &&
          getComputedStyle(mark.querySelector("path")).fill !== getComputedStyle(mark.closest("header")).backgroundColor,
      };
    })()`);
    assert(styles.buttonFocused && styles.outlineStyle === "solid" && parseFloat(styles.outlineWidth) >= 2, "missing button focus outline");
    assert(styles.markVisible, "train not distinguishable from background");
  } finally {
    await page("Emulation.setEmulatedMedia", { features: [] });
  }
});

await test("names over 64 characters are refused without a request", async () => {
  assert(await evaluate("document.getElementById('add-name').maxLength") === 64, "maxlength");
  const before = apiCalls().length;
  await type("add-name", "x".repeat(65));
  await submit("add-form");
  assert((await text("add-error")).includes("64"), "no length error");
  assert(await evaluate("document.getElementById('add-name').getAttribute('aria-invalid')") === "true", "not marked invalid");
  assert(apiCalls().length === before, "request sent");
  await type("add-name", "");
});

await test("add user shows a masked one-time token with reveal/hide", async () => {
  await type("add-name", "Carol <b>x</b>");
  await submit("add-form");
  await waitFor(vis("secret"));
  assert((await text("secret-for")) === "Carol <b>x</b>", "secret-for");
  assert(await evaluate("document.getElementById('secret-value').type") === "password", "not masked");
  assert(await evaluate(`document.getElementById('secret-value').value === ${JSON.stringify(mock.lastToken)}`), "wrong token");
  await click("#secret-reveal");
  assert(await evaluate("document.getElementById('secret-value').type") === "text", "not revealed");
  assert(await evaluate("document.getElementById('secret-reveal').getAttribute('aria-pressed')") === "true", "aria-pressed");
  await click("#secret-reveal");
  assert(await evaluate("document.getElementById('secret-value').type") === "password", "not hidden again");
  await waitFor("document.querySelectorAll('#users-body tr').length === 3");
});

await test("copy uses the clipboard when allowed", async () => {
  await cdp("Browser.grantPermissions", { origin: base, permissions: ["clipboardReadWrite", "clipboardSanitizedWrite"] });
  await page("Page.bringToFront");
  await evaluate("document.getElementById('secret-copy').focus()");
  await click("#secret-copy");
  await waitFor("document.getElementById('secret-status').textContent.length > 0");
  const status = await text("secret-status");
  assert(/Copied/.test(status), "status: " + status);
  if (status.startsWith("Copied to the clipboard.") && !status.includes("shown")) {
    assert(await evaluate("document.getElementById('secret-value').type") === "password", "revealed on clipboard copy");
  }
});

await test("copy falls back to a revealed, selected token without the Clipboard API", async () => {
  await evaluate("Object.defineProperty(navigator, 'clipboard', { value: undefined, configurable: true }); document.execCommand = () => false;");
  await click("#secret-copy");
  await waitFor("document.getElementById('secret-status').textContent.includes('Ctrl+C')");
  assert(await evaluate("document.getElementById('secret-value').type") === "text", "not revealed for manual copy");
  assert(await evaluate("document.activeElement.id") === "secret-value", "not focused");
  assert(await evaluate("(() => { const e = document.getElementById('secret-value'); return e.selectionEnd - e.selectionStart === e.value.length; })()"), "not selected");
  await click("#secret-done");
  assert(!(await evaluate(vis("secret"))), "secret still shown");
  assert(await evaluate("document.getElementById('secret-value').value") === "", "secret kept in DOM");
});

await test("rotate asks first, encodes the id, and shows a new token", async () => {
  mock.log.length = 0;
  // Cancel first.
  await click('#users-body button[aria-label="Rotate token for Alice"]');
  await waitFor("document.getElementById('confirm').open");
  assert(await evaluate("document.activeElement.id") === "confirm-cancel", "cancel not focused");
  await click("#confirm-cancel");
  await sleep(50);
  assert(!apiCalls().some((r) => r.method === "POST"), "rotated after cancel");
  // Issue for the user with a slash in the id (no confirmation: no token yet).
  await evaluate(`[...document.querySelectorAll('#users-body button')].find(b => b.getAttribute('aria-label').startsWith('Issue token for <img')).click()`);
  await waitFor(vis("secret"));
  assert(apiCalls().some((r) => r.method === "POST" && r.url === "/v1/admin/users/u%2Fslash%20bob/token"), "id not encoded");
  await click("#secret-done");
  // Rotate Alice for real.
  await click('#users-body button[aria-label="Rotate token for Alice"]');
  await waitFor("document.getElementById('confirm').open");
  await click("#confirm-ok");
  await waitFor(vis("secret") + " && document.getElementById('secret-for').textContent === 'Alice'");
  await shot("admin-secret-desktop.png", 1100, 900);
  await shot("admin-secret-mobile.png", 390, 1100);
  await page("Emulation.clearDeviceMetricsOverride");
});

await test("revoke asks first, sends DELETE, updates the row and clears its secret", async () => {
  await click('#users-body button[aria-label="Revoke token for Alice"]');
  await waitFor("document.getElementById('confirm').open");
  await click("#confirm-ok");
  await waitFor(`[...document.querySelectorAll('#users-body tr')].find(r => r.querySelector('td.name').textContent === 'Alice').querySelector('.badge').textContent === 'No token'`);
  assert(apiCalls().some((r) => r.method === "DELETE" && r.url === "/v1/admin/users/u-alice/token"), "no DELETE");
  assert(!(await evaluate(vis("secret"))), "revoked user's secret still shown");
  assert(await evaluate(`document.querySelector('#users-body button[aria-label="Revoke token for Alice"]').disabled`), "revoke enabled without token");
});

await test("a failed refresh keeps the list and marks it stale", async () => {
  mock.failUsers = true;
  await click("#refresh");
  await waitFor(vis("users-error"));
  const msg = await text("users-error");
  assert(msg.includes("database is locked") && msg.includes("out of date"), msg);
  assert(await evaluate("document.querySelectorAll('#users-body tr').length") === 3, "list dropped");
  mock.failUsers = false;
  await click("#refresh");
  await waitFor("!" + vis("users-error"));
  await shot("admin-users-desktop.png", 1100, 800);
  await page("Emulation.clearDeviceMetricsOverride");
});

await test("sign-out clears data and ignores late responses", async () => {
  mock.slowUsers = 400;
  mock.log.length = 0;
  await click("#refresh");
  await sleep(50);
  await click("#sign-out");
  assert(await evaluate(vis("signin")), "sign-in not shown");
  assert(!(await evaluate(vis("panel"))), "panel still shown");
  assert(!(await evaluate(vis("session-bar"))), "session bar still shown");
  await sleep(600);
  mock.slowUsers = 0;
  assert(await evaluate("document.querySelectorAll('#users-body tr').length") === 0, "stale rows after sign-out");
  assert(!(await evaluate(vis("panel"))), "late response re-opened panel");
  // A later sign-in for someone else must not see the previous list.
  mock.log.length = 0;
  await signIn(PLAIN);
  await waitFor(vis("denied"));
  assert(await evaluate("document.querySelectorAll('#users-body tr').length") === 0, "previous users visible");
});

await test("a 401 mid-session signs out with an explanation", async () => {
  await goto(base + "/admin");
  await signIn(ADMIN);
  await waitFor(vis("panel") + " && !document.getElementById('refresh').disabled");
  // Simulate revocation: from now on the server rejects the token.
  const origEmit = server.listeners("request")[0];
  server.removeAllListeners("request");
  server.on("request", (req, res) => {
    if (req.url.startsWith("/v1/")) { res.writeHead(401, { "content-type": "application/json" }); return res.end('{"error":"invalid token"}'); }
    origEmit(req, res);
  });
  await click("#refresh");
  await waitFor(vis("signin") + " && " + vis("signin-error"));
  assert((await text("signin-error")).includes("revoked"), "no explanation");
  assert(await evaluate("document.querySelectorAll('#users-body tr').length") === 0, "rows kept");
  server.removeAllListeners("request");
  server.on("request", origEmit);
});

await test("credentials in the URL are ignored and scrubbed", async () => {
  mock.log.length = 0;
  await goto(base + "/admin?token=" + tok());
  assert(!(await evaluate("location.href")).includes("token"), "query kept");
  assert((await text("signin-error")).includes("ignores credentials"), "no warning");
  assert(apiCalls().length === 0, "used the URL token");
});

await test("plain HTTP on a non-loopback host refuses sign-in before any request", async () => {
  mock.log.length = 0;
  await goto(`http://metro.lan:${port}/admin`);
  assert(await evaluate(vis("unsafe")), "warning not shown");
  assert(!(await evaluate(vis("signin"))), "sign-in shown");
  assert((await text("unsafe-ssh")).includes(`8080:127.0.0.1:${port}`) && (await text("unsafe-ssh")).includes("metro.lan"), "ssh hint");
  // Even if someone un-hides the form, submission sends nothing.
  await evaluate("document.getElementById('signin').hidden = false");
  await signIn(ADMIN);
  await sleep(200);
  assert(apiCalls().length === 0, "credential sent over plain HTTP");
  assert(await evaluate("document.getElementById('token').value") === "", "token left in field");
  await shot("admin-insecure.png", 900, 700);
  await page("Emulation.clearDeviceMetricsOverride");
});

await test("a framed panel refuses sign-in", async () => {
  await page("Page.navigate", { url: base + "/frame.html" });
  await sleep(600);
  const { frameTree } = await page("Page.getFrameTree");
  const child = frameTree.childFrames?.[0]?.frame;
  assert(child, "no iframe");
  const { executionContextId } = await page("Page.createIsolatedWorld", { frameId: child.id });
  const r = await page("Runtime.evaluate", { contextId: executionContextId, returnByValue: true,
    expression: "!document.getElementById('unsafe').hidden && document.getElementById('signin').hidden" });
  assert(r.result.value === true, "framed page offers sign-in");
});

await test("the page never logs to the console and has no CSP violations", async () => {
  const logs = consoleMsgs.filter((m) => m.method === "Runtime.consoleAPICalled");
  const csp = consoleMsgs.filter((m) => m.method === "Log.entryAdded" && /Content Security Policy|Refused/.test(m.params.entry.text));
  const exc = consoleMsgs.filter((m) => m.method === "Runtime.exceptionThrown");
  assert(logs.length === 0, logs.length + " console calls");
  assert(csp.length === 0, "CSP: " + csp.map((m) => m.params.entry.text).join("; "));
  assert(exc.length === 0, "exceptions: " + exc.map((m) => m.params.exceptionDetails.text).join("; "));
});

await cleanup();
process.stdout.write(`\n${passed} passed, ${failures.length} failed\n`);
process.exitCode = failures.length ? 1 : 0;
