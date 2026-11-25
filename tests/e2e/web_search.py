#!/usr/bin/env python3
"""Test web search with the real metro-server and metro-connector, a scripted model and loopback vendors.

Runs entirely on 127.0.0.1: the connector's vendor URLs are replaced by a
loopback fixture through METRO_WEB_TEST_BASE_* (which only accepts loopback
URLs). No real vendor is called.

Checks:
1. Bob owns a Space and allows web_search on its agent. Alice registers her
   computer (`metro-connector web setup`, session token from a private
   file), saves a Firecrawl key there and serves web search. Her @agent
   message makes the model call web_search: Tavily (free) is rate-limited,
   Exa (free, MCP) answers. The room shows the exact query, a summary with
   every attempt, and the reply. The search ran on Alice's connector.
2. Bob has no web device: his run gets no web tools and a note instead.
3. Carol registered a computer that isn't running: her search fails at once
   with a visible error, and the agent still answers.
   The model gets the result framed as untrusted web data; the scripted
   model answers with its own summary (never an echo), and the next turn's
   history carries neither the web text nor its framing.
4. Every provider limited, and Firecrawl echoes Alice's key back in a
   successful answer: that answer is refused on her computer; the error
   lists each provider; TinyFish needs a key.
5. A scheduled prompt of Carol's (offline computer) answers without web
   data, with the error visible in the room.
6. The Firecrawl key never reaches the server's database, its output or
   the connector's output; only the Firecrawl fixture saw it.
"""

from contextlib import ExitStack
from functools import partial
from http.server import BaseHTTPRequestHandler, HTTPServer
import datetime
import json
import os
from pathlib import Path
import secrets
import signal
import subprocess
import threading
import time
import uuid
from urllib.parse import quote

from smoke import (
    HOST, MAX_BODY, clean_env, content_parts, main, read_events, remaining, request, require,
    server_port, stop_child, temp_state, terminate, text_reply, tool_reply, wait_for_server,
)

MODEL = "metro-web-e2e-model"
MODEL_KEY = "local-web-e2e-not-a-real-key"


class Fixture(HTTPServer):
    """Scripted model (OpenAI Chat Completions) and web vendors on one port."""

    def __init__(self):
        super().__init__((HOST, 0), Handler)
        self.base = f"http://{HOST}:{self.server_port}"
        self.error = None
        self.lock = threading.Lock()
        self.model_requests = []
        self.vendor_hits = []
        # Vendor behaviour: "ok" or "limited".
        self.mode = {"tavily": "limited", "exa": "ok", "parallel": "limited",
                     "firecrawl": "limited", "tinyfish": "limited"}
        self.firecrawl_auth = []
        self.query = f"metro web e2e {uuid.uuid4().hex[:8]}"
        self.exa_title = f"Exa result {uuid.uuid4().hex[:8]}"

    def handle_error(self, _request, _address):
        if self.error is None:
            self.error = "fixture HTTP handler failed"

    # -- model

    def complete(self, req):
        messages = req["messages"]
        tools = {t["function"]["name"] for t in req.get("tools", []) if t.get("type") == "function"}
        system = "\n".join(
            part for m in messages if m["role"] == "system" for part in content_parts(m.get("content"))
        )
        users = [m for m in messages if m["role"] == "user"]
        last_user = "\n".join(content_parts(users[-1].get("content"))) if users else ""
        with self.lock:
            self.model_requests.append({"tools": tools, "system": system, "user": last_user,
                                        "last": messages[-1], "all": json.dumps(messages)})
        last = messages[-1]
        if last["role"] == "tool":
            # A summary of the assistant's own: never an echo of tool output.
            return text_reply(f"ANSWER summarized {uuid.uuid4().hex[:8]}")
        if "SEARCH-ME" in last_user and "web_search" in tools:
            return tool_reply(f"call_{uuid.uuid4().hex}", {
                "name": "web_search", "arguments": json.dumps({"query": self.query}),
            })
        return text_reply("ANSWER plain")

    # -- vendors

    def vendor(self, path, headers, body):
        name = path.strip("/").split("/")[0].split("?")[0]
        with self.lock:
            self.vendor_hits.append(name)
            if name == "firecrawl":
                self.firecrawl_auth.append(headers.get("Authorization") or "")
        mode = self.mode.get(name, "limited")
        if name == "exa":
            msg = json.loads(body or b"{}")
            method = msg.get("method")
            if method == "initialize":
                return 200, {"jsonrpc": "2.0", "id": msg["id"], "result": {
                    "protocolVersion": "2025-11-25", "capabilities": {}, "serverInfo": {"name": "fx"}}}
            if method == "notifications/initialized":
                return 202, None
            if mode != "ok":
                return 429, {"error": "limited"}
            text = f"Title: {self.exa_title}\nURL: https://exa.example/result\nPublished: 2026-11-20\n"
            return 200, {"jsonrpc": "2.0", "id": msg["id"],
                         "result": {"content": [{"type": "text", "text": text}]}}
        if name == "parallel":
            msg = json.loads(body or b"{}")
            if msg.get("method") == "initialize":
                return 200, {"jsonrpc": "2.0", "id": msg["id"], "result": {
                    "protocolVersion": "2025-11-25", "capabilities": {}}}
            if msg.get("method") == "notifications/initialized":
                return 202, None
            return 429, {"error": "limited"}
        if mode == "ok":
            return 200, {"results": [{"title": "T", "url": "https://t.example/", "content": "c"}]}
        if mode == "reflect":
            # A vendor that echoes the credential back in a successful answer.
            echoed = (headers.get("Authorization") or "").removeprefix("Bearer ")
            return 200, {"success": True, "data": {"web": [
                {"title": f"echo {echoed}", "url": "https://r.example/", "description": "d"}]}}
        return 429, {"error": "limited"}


