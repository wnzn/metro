"""Shared real-server and scripted model helpers for the messaging e2e tests."""

from functools import partial
from http.server import ThreadingHTTPServer
import json
from pathlib import Path
import time

from device_connector import assert_hidden
from pages import poll
from settings import ModelHandler, serve_fixture, token
from smoke import (
    HOST, check_processes, clean_env, content_parts, request, require,
    server_port, start_child, wait_for_server,
)


def activity(event):
    return event.get("activity") or {}


def role_text(payload, role):
    return ["\n".join(content_parts(m.get("content"))) for m in payload["messages"]
            if m["role"] == role]


class Model(ThreadingHTTPServer):
    daemon_threads = True
    model = "fixture/messaging"

    def __init__(self, script):
        super().__init__((HOST, 0), ModelHandler)
        self.base_url = f"http://{HOST}:{self.server_port}/v1"
        self.key = token()
        self.error = None
        self.script = script
        self.calls = []

    def respond(self, method, path, auth, payload):
        require(auth == f"Bearer {self.key}", "wrong loopback chat credential")
        require((method, path) == ("POST", "/v1/chat/completions")
                and payload["model"] == self.model and not payload.get("stream"),
                "unexpected loopback chat request")
        self.calls.append(payload)
        reply = self.script(payload)
        if reply is None:
            return 400, {"error": {"message": "scripted completion failure"}}
        return 200, {
            "id": "chatcmpl-messaging", "object": "chat.completion", "created": int(time.time()),
            "model": self.model,
            "choices": [{"index": 0, "message": reply,
                         "finish_reason": "tool_calls" if "tool_calls" in reply else "stop"}],
            "usage": {"prompt_tokens": 1, "completion_tokens": 1, "total_tokens": 2},
        }


class Fixture:
    def __init__(self, stack, directory, binary, timeout, script, env=None, vendors=()):
        self.deadline = time.monotonic() + timeout
        self.tokens = {user: token() for user in ("alice", "bob", "carol", "never")}
        self.model = serve_fixture(stack, Model(script))
        self.vendors = (self.model, *vendors)
        self.secrets = (*self.tokens.values(), *(vendor.key for vendor in self.vendors))
        port = server_port()
        server_env = clean_env(directory) | {
            "METRO_BIND": f"{HOST}:{port}", "METRO_DB": str(Path(directory) / "metro.sqlite"),
            "METRO_DEV_USERS": json.dumps(self.tokens), "METRO_MODEL": self.model.model,
            "METRO_MODEL_BASE_URL": self.model.base_url, "METRO_MODEL_API_KEY": self.model.key,
        } | (env or {})
        self.children = [start_child(stack, "messaging server", binary, directory, server_env)]
        wait_for_server(port, self.children, self.model, self.deadline)
        self.request = partial(request, port, self.deadline)

    def check(self):
        for vendor in self.vendors:
            check_processes(self.children, vendor)

    def call(self, user, method, path, body=None, status=200):
        data = self.request(self.tokens[user], method, path, body=body, expect=(status,),
                            read_json=status != 204)
        assert_hidden(data, self.secrets)
        return data

    def events(self, user, place, after=0):
        return self.call(user, "GET", f"{place}/events?after={after}")["events"]

    def space(self, owner, name, *members):
        space = self.call(owner, "POST", "/v1/spaces", {"name": name}, 201)["id"]
        for user in members:
            self.call(owner, "POST", f"/v1/spaces/{space}/members", {"user_id": user}, 201)
        return space, f"/v1/spaces/{space}/rooms/lobby"

    def post(self, user, place, text, status=202, **fields):
        event = self.call(user, "POST", place + "/messages", {"text": text, **fields}, status)
        require(event["text"] == text and event["actor_id"] == user, "message didn't round-trip")
        return event

    def wait(self, what, fn):
        return poll(min(self.deadline, time.monotonic() + 12), what, fn, (self.check,))

    def phase(self, user, place, after, phase="completed", run_id=None):
        return self.wait(phase, lambda: next((
            e for e in self.events(user, place, after)
            if activity(e).get("phase") == phase
            and (run_id is None or activity(e).get("id") == run_id)
        ), None))
