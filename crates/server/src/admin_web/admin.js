// Metro Workspace admin panel.
//
// Security notes:
// - The admin token comes only from the sign-in form. It lives in one
//   in-memory session object, is never stored, logged or put in a URL, and
//   is dropped on sign-out, on a rejected token and when the page is hidden
//   or unloaded.
// - Requests go only to this page's own origin, with the token in an
//   Authorization header, no cookies, no redirects and no caching.
// - Sign-in is refused before any network call on plain HTTP to a
//   non-loopback host, inside a frame, or from a non-HTTP origin.
// - Server-supplied text is only ever written with textContent.
"use strict";

(() => {
  const $ = (id) => document.getElementById(id);
  const el = {
    sessionBar: $("session-bar"), whoName: $("who-name"), whoId: $("who-id"), signOut: $("sign-out"),
    unsafe: $("unsafe"), unsafeReason: $("unsafe-reason"), unsafeSsh: $("unsafe-ssh"),
    unsafeLocal: $("unsafe-local"), unsafeLoopback: $("unsafe-loopback"),
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
    updateNotice: $("update-notice"), updateCurrent: $("update-current"), updateLatest: $("update-latest"),
    updateImage: $("update-image"), updateLink: $("update-link"),
    announce: $("announce"),
    confirm: $("confirm"), confirmTitle: $("confirm-title"), confirmBody: $("confirm-body"),
    confirmOk: $("confirm-ok"), confirmCancel: $("confirm-cancel"),
  };

  // ---------------------------------------------------------------- safety

  function isLoopbackHost(host) {
    const h = host.toLowerCase();
    return h === "localhost" || h.endsWith(".localhost") || h === "[::1]" ||
      /^127(\.\d{1,3}){3}$/.test(h);
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
    if (proto === "http:" && !isLoopbackHost(location.hostname)) {
      return "This page was opened over plain HTTP from " + location.host +
        ". Anyone on the network path could read an admin token sent from here.";
    }
    if (!window.isSecureContext) {
      return "Your browser doesn't treat this page as a secure context.";
    }
    return null;
  }

  function showUnsafe(reason) {
    el.unsafeReason.textContent = reason;
    const port = location.port || (location.protocol === "https:" ? "443" : "80");
    const host = location.hostname || "<metro-host>";
    el.unsafeSsh.textContent = "ssh -N -L 8080:127.0.0.1:" + port + " <you>@" + host;
    el.unsafeLoopback.textContent = "http://127.0.0.1" + (location.port ? ":" + location.port : "") + "/admin";
    el.signin.hidden = true;
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
    return { users: [], loadedAt: null, loading: false, busy: new Set() };
  }

  async function api(method, path, body) {
    const s = session;
    if (!s) throw new SessionGone();
    if (unsafeReason()) throw new ApiError(0, "This page can't send credentials.");
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
  // user list and any one-time secret.
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
    el.signin.hidden = false;
    if (message) showAlert(el.signinError, message); else hideAlert(el.signinError);
    el.token.focus();
  }

  // ---------------------------------------------------------------- updates

  // Operator notice from the server's own verified daily release check.
  // Optional: an older server (404) or an error just shows nothing.
  async function loadUpdateNotice(s) {
    try {
      const u = await api("GET", "/v1/server/update");
      if (session !== s || !u || u.available !== true || !u.latest) return;
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
    el.signOut.addEventListener("click", () => { endSession(null); announce("Signed out."); });
    el.deniedBack.addEventListener("click", () => endSession(null));
    el.refresh.addEventListener("click", () => loadUsers());
    el.secretReveal.addEventListener("click", () =>
      setRevealed(el.secretValue.type === "password"));
    el.secretCopy.addEventListener("click", copySecret);
    el.secretDone.addEventListener("click", finishSecret);
    el.token.addEventListener("input", () => el.token.removeAttribute("aria-invalid"));
    el.addName.addEventListener("input", () => el.addName.removeAttribute("aria-invalid"));
    // Don't let the browser keep the session in its back/forward cache.
    window.addEventListener("pagehide", () => { if (session || !el.secret.hidden) dropSession(); });
    window.addEventListener("pageshow", (e) => { if (e.persisted) endSession(null); });

    const reason = unsafeReason();
    if (reason) { showUnsafe(reason); return; }
    el.signin.hidden = false;
    if (hadSecretInUrl) {
      showAlert(el.signinError,
        "This page ignores credentials in its address. If you put a token in a link, it may be in your history or logs; rotate it.");
    }
    el.token.focus();
  }

  if (document.readyState === "loading") document.addEventListener("DOMContentLoaded", init);
  else init();
})();