class Handler(BaseHTTPRequestHandler):
    def log_message(self, *_):
        pass

    def do_GET(self):
        self.dispatch()

    def do_POST(self):
        self.dispatch()

    def dispatch(self):
        fx = self.server
        try:
            size = int(self.headers.get("Content-Length", "0"))
            require(0 <= size <= MAX_BODY, "fixture request too large")
            body = self.rfile.read(size) if size else b""
            if self.path == "/v1/chat/completions":
                require(self.headers.get("Authorization") == f"Bearer {MODEL_KEY}",
                        "model fixture got the wrong key")
                message = fx.complete(json.loads(body))
                status, payload = 200, {
                    "id": f"chatcmpl-{uuid.uuid4().hex}", "object": "chat.completion",
                    "created": int(time.time()), "model": MODEL,
                    "choices": [{"index": 0, "message": message,
                                 "finish_reason": "tool_calls" if "tool_calls" in message else "stop"}],
                    "usage": {"prompt_tokens": 1, "completion_tokens": 1, "total_tokens": 2},
                }
            else:
                status, payload = fx.vendor(self.path, self.headers, body)
        except Exception as exc:
            if fx.error is None:
                fx.error = str(exc) if type(exc) is RuntimeError else "fixture rejected a request"
            status, payload = 400, {"error": "fixture"}
        data = b"" if payload is None else json.dumps(payload).encode()
        self.send_response(status)
        if payload is not None:
            self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(data)))
        self.end_headers()
        self.wfile.write(data)


def private_file(path, value):
    with os.fdopen(os.open(path, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600), "w") as stream:
        stream.write(f"{value}\n")


