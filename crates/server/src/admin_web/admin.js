// Metro Workspace admin panel.
//
// Security notes:
// - The admin token comes only from the sign-in form. It lives in one
//   in-memory session object, is never stored, logged or put in a URL, and
//   is dropped on sign-out, on a rejected token and when the page is hidden
//   or unloaded.
// - Requests go only to this page's own origin, with the token in an
//   Authorization header, no cookies, no redirects and no caching.
// - Sign-in is refused before any network call inside a frame or from a
//   non-HTTP origin.
// - Plain HTTP works. On loopback and Tailscale hosts (100.64.0.0/10,
//   fd7a:115c:a1e0::/48, *.ts.net) it's used without asking. On any other
//   host the token field stays hidden until the person has read a warning
//   and chosen "Sign in anyway"; that choice (never the token) is remembered
//   for this server in localStorage.
// - Server-supplied text is only ever written with textContent.
"use strict";

(() => {
  const $ = (id) => document.getElementById(id);
  const el = {
    sessionBar: $("session-bar"), whoName: $("who-name"), whoId: $("who-id"), signOut: $("sign-out"),
    unsafe: $("unsafe"), unsafeReason: $("unsafe-reason"), unsafeSsh: $("unsafe-ssh"),
    unsafeLocal: $("unsafe-local"), unsafeLoopback: $("unsafe-loopback"),
    plainHttp: $("plain-http"), plainHttpReason: $("plain-http-reason"), plainHttpAccept: $("plain-http-accept"),
    plainHttpNote: $("plain-http-note"), plainHttpForget: $("plain-http-forget"), plainHttpStatus: $("plain-http-status"),
    signin: $("signin"), signinForm: $("signin-form"), signinError: $("signin-error"),
    token: $("token"), signinSubmit: $("signin-submit"),
    denied: $("denied"), deniedId: $("denied-id"), deniedBack: $("denied-back"),
    panel: $("panel"),
    secret: $("secret"), secretFor: $("secret-for"), secretValue: $("secret-value"),
    secretReveal: $("secret-reveal"), secretCopy: $("secret-copy"), secretStatus: $("secret-status"),
    secretDone: $("secret-done"),
    addForm: $("add-form"), addName: $("add-name"), addSubmit: $("add-submit"), addError: $("add-error"),
    usersCard: $("users-card"), usersUpdated: $("users-updated"), refresh: $("refresh"),
    usersError: $("users-error"), usersEmpty: $("users-empty"), usersLoading: $("users-loading"),
    usersTable: $("users-table"), usersBody: $("users-body"),
    pageLimitsForm: $("page-limits-form"), pageLimitsSave: $("page-limits-save"),
    pageLimitsRetry: $("page-limits-retry"), pageLimitsError: $("page-limits-error"),
    pageLimitsStatus: $("page-limits-status"),
    updateNotice: $("update-notice"), updateCurrent: $("update-current"), updateLatest: $("update-latest"),
    updateImage: $("update-image"), updateLink: $("update-link"),
    announce: $("announce"),
    confirm: $("confirm"), confirmTitle: $("confirm-title"), confirmBody: $("confirm-body"),
    confirmOk: $("confirm-ok"), confirmCancel: $("confirm-cancel"),
  };

  // ---------------------------------------------------------------- safety

  // host-class:begin (admin.browser-test.mjs runs this block against
  // crates/url-policy/testdata/host_vectors.json)
  // "loopback", "tailscale" or "other" for a host, taken exactly as given: a
  // trailing dot makes it a name for the resolver ("other"); only a MagicDNS
  // name may end in a dot. Same rules as apps/desktop/src/api/serverUrl.ts
  // and the metro-url-policy crate. The page passes location.hostname, which
  // the browser has already put in canonical form.
  function hostClass(hostname) {
    let h = hostname.toLowerCase();
    if (h.startsWith("[") && h.endsWith("]")) h = h.slice(1, -1);
    if (!h) return "other";
    if (h === "localhost") return "loopback";
    const name = h.endsWith(".") ? h.slice(0, -1) : h;
    if (name.endsWith(".ts.net")) {
      const labels = name.slice(0, -".ts.net".length);
      if (labels !== "" && labels.split(".").every((l) => /^[a-z0-9](?:[a-z0-9-]*[a-z0-9])?$/.test(l))) return "tailscale";
    }
    const ipv4 = (text) => {
      const m = /^(0|[1-9]\d{0,2})\.(0|[1-9]\d{0,2})\.(0|[1-9]\d{0,2})\.(0|[1-9]\d{0,2})$/.exec(text);
      if (!m) return null;
      const o = m.slice(1).map(Number);
      return o.every((n) => n <= 255) ? o : null;
    };
    const classifyV4 = (o) => {
      if (o[0] === 127) return "loopback";
      if (o[0] === 100 && o[1] >= 64 && o[1] <= 127) return "tailscale";
      return "other";
    };
    // The eight 16-bit groups of an IPv6 address (IPv4 tail allowed), or null.
    const ipv6 = (host) => {
      if (!host.includes(":") || !/^[0-9a-f:.]+$/.test(host)) return null;
      let text = host;
      const tail = [];
      const cut = text.lastIndexOf(":");
      const last = text.slice(cut + 1);
      if (last.includes(".")) {
        const v4 = ipv4(last);
        if (!v4) return null;
        tail.push((v4[0] << 8) | v4[1], (v4[2] << 8) | v4[3]);
        text = text.slice(0, cut);
        if (text.endsWith(":")) text += ":";
      }
      const halves = text.split("::");
      if (halves.length > 2) return null;
      const parse = (x) => (x === "" ? [] : x.split(":"));
      const head = parse(halves[0]);
      const rest = halves.length === 2 ? parse(halves[1]) : [];
      if ([...head, ...rest].some((g) => !/^[0-9a-f]{1,4}$/.test(g))) return null;
      const hex = (g) => parseInt(g, 16);
      const width = 8 - tail.length;
      let groups;
      if (halves.length === 2) {
        const fill = width - head.length - rest.length;
        if (fill < 1) return null;
        groups = [...head.map(hex), ...new Array(fill).fill(0), ...rest.map(hex)];
      } else {
        if (head.length !== width) return null;
        groups = head.map(hex);
      }
      return [...groups, ...tail];
    };
    const v4 = ipv4(h);
    if (v4) return classifyV4(v4);
    const v6 = ipv6(h);
    if (v6) {
      if (v6.slice(0, 7).every((g) => g === 0) && v6[7] === 1) return "loopback";
      if (v6.slice(0, 5).every((g) => g === 0) && v6[5] === 0xffff) {
        return classifyV4([v6[6] >> 8, v6[6] & 0xff, v6[7] >> 8, v6[7] & 0xff]);
      }
      if (v6[0] === 0xfd7a && v6[1] === 0x115c && v6[2] === 0xa1e0) return "tailscale";
    }
    return "other";
  }
  // host-class:end

  // Plain HTTP to a host that is neither loopback nor Tailscale: the token
  // would cross the network unencrypted, so the person is asked first.
  function plainHttpRisk() {
    return location.protocol === "http:" && hostClass(location.hostname) === "other";
  }

  // The choice lives in localStorage, shared by this server's tabs, and is
  // read again before every token send. Only when storage can't be used at
  // all is it kept in this page's memory instead.
  const PLAIN_HTTP_KEY = "metro.admin.plainHttpAccepted";
  let plainHttpAcceptedHere = false; // only when localStorage is unavailable
  let storageWorks = null;
  function storageAvailable() {
    if (storageWorks !== null) return storageWorks;
    try {
      const probe = PLAIN_HTTP_KEY + ".probe";
      localStorage.setItem(probe, "1");
      localStorage.removeItem(probe);
      storageWorks = true;
    } catch {
      storageWorks = false;
    }
    return storageWorks;
  }
  function plainHttpAccepted() {
    if (!storageAvailable()) return plainHttpAcceptedHere;
    try { return localStorage.getItem(PLAIN_HTTP_KEY) === "1"; } catch { return false; }
  }
  function acceptPlainHttp() {
    if (!storageAvailable()) { plainHttpAcceptedHere = true; return; }
    try { localStorage.setItem(PLAIN_HTTP_KEY, "1"); } catch { plainHttpAcceptedHere = true; storageWorks = false; }
  }
  // Withdraws the choice: the warning shows again before the next sign-in.
  // Signing out keeps it; only this does.
  function forgetPlainHttp() {
    plainHttpAcceptedHere = false;
    try { localStorage.removeItem(PLAIN_HTTP_KEY); } catch { /* nothing kept */ }
  }

  // Returns null when this page may take a credential, else the reason.
  function unsafeReason() {
    let framed = true;
    try { framed = window.top !== window.self; } catch { framed = true; }
    if (framed) {
      return "This page is being shown inside another page. That page could watch what you type, so the panel only works when opened directly in its own tab.";
    }
    const proto = location.protocol;
    if (proto !== "https:" && proto !== "http:") {
      return "This page wasn't loaded from a Metro server, so there's no server it can safely send a token to.";
    }
    // Browsers never treat plain HTTP outside loopback as a secure context;
    // that's what the plain-HTTP warning is for.
    if (proto === "https:" && !window.isSecureContext) {
      return "Your browser doesn't treat this page as a secure context.";
    }
    return null;
  }

  // The sign-in form, or first the plain-HTTP warning when it applies and
  // hasn't been accepted for this server yet.
  function showSignIn() {
    if (plainHttpRisk() && !plainHttpAccepted()) {
      el.plainHttpReason.textContent = "Anyone on the network between you and " + location.host +
        " could read the admin token you sign in with. Use it on networks you trust, such as Tailscale or a VPN.";
      el.signin.hidden = true;
      el.plainHttp.hidden = false;
      el.plainHttpAccept.focus();
      return false;
    }
    el.plainHttp.hidden = true;
    el.plainHttpNote.hidden = !plainHttpRisk();
    el.signin.hidden = false;
    return true;
  }

  function showUnsafe(reason) {
    el.unsafeReason.textContent = reason;
    const port = location.port || (location.protocol === "https:" ? "443" : "80");
    const host = location.hostname || "<metro-host>";
    el.unsafeSsh.textContent = "ssh -N -L 8080:127.0.0.1:" + port + " <you>@" + host;
    el.unsafeLoopback.textContent = "http://127.0.0.1" + (location.port ? ":" + location.port : "") + "/admin";
    el.signin.hidden = true;
    el.plainHttp.hidden = true;
    el.unsafe.hidden = false;
  }

  // Credentials in a URL are never read; drop them from the address bar
  // and history so they're not kept around any longer.
  function scrubUrl() {
    if (!location.search && !location.hash) return false;
    const looksSecret = /token|bearer|secret|key|auth|password/i.test(location.search + location.hash);
    try { history.replaceState(null, "", location.pathname); } catch { /* ignore */ }
    return looksSecret;
  }

  // ---------------------------------------------------------------- session

  const NAME_MAX = 64; // the server's display-name limit, in characters

  class SessionGone extends Error {}
  class ApiError extends Error {
    constructor(status, message) { super(message); this.status = status; }
  }

  let session = null; // { token, abort: AbortController, me }
  let state = freshState();

  function freshState() {
    return { users: [], loadedAt: null, loading: false, busy: new Set(),
      pageLimits: null, pageLimitsBusy: false };
  }

  async function api(method, path, body) {
    const s = session;
    if (!s) throw new SessionGone();
    if (unsafeReason()) throw new ApiError(0, "This page can't send credentials.");
    if (plainHttpRisk() && !plainHttpAccepted()) throw new ApiError(0, "Confirm the unencrypted connection first.");
    const url = new URL(path, location.origin);
    if (url.origin !== location.origin) throw new ApiError(0, "Refusing a cross-origin request.");
    const headers = { Authorization: "Bearer " + s.token, Accept: "application/json" };
    const init = {
      method, headers,
      mode: "same-origin", credentials: "omit", redirect: "error",
      cache: "no-store", referrerPolicy: "no-referrer", signal: s.abort.signal,
    };
    if (body !== undefined) {
      headers["Content-Type"] = "application/json";
      init.body = JSON.stringify(body);
    }
    let res;
    try {
      res = await fetch(url.href, init);
    } catch (e) {
      if (session !== s) throw new SessionGone();
      throw new ApiError(0, navigator.onLine === false
        ? "You appear to be offline."
        : "Couldn't reach the Metro server. Check that it's running and try again.");
    }
    if (session !== s) throw new SessionGone();
    let data = null;
    if (res.status !== 204 && (res.headers.get("content-type") || "").includes("application/json")) {
      try { data = await res.json(); } catch { data = null; }
    }
    if (session !== s) throw new SessionGone();
    if (res.status === 401) {
      endSession("The server didn't accept the token, or it has been revoked. Sign in again.");
      throw new SessionGone();
    }
    if (!res.ok) {
      const msg = data && typeof data.error === "string" && data.error
        ? data.error.slice(0, 300)
        : "The server answered " + res.status + (res.statusText ? " " + res.statusText : "") + ".";
      throw new ApiError(res.status, msg);
    }
    return data;
  }

  function errorText(e) {
    if (e instanceof ApiError) return e.message;
    return "Something went wrong. Try again.";
  }

  // A 403 on an admin route means the account lost admin rights: no
  // non-admin view exists, so the session ends.
  function handleForbidden(e) {
    if (e instanceof ApiError && e.status === 403) {
      endSession("This account is no longer an organization admin. You've been signed out.");
      return true;
    }
    return false;
  }

  function validToken(t) {
    return /^[\x21-\x7e]+$/.test(t) && t.length <= 4096;
  }

  async function signIn(ev) {
    ev.preventDefault();
    hideAlert(el.signinError);
    const reason = unsafeReason();
    if (reason) { el.token.value = ""; showUnsafe(reason); return; }
    if (plainHttpRisk() && !plainHttpAccepted()) { el.token.value = ""; showSignIn(); return; }
    let token = el.token.value.trim().replace(/^Bearer\s+/i, "");
    if (!token) {
      el.token.setAttribute("aria-invalid", "true");
      showAlert(el.signinError, "Enter an admin access token.");
      el.token.focus();
      return;
    }
    if (!validToken(token)) {
      el.token.setAttribute("aria-invalid", "true");
      showAlert(el.signinError, "That doesn't look like an access token. Tokens contain no spaces or special characters.");
      el.token.focus();
      return;
    }
    el.token.removeAttribute("aria-invalid");
    el.token.value = "";
    el.signinSubmit.disabled = true;
    el.signinSubmit.textContent = "Signing in…";
    session = { token, abort: new AbortController(), me: null };
    token = null;
    const s = session;
    try {
      const me = await api("GET", "/v1/me");
      if (!me || typeof me.id !== "string" || typeof me.is_org_admin !== "boolean") {
        throw new ApiError(0, "This Metro server doesn't report admin status, so the panel can't be used with it.");
      }
      if (!me.is_org_admin) {
        dropSession();
        el.deniedId.textContent = me.id;
        el.signin.hidden = true;
        el.denied.hidden = false;
        el.deniedBack.focus();
        return;
      }
      s.me = me;
      el.whoName.textContent = typeof me.actor === "string" && me.actor ? me.actor : me.id;
      el.whoId.textContent = me.actor && me.actor !== me.id ? "(" + me.id + ")" : "";
      el.signin.hidden = true;
      el.sessionBar.hidden = false;
      el.panel.hidden = false;
      announce("Signed in. Loading managed users.");
      el.addName.focus();
      loadUpdateNotice(s);
      loadPageLimits();
      await loadUsers();
    } catch (e) {
      if (e instanceof SessionGone) return;
      if (session === s) dropSession();
      showAlert(el.signinError, errorText(e));
      el.token.focus();
    } finally {
      el.signinSubmit.disabled = false;
      el.signinSubmit.textContent = "Sign in";
    }
  }

  // Forget everything tied to the session: token, in-flight requests,
  // user list, Page limits and any one-time secret.
  function dropSession() {
    if (session) { session.abort.abort(); session.token = null; }
    session = null;
    state = freshState();
    clearSecret();
    el.usersBody.replaceChildren();
    el.usersTable.hidden = true;
    el.usersEmpty.hidden = true;
    el.usersLoading.hidden = true;
    el.usersUpdated.textContent = "";
    el.usersCard.setAttribute("aria-busy", "false");
    hideAlert(el.usersError);
    hideAlert(el.addError);
    el.addName.value = "";
    resetPageLimits();
    el.token.value = "";
    el.whoName.textContent = "";
    el.whoId.textContent = "";
    el.panel.hidden = true;
    el.updateNotice.hidden = true;
    el.sessionBar.hidden = true;
    if (el.confirm.open) el.confirm.close("cancel");
  }

  function endSession(message) {
    dropSession();
    el.denied.hidden = true;
    const reason = unsafeReason();
    if (reason) { showUnsafe(reason); return; }
    if (!showSignIn()) {
      if (message) showAlert(el.plainHttpStatus, message); else hideAlert(el.plainHttpStatus);
      return;
    }
    if (message) showAlert(el.signinError, message); else hideAlert(el.signinError);
    el.token.focus();
  }

  // ---------------------------------------------------------------- updates

  // Operator notice from the server's own verified daily release check.
  // Optional: an older server (404) or an error just shows nothing.
  async function loadUpdateNotice(s) {
    try {
      const u = await api("GET", "/v1/server/update");
      if (session !== s) return;
      el.updateNotice.hidden = true;
      if (!u || u.available !== true || u.latest?.historical !== false) return;
      const version = String(u.latest.version || "");
      if (!/^[0-9]+\.[0-9]+\.[0-9]+$/.test(version)) return;
      el.updateCurrent.textContent = String(u.current || "");
      el.updateLatest.textContent = version;
      el.updateImage.textContent = String((u.latest.images && u.latest.images.server) || "");
      // Built from a constant and the checked version, never from a server URL.
      el.updateLink.href = "https://github.com/wnzn/metro/releases/tag/v" + version;
      el.updateNotice.hidden = false;
    } catch (e) {
      // No notice; never end the session for this optional call.
    }
  }

  // ---------------------------------------------------------------- Page limits

  const PAGE_LIMITS = "/v1/admin/settings";
  const limitFields = [
    { key: "pages_per_space", input: $("pages-per-space") },
    { key: "versions_per_page", input: $("versions-per-page") },
  ];

  function showPageLimits(data) {
    if (!data || !limitFields.every(({ key }) => Number.isInteger(data[key]))) {
      throw new ApiError(0, "The server didn't return both Page limits as whole numbers.");
    }
    state.pageLimits = data;
    for (const { input, key } of limitFields) input.value = String(data[key]);
    el.pageLimitsForm.hidden = false;
  }

  function resetPageLimits() {
    el.pageLimitsForm.hidden = true;
    el.pageLimitsRetry.hidden = true;
    el.pageLimitsRetry.removeAttribute("aria-disabled");
    el.pageLimitsSave.removeAttribute("aria-disabled");
    el.pageLimitsStatus.textContent = "";
    hideAlert(el.pageLimitsError);
    for (const { input } of limitFields) {
      input.value = "";
      input.readOnly = false;
      input.removeAttribute("aria-invalid");
    }
  }

  async function loadPageLimits() {
    if (!session?.me?.is_org_admin || state.pageLimitsBusy || state.pageLimits) return;
    const s = session;
    state.pageLimitsBusy = true;
    el.pageLimitsRetry.setAttribute("aria-disabled", "true");
    hideAlert(el.pageLimitsError);
    el.pageLimitsStatus.textContent = "Loading Page limits…";
    try {
      const data = await api("GET", PAGE_LIMITS);
      if (session !== s) return;
      showPageLimits(data);
      const moveFocus = document.activeElement === el.pageLimitsRetry;
      el.pageLimitsRetry.hidden = true;
      if (moveFocus) limitFields[0].input.focus();
    } catch (e) {
      if (e instanceof SessionGone || session !== s || handleForbidden(e)) return;
      showAlert(el.pageLimitsError, "Couldn't load Page limits: " + errorText(e));
      el.pageLimitsRetry.hidden = false;
    } finally {
      if (session === s) {
        state.pageLimitsBusy = false;
        el.pageLimitsRetry.removeAttribute("aria-disabled");
        el.pageLimitsStatus.textContent = "";
      }
    }
  }

  async function savePageLimits(ev) {
    ev.preventDefault();
    if (!session?.me?.is_org_admin || !state.pageLimits || state.pageLimitsBusy) return;
    const s = session;
    hideAlert(el.pageLimitsError);
    el.pageLimitsStatus.textContent = "";
    const patch = {};
    const invalid = [];
    for (const { input, key } of limitFields) {
      const value = input.valueAsNumber;
      if (!input.validity.valid || !Number.isInteger(value)) {
        input.setAttribute("aria-invalid", "true");
        invalid.push(input);
      } else if (value !== state.pageLimits[key]) {
        patch[key] = value;
      }
    }
    if (invalid.length) {
      showAlert(el.pageLimitsError, invalid.map((input) =>
        input.labels[0].textContent + ": enter a whole number from " + input.min + " to " + input.max + "."
      ).join(" "));
      invalid[0].focus();
      return;
    }
    if (Object.keys(patch).length === 0) {
      el.pageLimitsStatus.textContent = "No changes to save.";
      return;
    }
    state.pageLimitsBusy = true;
    el.pageLimitsSave.setAttribute("aria-disabled", "true");
    el.pageLimitsStatus.textContent = "Saving Page limits…";
    for (const { input } of limitFields) input.readOnly = true;
    try {
      const data = await api("PATCH", PAGE_LIMITS, patch);
      if (session !== s) return;
      showPageLimits(data);
      el.pageLimitsStatus.textContent = "Page limits saved.";
    } catch (e) {
      if (e instanceof SessionGone || session !== s || handleForbidden(e)) return;
      el.pageLimitsStatus.textContent = "";
      showAlert(el.pageLimitsError, "Couldn't save Page limits: " + errorText(e) + " Select Save Page limits to retry.");
    } finally {
      if (session === s) {
        state.pageLimitsBusy = false;
        el.pageLimitsSave.removeAttribute("aria-disabled");
        for (const { input } of limitFields) input.readOnly = false;
      }
    }
  }

  // ---------------------------------------------------------------- users

  async function loadUsers() {
    if (!session || state.loading) return;
    const s = session;
    state.loading = true;
    el.refresh.disabled = true;
    el.usersCard.setAttribute("aria-busy", "true");
    if (!state.loadedAt) el.usersLoading.hidden = false;
    try {
      const data = await api("GET", "/v1/admin/users");
      if (!data || !Array.isArray(data.users)) throw new ApiError(0, "The server sent an unexpected user list.");
      state.users = data.users
        .filter((u) => u && typeof u.id === "string")
        .map((u) => ({ id: u.id, name: typeof u.name === "string" ? u.name : "", has_token: u.has_token === true }));
      state.loadedAt = new Date();
      hideAlert(el.usersError);
      render();
    } catch (e) {
      if (e instanceof SessionGone || session !== s) return;
      if (handleForbidden(e)) return;
      const base = e instanceof ApiError && e.status === 404 && !state.loadedAt
        ? "this Metro server doesn't offer the user admin API."
        : errorText(e);
      showAlert(el.usersError, state.loadedAt
        ? "Couldn't refresh the list: " + base + " Showing the list from " + fmtTime(state.loadedAt) + "; it may be out of date."
        : "Couldn't load users: " + base);
      el.usersUpdated.textContent = state.loadedAt ? "Out of date (from " + fmtTime(state.loadedAt) + ")" : "";
    } finally {
      if (session === s) {
        state.loading = false;
        el.refresh.disabled = false;
        el.usersLoading.hidden = true;
        el.usersCard.setAttribute("aria-busy", "false");
      }
    }
  }

  function fmtTime(d) {
    return d.toLocaleTimeString([], { hour: "2-digit", minute: "2-digit", second: "2-digit" });
  }

  function displayName(u) { return u.name.trim() ? u.name : "(no name)"; }

  function render() {
    const users = [...state.users].sort((a, b) =>
      displayName(a).localeCompare(displayName(b), undefined, { sensitivity: "base" }) || a.id.localeCompare(b.id));
    const rows = users.map(renderRow);
    el.usersBody.replaceChildren(...rows);
    el.usersTable.hidden = users.length === 0;
    el.usersEmpty.hidden = users.length !== 0 || !state.loadedAt;
    if (state.loadedAt) el.usersUpdated.textContent = "Updated " + fmtTime(state.loadedAt);
  }

  function button(label, cls, ariaLabel, onClick, disabled) {
    const b = document.createElement("button");
    b.type = "button";
    b.className = "btn btn-sm " + cls;
    b.textContent = label;
    b.setAttribute("aria-label", ariaLabel);
    b.disabled = !!disabled;
    b.addEventListener("click", onClick);
    return b;
  }

  function renderRow(u) {
    const tr = document.createElement("tr");
    const name = displayName(u);
    const busy = state.busy.has(u.id);

    const tdName = document.createElement("td");
    tdName.className = "name";
    tdName.textContent = name;

    const tdId = document.createElement("td");
    tdId.className = "uid";
    const idLabel = document.createElement("span");
    idLabel.className = "visually-hidden";
    idLabel.textContent = "User id: ";
    tdId.append(idLabel, document.createTextNode(u.id));

    const tdTok = document.createElement("td");
    const badge = document.createElement("span");
    badge.className = "badge " + (u.has_token ? "badge-ok" : "badge-none");
    badge.textContent = busy ? "Working…" : u.has_token ? "Active" : "No token";
    tdTok.append(badge);

    const tdAct = document.createElement("td");
    tdAct.className = "actions";
    const inner = document.createElement("div");
    inner.className = "actions-inner";
    inner.append(
      button(u.has_token ? "Rotate token" : "Issue token", "btn-secondary",
        (u.has_token ? "Rotate token for " : "Issue token for ") + name,
        () => issueToken(u), busy),
      button("Revoke", "btn-ghost-danger", "Revoke token for " + name,
        () => revokeToken(u), busy || !u.has_token),
    );
    tdAct.append(inner);

    tr.append(tdName, tdId, tdTok, tdAct);
    return tr;
  }

  function userPath(id) { return "/v1/admin/users/" + encodeURIComponent(id) + "/token"; }

  function setUser(id, patch) {
    const u = state.users.find((x) => x.id === id);
    if (u) Object.assign(u, patch);
  }

  async function withBusy(u, fn) {
    if (!session || state.busy.has(u.id)) return;
    const s = session;
    state.busy.add(u.id);
    render();
    hideAlert(el.usersError);
    try {
      await fn();
    } catch (e) {
      if (e instanceof SessionGone || session !== s) return;
      if (handleForbidden(e)) return;
      if (e instanceof ApiError && e.status === 404) {
        showAlert(el.usersError, displayName(u) + " no longer exists on the server. The list has been refreshed.");
        state.busy.delete(u.id);
        await loadUsers();
        return;
      }
      showAlert(el.usersError, errorText(e));
    } finally {
      if (session === s) { state.busy.delete(u.id); render(); }
    }
  }

  async function issueToken(u) {
    const name = displayName(u);
    if (u.has_token) {
      const ok = await confirmAction("Rotate token for " + name + "?",
        "Their current token stops working right away. You'll get a new token to hand over.", "Rotate token", false);
      if (!ok) return;
    }
    await withBusy(u, async () => {
      const data = await api("POST", userPath(u.id));
      if (!data || typeof data.token !== "string" || !data.token) throw new ApiError(0, "The server didn't return a token.");
      setUser(u.id, { has_token: true });
      showSecret(name, data.token, u.id);
    });
  }

  async function revokeToken(u) {
    const name = displayName(u);
    const ok = await confirmAction("Revoke token for " + name + "?",
      "They're signed out of Metro until you issue a new token. Their account and history stay.", "Revoke", true);
    if (!ok) return;
    await withBusy(u, async () => {
      await api("DELETE", userPath(u.id));
      setUser(u.id, { has_token: false });
      if (secretOwner === u.id) clearSecret();
      announce("Token revoked for " + name + ".");
    });
  }

  async function addUser(ev) {
    ev.preventDefault();
    if (!session) return;
    const s = session;
    hideAlert(el.addError);
    const name = el.addName.value.trim();
    if (!name) {
      el.addName.setAttribute("aria-invalid", "true");
      showAlert(el.addError, "Enter a display name.");
      el.addName.focus();
      return;
    }
    if ([...name].length > NAME_MAX) {
      el.addName.setAttribute("aria-invalid", "true");
      showAlert(el.addError, "Names can be at most " + NAME_MAX + " characters.");
      el.addName.focus();
      return;
    }
    el.addName.removeAttribute("aria-invalid");
    el.addSubmit.disabled = true;
    el.addSubmit.textContent = "Adding…";
    try {
      const data = await api("POST", "/v1/admin/users", { name });
      if (!data || typeof data.id !== "string" || typeof data.token !== "string" || !data.token) {
        throw new ApiError(0, "The server didn't return the new user's token.");
      }
      el.addName.value = "";
      const created = { id: data.id, name: typeof data.name === "string" ? data.name : name, has_token: true };
      if (!state.users.some((x) => x.id === created.id)) state.users.push(created);
      render();
      showSecret(displayName(created), data.token, created.id);
      loadUsers();
    } catch (e) {
      if (e instanceof SessionGone || session !== s) return;
      if (handleForbidden(e)) return;
      showAlert(el.addError, "Couldn't add the user: " + errorText(e));
    } finally {
      if (session === s) {
        el.addSubmit.disabled = false;
        el.addSubmit.textContent = "Add user and issue token";
      }
    }
  }

  // ---------------------------------------------------------------- one-time secret

  let secretOwner = null;

  function showSecret(name, token, ownerId) {
    secretOwner = ownerId;
    el.secretFor.textContent = name;
    el.secretValue.value = token;
    setRevealed(false);
    el.secretStatus.textContent = "";
    el.secret.hidden = false;
    announce("New token for " + name + " is ready. It's shown only once.");
    el.secret.scrollIntoView({ block: "nearest" });
    el.secretCopy.focus();
  }

  function clearSecret() {
    secretOwner = null;
    el.secretValue.value = "";
    setRevealed(false);
    el.secretFor.textContent = "";
    el.secretStatus.textContent = "";
    el.secret.hidden = true;
  }

  function setRevealed(on) {
    el.secretValue.type = on ? "text" : "password";
    el.secretReveal.setAttribute("aria-pressed", on ? "true" : "false");
    el.secretReveal.textContent = on ? "Hide" : "Reveal";
  }

  async function copySecret() {
    const value = el.secretValue.value;
    if (!value) return;
    if (navigator.clipboard && window.isSecureContext) {
      try {
        await navigator.clipboard.writeText(value);
        el.secretStatus.textContent = "Copied to the clipboard.";
        return;
      } catch { /* fall through to manual copy */ }
    }
    // Manual fallback: show and select the token so the person can copy it.
    setRevealed(true);
    el.secretValue.focus();
    el.secretValue.select();
    let copied = false;
    try { copied = document.execCommand("copy"); } catch { copied = false; }
    el.secretStatus.textContent = copied
      ? "Copied to the clipboard. The token is shown; press Hide when you're done."
      : "Your browser blocked automatic copying. The token is shown and selected: press Ctrl+C (⌘C on a Mac) to copy it, then press Hide.";
  }

  function finishSecret() {
    clearSecret();
    announce("Token hidden.");
    el.addName.focus();
  }

  // ---------------------------------------------------------------- helpers

  function showAlert(node, text) { node.textContent = text; node.hidden = false; }
  function hideAlert(node) { node.textContent = ""; node.hidden = true; }

  let announceTimer = 0;
  function announce(text) {
    el.announce.textContent = "";
    clearTimeout(announceTimer);
    announceTimer = setTimeout(() => { el.announce.textContent = text; }, 50);
  }

  function confirmAction(title, body, okLabel, danger) {
    if (typeof el.confirm.showModal !== "function") {
      return Promise.resolve(window.confirm(title + "\n\n" + body));
    }
    el.confirmTitle.textContent = title;
    el.confirmBody.textContent = body;
    el.confirmOk.textContent = okLabel;
    el.confirmOk.className = "btn " + (danger ? "btn-danger" : "btn-primary");
    el.confirm.returnValue = "cancel";
    return new Promise((resolve) => {
      const s = session;
      el.confirm.addEventListener("close", () => {
        resolve(el.confirm.returnValue === "ok" && session === s && s !== null);
      }, { once: true });
      el.confirm.showModal();
      el.confirmCancel.focus();
    });
  }

  // ---------------------------------------------------------------- wiring

  function init() {
    const hadSecretInUrl = scrubUrl();
    el.signinForm.addEventListener("submit", signIn);
    el.addForm.addEventListener("submit", addUser);
    el.pageLimitsForm.addEventListener("submit", savePageLimits);
    el.pageLimitsRetry.addEventListener("click", loadPageLimits);
    for (const { input } of limitFields) {
      input.addEventListener("input", () => {
        input.removeAttribute("aria-invalid");
        el.pageLimitsStatus.textContent = "";
      });
    }
    el.signOut.addEventListener("click", () => { endSession(null); announce("Signed out."); });
    el.deniedBack.addEventListener("click", () => endSession(null));
    el.refresh.addEventListener("click", () => loadUsers());
    el.secretReveal.addEventListener("click", () =>
      setRevealed(el.secretValue.type === "password"));
    el.secretCopy.addEventListener("click", copySecret);
    el.secretDone.addEventListener("click", finishSecret);
    el.plainHttpAccept.addEventListener("click", () => {
      hideAlert(el.plainHttpStatus);
      acceptPlainHttp();
      if (showSignIn()) el.token.focus();
    });
    // Another tab of this server withdrew the choice (or cleared storage):
    // a session here may not send its token again; it ends, and the warning
    // shows before the next sign-in.
    window.addEventListener("storage", (e) => {
      if (e.key !== null && e.key !== PLAIN_HTTP_KEY) return;
      if (!plainHttpRisk() || plainHttpAccepted()) return;
      if (session) {
        endSession("The choice to sign in over this unencrypted connection was withdrawn in another tab. You've been signed out.");
      } else if (!el.signin.hidden) {
        el.token.value = "";
        showSignIn();
      }
    });
    el.plainHttpForget.addEventListener("click", () => {
      forgetPlainHttp();
      el.token.value = "";
      hideAlert(el.signinError);
      showSignIn();
      announce("Choice forgotten. The warning shows again before you sign in.");
    });
    el.token.addEventListener("input", () => el.token.removeAttribute("aria-invalid"));
    el.addName.addEventListener("input", () => el.addName.removeAttribute("aria-invalid"));
    // Don't let the browser keep the session in its back/forward cache.
    window.addEventListener("pagehide", () => { if (session || !el.secret.hidden) dropSession(); });
    window.addEventListener("pageshow", (e) => { if (e.persisted) endSession(null); });

    const reason = unsafeReason();
    if (reason) { showUnsafe(reason); return; }
    if (hadSecretInUrl) {
      showAlert(el.signinError,
        "This page ignores credentials in its address. If you put a token in a link, it may be in your history or logs; rotate it.");
    }
    if (showSignIn()) el.token.focus();
  }

  if (document.readyState === "loading") document.addEventListener("DOMContentLoaded", init);
  else init();
})();