class Guarded:
    """A child whose output is scanned in memory for a secret, never kept or printed."""

    def __init__(self, stack, args, cwd, env, secret):
        self.child = subprocess.Popen(args, cwd=cwd, env=env, stdin=subprocess.DEVNULL,
                                      stdout=subprocess.PIPE, stderr=subprocess.STDOUT)
        self.seen = threading.Event()
        encoded = secret.encode()

        def drain():
            tail = b""
            with self.child.stdout as out:
                while chunk := out.read1(8192):
                    window = tail + chunk
                    if encoded in window:
                        self.seen.set()
                    tail = window[-(len(encoded) - 1):]

        self.reader = threading.Thread(target=drain)
        self.reader.start()
        stack.callback(self.finish)

    def finish(self):
        stop_child(self.child)
        self.reader.join(3)
        require(not self.seen.is_set(), "the provider key appeared in a process's output")

    def wait(self, deadline, expected, label):
        try:
            code = self.child.wait(timeout=remaining(deadline))
        except subprocess.TimeoutExpired:
            raise TimeoutError(f"{label} did not finish") from None
        require(code == expected, f"{label}: exit status {code}")


def wait_events(call, user, room, deadline, fx, predicate, label):
    while True:
        require(fx.error is None, fx.error or "")
        events = read_events(call, user, room)
        if predicate(events):
            return events
        if time.monotonic() > deadline:
            raise TimeoutError(f"timed out waiting for {label}")
        time.sleep(0.05)


def agent_reply_after(events, event_id):
    return [e for e in events if e["id"] > event_id and e["kind"] == "agent"]


def web_search(server_binary, connector, timeout):
    alice, bob, carol = (secrets.token_hex(24) for _ in range(3))
    canary = f"fc-canary-{secrets.token_hex(16)}"
    with temp_state() as directory, ExitStack() as stack:
        state = Path(directory)
        deadline = time.monotonic() + timeout
        fx = stack.enter_context(Fixture())
        thread = threading.Thread(target=fx.serve_forever, kwargs={"poll_interval": 0.05})
        thread.start()
        stack.callback(thread.join, 3)
        stack.callback(fx.shutdown)

        port = server_port()
        db = state / "metro.sqlite"
        env = clean_env(directory)
        server = Guarded(stack, [str(server_binary)], state, env | {
            "METRO_BIND": f"{HOST}:{port}",
            "METRO_DB": str(db),
            "METRO_DEV_USERS": json.dumps({"alice": alice, "bob": bob, "carol": carol}),
            "METRO_MODEL_BASE_URL": f"{fx.base}/v1",
            "METRO_MODEL": MODEL,
            "METRO_MODEL_API_KEY": MODEL_KEY,
            "METRO_SCHEDULE_TICK_SECS": "1",
            "RUST_LOG": "metro_server=debug",
        }, canary)
        wait_for_server(port, [(server.child, "server")], fx, deadline)
        call = partial(request, port, deadline)

        # Bob's Space; Alice and Carol are members; web_search allowed.
        space = call(bob, "POST", "/v1/spaces", body={"name": "Web"}, read_json=True)
        sid = space["id"]
        base = f"/v1/spaces/{quote(sid, safe='')}"
        room = f"{base}/rooms/lobby"
        for user in ("alice", "carol"):
            call(bob, "POST", f"{base}/members", body={"user_id": user})
        agents = call(bob, "GET", f"{base}/agents", read_json=True)
        require("web_search" not in agents["agents"][0]["tools"], "web tools were on by default")
        aid = agents["agents"][0]["id"]
        tools = agents["agents"][0]["tools"] + ["web_search", "web_fetch"]
        call(bob, "PATCH", f"{base}/agents/{quote(aid, safe='')}", body={"tools": tools})

        # Alice and Carol register their computers; only Alice's runs.
        vendor_env = {f"METRO_WEB_TEST_BASE_{p.upper()}": f"{fx.base}/{p}"
                      for p in ("tavily", "exa", "parallel", "firecrawl")}
        vendor_env["METRO_WEB_TEST_BASE_TINYFISH_SEARCH"] = f"{fx.base}/tinyfish"
        vendor_env["METRO_WEB_TEST_BASE_TINYFISH_FETCH"] = f"{fx.base}/tinyfish"
        for who, token in (("alice", alice), ("carol", carol)):
            home = state / f"{who}-home"
            home.mkdir(mode=0o700)
            session_file = state / f"{who}-session"
            private_file(session_file, token)
            cenv = env | vendor_env | {"METRO_CONNECTOR_HOME": str(home), "METRO_CONNECTOR_POLL_MS": "50"}
            Guarded(stack, [str(connector), "web", "setup", "--server-url", f"http://{HOST}:{port}",
                            "--session-token-file", str(session_file), "--label", f"{who} e2e"],
                    state, cenv, canary).wait(deadline, 0, f"{who}'s web setup")
            session_file.unlink()
            if who == "alice":
                key_file = state / "fc-key"
                private_file(key_file, canary)
                Guarded(stack, [str(connector), "web", "key", "set", "firecrawl", "--from-file",
                                str(key_file)], state, cenv, canary).wait(deadline, 0, "key set")
                key_file.unlink()
                alice_env = cenv
        worker = Guarded(stack, [str(connector), "web", "run"], state, alice_env, canary)
        while True:
            devices = call(alice, "GET", "/v1/web-devices", read_json=True)["devices"]
            require(len(devices) == 1, "Alice should have one web device")
            if devices[0]["online"]:
                break
            require(worker.child.poll() is None, "Alice's web connector exited")
            time.sleep(0.05)
            remaining(deadline)
        require(call(bob, "GET", "/v1/web-devices", read_json=True)["devices"] == [],
                "Bob saw web devices")

        # 1. Alice searches: Tavily limited, Exa answers, on Alice's computer.
        sent = call(alice, "POST", f"{room}/messages", body={"text": "@agent SEARCH-ME please"},
                    read_json=True)
        events = wait_events(call, alice, room, deadline, fx,
                             lambda ev: agent_reply_after(ev, sent["id"]), "Alice's reply")
        mine = [e for e in events if e["id"] > sent["id"]]
        calls = [e["text"] for e in mine if e["kind"] == "tool_call"]
        require(calls == [f'web_search "{fx.query}"'], "the exact query was not shown")
        results = [e for e in mine if e["kind"] == "tool_result"]
        require(len(results) == 1, "expected one web_search result")
        summary = results[0]["text"]
        # Exa's text output is shown as text, never turned into results.
        require(summary.startswith("web_search via Exa (free): results as text"),
                "wrong provider summary")
        require("Tried: Tavily (free) rate-limited" in summary
                and "→ Exa (free) ok" in summary, "the rotation was not shown")
        require(fx.exa_title not in json.dumps(mine), "web text reached the room")
        note = results[0]["activity"]["tool"]
        require(note["provider"] == "exa" and [a["outcome"] for a in note["attempts"]] ==
                ["limited", "ok"], "activity.tool did not carry the attempts")
        # The framing is checked on the tool message the model received.
        tool_msgs = [r["last"] for r in fx.model_requests if r["last"]["role"] == "tool"]
        require(len(tool_msgs) == 1, "expected one tool message")
        told = "\n".join(content_parts(tool_msgs[0].get("content")))
        require(told.startswith("<web-data id=\"") and 'source="exa"' in told
                and fx.exa_title in told and told.rstrip().endswith("\">")
                and "</web-data id=\"" in told, "the tool result was not framed as web data")
        reply = agent_reply_after(events, sent["id"])[0]["text"]
        require(reply.startswith("ANSWER summarized") and fx.exa_title not in reply,
                "the fixture must not echo tool output")
        require(fx.vendor_hits[:1] == ["tavily"] and "exa" in fx.vendor_hits,
                "the vendors were not tried in order")
        require("tinyfish" not in fx.vendor_hits, "TinyFish was called without a key")

        # 2. Bob has no computer for web search: no tools, a note.
        before = len(fx.model_requests)
        sent = call(bob, "POST", f"{room}/messages", body={"text": "@agent hello from Bob"},
                    read_json=True)
        wait_events(call, bob, room, deadline, fx, lambda ev: agent_reply_after(ev, sent["id"]),
                    "Bob's reply")
        req = fx.model_requests[before]
        require("web_search" not in req["tools"] and "Web search isn't set up for the person who asked"
                in req["system"], "Bob's run got web tools or no note")
        # Tool results are never history: the next turn in the room carries
        # neither the web text nor its framing.
        require(fx.exa_title not in req["all"] and "web-data" not in req["all"],
                "a tool result became model history")

        # 3. Carol's computer isn't running: a visible error at once.
        started = time.monotonic()
        sent = call(carol, "POST", f"{room}/messages", body={"text": "@agent SEARCH-ME carol"},
                    read_json=True)
        events = wait_events(call, carol, room, deadline, fx,
                             lambda ev: agent_reply_after(ev, sent["id"]), "Carol's reply")
        require(time.monotonic() - started < 10, "the offline failure was not immediate")
        errors = [e["text"] for e in events if e["id"] > sent["id"] and e["kind"] == "error"]
        require(any("Metro isn't running on carol's computer" in t for t in errors),
                "no visible offline error")

        # 4. Everything limited, and Firecrawl echoes Alice's key back in a
        # successful answer: that answer is refused on her computer.
        fx.mode["exa"] = "limited"
        fx.mode["firecrawl"] = "reflect"
        call(alice, "POST", "/v1/web-search/prefs", expect=(405,), label="prefs is GET/PUT")
        hits = len(fx.vendor_hits)
        sent = call(alice, "POST", f"{room}/messages", body={"text": "@agent SEARCH-ME again"},
                    read_json=True)
        events = wait_events(call, alice, room, deadline, fx,
                             lambda ev: agent_reply_after(ev, sent["id"]), "the exhausted reply")
        errors = [e["text"] for e in events if e["id"] > sent["id"] and e["kind"] == "error"]
        require(len(errors) == 1, "expected one exhausted error")
        for part in ("every web search provider on alice's computer is unavailable",
                     "Tavily (free): paused after an earlier limit", "Exa (free): rate-limited",
                     "Parallel (free): rate-limited", "Firecrawl (your key): sent an unusable answer",
                     "TinyFish: needs a free key"):
            require(part in errors[0], f"exhausted error is missing {part!r}")
        require("tavily" not in fx.vendor_hits[hits:], "a cooling provider was called")
        require(fx.firecrawl_auth and all(a == f"Bearer {canary}" for a in fx.firecrawl_auth),
                "Firecrawl did not get Alice's key")
        require(canary not in json.dumps(read_events(call, bob, room)),
                "the echoed key reached the room")
        require(all(canary not in r["all"] for r in fx.model_requests),
                "the echoed key reached the model")

        # 5. Carol's scheduled prompt: answered without web data.
        at = datetime.datetime.now(datetime.timezone.utc) + datetime.timedelta(seconds=3)
        call(carol, "POST", "/v1/schedules", expect=(201,), body={
            "name": "web brief", "prompt": "SEARCH-ME scheduled", "space_id": sid,
            "room_id": "lobby", "enabled": True,
            "schedule": {"kind": "once", "at": at.isoformat().replace("+00:00", "Z")},
        })
        last = read_events(call, carol, room)[-1]["id"]
        events = wait_events(call, carol, room, deadline, fx,
                             lambda ev: agent_reply_after(ev, last), "the scheduled reply")
        errors = [e["text"] for e in events if e["id"] > last and e["kind"] == "error"]
        require(any("Metro isn't running on carol's computer" in t for t in errors),
                "the scheduled prompt's search failure was not visible")

        # 6. The key stayed on Alice's computer.
        stop_child(worker.child)
        for path in (db, Path(f"{db}-wal"), Path(f"{db}-journal")):
            if path.is_file():
                require(canary.encode() not in path.read_bytes(), "the provider key reached Metro's DB")
        require(fx.error is None, fx.error or "")
    print("PASS: web search ran on the asker's computer with rotation shown; no device, offline, "
          "exhausted and scheduled cases failed visibly; the provider key stayed on the device.")


if __name__ == "__main__":
    signal.signal(signal.SIGTERM, terminate)
    raise SystemExit(main(web_search, binaries=("metro-server", "metro-connector"),
                          description=__doc__))
