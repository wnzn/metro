#!/usr/bin/env python3
"""Exercise Metro provider and runner settings with real binaries and local fixtures."""

import base64
from contextlib import ExitStack
from functools import partial
import http.client
from http.server import BaseHTTPRequestHandler, HTTPServer
import json
from pathlib import Path
import secrets
import signal
import threading
import time
from urllib.parse import quote
import uuid

from smoke import (
    CLAIM, HOST, MAX_BODY, SPACES, check_processes, clean_env, content_parts, create_space,
    exercise_demos, main, read_events, remaining, request, require, server_port, start_child,
    stop_child, temp_state, terminate, text_reply, tool_reply, wait_for_server,
)

CATALOG = "/v1/provider-catalog"
PROVIDERS = "/v1/providers"
RUNNERS = "/v1/runners"
CHAT = "/v1/chat"
WORKER_PROTOCOL = "X-Metro-Runner-Protocol"


def token():
    return secrets.token_hex(32)


def route(collection, item_id):
    require(isinstance(item_id, str) and bool(item_id), "missing resource ID")
    return f"{collection}/{quote(item_id, safe='')}"


class ModelHandler(BaseHTTPRequestHandler):
    def log_message(self, *_):
        pass

    def do_GET(self):
        self.dispatch("GET")

    def do_POST(self):
        self.dispatch("POST")

    def dispatch(self, method):
        fixture = self.server
        try:
            payload = None
            if method == "POST":
                size = int(self.headers["Content-Length"])
                require(0 < size <= MAX_BODY, "invalid model request size")
                payload = json.loads(self.rfile.read(size))
            status, body = fixture.respond(
                method, self.path, self.headers.get("Authorization"), payload,
            )
        except Exception as exc:
            # Only our fixed, controlled assertion messages are safe to surface.
            fixture.error = str(exc) if type(exc) is RuntimeError else "invalid model request"
            status, body = 400, {"error": {"message": "fixture rejected the request"}}
        data = json.dumps(body).encode()
        self.send_response(status)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(data)))
        self.end_headers()
        self.wfile.write(data)


class ModelFixture(HTTPServer):
    def __init__(self, model, api_key=None, *, can_list=True):
        super().__init__((HOST, 0), ModelHandler)
        self.base_url = f"http://{HOST}:{self.server_port}/v1"
        self.model = model
        self.api_key = api_key
        self.can_list = can_list
        self.error = None
        self.requests = 0
        self.list_calls = 0
        self.chat_calls = 0
        self.prompt = f"Settings test {uuid.uuid4().hex}"
        self.answer = f"Settings reply {uuid.uuid4().hex}"
        self.call_id = f"call_{uuid.uuid4().hex}"

    def get_request(self):
        connection, address = super().get_request()
        self.requests += 1
        connection.settimeout(2)
        return connection, address

    def respond(self, method, path, authorization, payload):
        expected_auth = f"Bearer {self.api_key}" if self.api_key is not None else None
        require(authorization == expected_auth, "wrong credentials on a fixture request")
        if method == "GET":
            require(path == "/v1/models", "unexpected fixture GET path")
            self.list_calls += 1
            if not self.can_list:
                return 404, {"error": {"message": "listing unavailable"}}
            return 200, {"object": "list", "data": [{"id": self.model, "object": "model"}]}
        require(path == "/v1/chat/completions", "unexpected fixture POST path")
        require(payload.get("model") == self.model, "wrong model selected")
        require(not payload.get("stream"), "unexpected streaming request")
        self.chat_calls += 1
        user_messages = [msg.get("content") for msg in payload["messages"] if msg["role"] == "user"]
        require(user_messages and self.prompt in "\n".join(content_parts(user_messages[-1])),
                "model received the wrong user's turn")
        if self.chat_calls == 1:
            tools = {item["function"]["name"] for item in payload.get("tools", [])
                     if item.get("type") == "function"}
            require(tools == {"react_to_message"}, "personal Chat exposed the wrong tools")
            message = tool_reply(self.call_id, {
                "name": "react_to_message", "arguments": json.dumps({"emoji": "👀"}),
            })
        else:
            require(self.chat_calls == 2, "unexpected additional model completion")
            require(any(msg["role"] == "tool" and msg.get("tool_call_id") == self.call_id
                        and list(content_parts(msg.get("content")))
                        for msg in payload["messages"]), "Rig did not replay the reaction tool result")
            message = text_reply(self.answer)
        return 200, {
            "id": f"chatcmpl-settings-{self.chat_calls}",
            "object": "chat.completion",
            "created": int(time.time()),
            "model": self.model,
            "choices": [{"index": 0, "message": message,
                         "finish_reason": "tool_calls" if "tool_calls" in message else "stop"}],
            "usage": {"prompt_tokens": 1, "completion_tokens": 1, "total_tokens": 2},
        }


def serve_fixture(stack, fixture):
    stack.enter_context(fixture)
    thread = threading.Thread(target=fixture.serve_forever, kwargs={"poll_interval": 0.05})
    thread.start()
    stack.callback(thread.join, 3)
    stack.callback(fixture.shutdown)
    return fixture


def public_list(call, user, path, field, private_values):
    data = call(user, "GET", path, read_json=True)
    require(isinstance(data, dict) and isinstance(data.get(field), list),
            f"expected a {field} array")
    check_redacted(data, private_values)
    return data[field]


def check_redacted(data, private_values):
    encoded = json.dumps(data)
    require(not any(value in encoded for value in private_values),
            "public response contained credential material")


def check_db_redacted(db_path, private_values):
    require(db_path.is_file() and db_path.stat().st_size > 0,
            "the fresh SQLite database was not created")
    # A live service may leave its newest writes in the WAL, not the main file.
    for path in (db_path, Path(f"{db_path}-wal"), Path(f"{db_path}-journal")):
        if path.is_file():
            data = path.read_bytes()
            require(not any(value.encode() in data for value in private_values),
                    "SQLite contained credential plaintext")


def wait_for_answer(call, user, fixture, children, deadline):
    while True:
        check_processes(children, fixture)
        events = read_events(call, user, CHAT)
        require(not any(event["kind"] == "error" for event in events),
                "personal Chat emitted an error")
        if any((event["kind"], event["actor"], event["text"]) ==
               ("agent", "agent", fixture.answer) for event in events):
            require(fixture.chat_calls == 2, "expected a two-turn Rig tool exchange")
            return
        time.sleep(min(0.05, remaining(deadline)))


def exercise_providers(call, alice, bob, keyed, manual, alternate, db_path, children, deadline):
    private_values = (keyed.api_key,)
    fixtures = (keyed, manual, alternate)
    catalog = call(alice, "GET", CATALOG, read_json=True)
    check_redacted(catalog, private_values)
    require(isinstance(catalog, dict) and isinstance(catalog.get("providers"), list),
            "catalog did not return a providers array")
    adapters = {entry["id"]: entry for entry in catalog["providers"]}
    require({"openai", "anthropic", "gcp.gemini", "custom"} <= adapters.keys(),
            "catalog did not expose the compiled providers and custom adapter")
    custom = adapters["custom"]
    require(custom["execution"] == "server" and custom["format"] == "openai"
            and custom["allow_custom_url"] is True and custom["requires_key"] is False
            and custom["lists_models"] is True, "custom adapter did not advertise its real capabilities")
    require(set(catalog["allowed_base_urls"]) == {fixture.base_url for fixture in fixtures},
             "catalog did not expose the exact configured fixture endpoints")
    require(all(f.requests == 0 for f in fixtures),
            "reading the catalog contacted an upstream fixture")

    def connections(user):
        return public_list(call, user, PROVIDERS, "connections", private_values)

    def create(user, name, fixture, **options):
        response = call(user, "POST", PROVIDERS, body={
            "name": name, "provider_id": custom["id"],
            "base_url": fixture.base_url, **options,
        }, expect=(201,), read_json=True)
        check_redacted(response, private_values)
        return route(PROVIDERS, response["id"]), response["id"]

    require(not connections(alice) and not connections(bob), "fresh users had saved connections")
    call(alice, "POST", PROVIDERS, body={
        "name": "Not allowlisted", "provider_id": custom["id"],
        "base_url": f"{keyed.base_url}/other", "model": keyed.model,
    }, expect=(400,), label="a different path on the allowlisted origin is not allowlisted")
    alice_route, alice_id = create(alice, "Alice authenticated", keyed, api_key=keyed.api_key)
    bob_route, bob_id = create(bob, "Bob manual model", manual, model=manual.model)
    alternate_route, alternate_id = create(
        bob, "Bob alternate model", alternate, model=alternate.model, is_default=False,
    )
    require(len({alice_id, bob_id, alternate_id}) == 3, "connection IDs were reused")
    check_db_redacted(db_path, private_values)
    for user, expected_ids in ((alice, {alice_id}), (bob, {bob_id, alternate_id})):
        require({entry["id"] for entry in connections(user)} == expected_ids,
                "connection lists were not isolated by owner")
    for user, foreign in ((bob, alice_route), (alice, bob_route), (alice, alternate_route)):
        call(user, "POST", f"{foreign}/models", body={}, expect=(404,),
             label="foreign model listing is hidden")
        call(user, "PATCH", foreign, body={"model": "foreign-change", "is_default": True},
             expect=(404,), label="foreign connection cannot be updated")
        call(user, "DELETE", foreign, expect=(404,), label="foreign connection cannot be deleted")
    require(all(f.requests == 0 for f in fixtures),
             "saving settings or a foreign request contacted a model")

    models = call(alice, "POST", f"{alice_route}/models", body={}, read_json=True)
    require(models == {"models": [keyed.model]} and keyed.list_calls == 1,
            "authenticated listing did not return the fixture model ID")
    call(bob, "POST", f"{bob_route}/models", body={}, expect=(502,),
         label="upstream model listing unavailable")
    require(manual.list_calls == 1, "manual endpoint did not receive its keyless listing request")
    call(alice, "PATCH", alice_route, body={"model": keyed.model, "is_default": True})
    def saved_choices(user):
        return {item["id"]: (item["model"], item["is_default"]) for item in connections(user)}

    bob_choices = {bob_id: (manual.model, True), alternate_id: (alternate.model, False)}
    require(saved_choices(alice) == {alice_id: (keyed.model, True)}
            and saved_choices(bob) == bob_choices, "model and default choices did not persist")

    call(alice, "POST", f"{CHAT}/messages", body={"text": keyed.prompt, "invoke_agent": True},
         label="Alice uses her default connection in Chat")
    wait_for_answer(call, alice, keyed, children, deadline)
    call(bob, "POST", f"{CHAT}/messages", body={
        "text": manual.prompt, "invoke_agent": True,
    }, label="Bob overrides the server fallback with his keyless default in Chat")
    wait_for_answer(call, bob, manual, children, deadline)
    require(alternate.requests == 0, "Bob's ordinary Chat unexpectedly used his non-default")

    default_requests = manual.requests
    call(bob, "POST", f"{CHAT}/messages", body={
        "text": alternate.prompt, "invoke_agent": True, "provider_connection_id": alternate_id,
    }, label="Bob explicitly selects his non-default connection for one Chat turn")
    wait_for_answer(call, bob, alternate, children, deadline)
    require(manual.requests == default_requests, "Bob's override also contacted his default endpoint")
    require(saved_choices(bob) == bob_choices, "Bob's per-turn override changed his saved default")

    for user, own, other in ((alice, keyed, manual), (bob, manual, keyed)):
        require(not any(other.prompt in event["text"] or other.answer in event["text"]
                        for event in read_events(call, user, CHAT)),
                "personal Chat exposed another user's transcript")
        require(own.chat_calls == 2, "unexpected model selection or extra completion")
    require(not any(alternate.prompt in event["text"] or alternate.answer in event["text"]
                    for event in read_events(call, alice, CHAT)),
            "Alice's Chat exposed Bob's alternate connection transcript")

    for user, connection in ((alice, alice_route), (bob, bob_route), (bob, alternate_route)):
        call(user, "DELETE", connection, expect=(204,))
        call(user, "POST", f"{connection}/models", body={}, expect=(404,),
             label="deleted connection can no longer list models")
    require(not connections(alice) and not connections(bob),
            "deleted connections remained in their owners' lists")
    check_db_redacted(db_path, private_values)


def worker_post(port, deadline, runner_token, path=CLAIM, *, protocol="2", body=None):
    headers = {"Authorization": f"Bearer {runner_token}", "Content-Type": "application/json"}
    if protocol is not None:
        headers[WORKER_PROTOCOL] = protocol
    connection = http.client.HTTPConnection(HOST, port, timeout=remaining(deadline))
    try:
        connection.request("POST", path, json.dumps(body or {}), headers)
        with connection.getresponse() as response:
            if response.status == 200 and path == CLAIM:
                payload = response.read(MAX_BODY + 1)
                require(len(payload) <= MAX_BODY, "oversized worker claim")
                try:
                    return response.status, json.loads(payload)
                except ValueError:
                    raise RuntimeError("invalid worker claim JSON") from None
            return response.status, None
    finally:
        connection.close()


def exercise_runners(stack, call, alice, bob, runner, state, env, port, children, fixture, deadline):
    alice_id = create_space(call, alice, "Alice settings Space")
    bob_id = create_space(call, bob, "Bob settings Space")
    alice_space, bob_space = (route(SPACES, item_id) for item_id in (alice_id, bob_id))

    def create(user, visibility, name):
        data = call(user, "POST", RUNNERS, body={"name": name, "visibility": visibility},
                    expect=(201,), read_json=True)
        entry, credential = data["runner"], data["token"]
        require(entry["name"] == name and entry["visibility"] == visibility
                and entry["can_manage"] is True, "invalid runner creation response")
        require(isinstance(credential, str) and credential.isascii() and credential.isprintable()
                and credential and not any(char.isspace() for char in credential),
                "invalid issued runner credential")
        route(RUNNERS, entry["id"])
        return entry["id"], credential

    call(bob, "POST", RUNNERS, body={"name": "Forbidden", "visibility": "organization"},
         expect=(403,), label="Bob cannot create an organization runner")
    org_id, org_token = create(alice, "organization", "Shared runner")
    personal_id, personal_token = create(alice, "personal", "Alice personal runner")
    require(org_id != personal_id and org_token != personal_token, "runner credentials were reused")
    for user, expected in (
        (alice, {org_id: ("organization", True), personal_id: ("personal", True)}),
        (bob, {org_id: ("organization", False)}),
    ):
        listed = public_list(call, user, RUNNERS, "runners", (org_token, personal_token))
        require({row["id"]: (row["visibility"], row["can_manage"]) for row in listed} == expected,
                "runner visibility or management permissions were incorrect")
    call(bob, "PUT", f"{bob_space}/runner", body={"runner_id": personal_id}, expect=(404,),
         label="Bob cannot attach Alice's personal runner")
    call(bob, "PUT", f"{alice_space}/runner", body={"runner_id": org_id}, expect=(404,),
         label="Bob cannot manage a Space he cannot see")

    def space_runner(owner, space):
        return call(owner, "GET", f"{space}/runner", read_json=True)["runner"]

    dedicated = call(alice, "POST", f"{alice_space}/runner-token", body={}, read_json=True)["token"]
    require(isinstance(dedicated, str) and dedicated, "missing dedicated credential")
    view = space_runner(alice, alice_space)
    require(view["id"] is None and view["kind"] == view["visibility"] == "dedicated",
            "dedicated runner was shown as a registered runner")
    call(alice, "PUT", f"{alice_space}/runner", body={"runner_id": org_id}, expect=(409,),
         label="attaching a registered runner cannot replace a dedicated one implicitly")
    require(worker_post(port, deadline, dedicated, protocol=None)[0] == 204,
            "rejected attachment revoked the dedicated worker")
    call(alice, "DELETE", f"{alice_space}/runner", expect=(204,))
    require(worker_post(port, deadline, dedicated, protocol=None)[0] == 401,
            "explicit detachment did not revoke the dedicated credential")

    def attach(owner, space):
        attached = call(owner, "PUT", f"{space}/runner", body={"runner_id": org_id},
                        read_json=True)["runner"]
        require(attached == {
            "id": org_id, "kind": "registered", "visibility": "organization", "name": "Shared runner",
        } and attached == space_runner(owner, space),
                "registered runner attachment was not reflected in the Space")

    for owner, space in ((alice, alice_space), (bob, bob_space)):
        attach(owner, space)
    require(worker_post(port, deadline, org_token)[0] == 204,
            "protocol 2 could not claim with the shared runner credential")
    for protocol in (None, "1"):
        require(worker_post(port, deadline, org_token, protocol=protocol)[0] == 426,
                "a legacy worker could use the shared runner credential")

    workspace = state / "runner-workspace"
    demos = []
    for owner, outsider, space, space_id, label in (
        (alice, bob, alice_space, alice_id, "Alice's Space"),
        (bob, alice, bob_space, bob_id, "Bob's Space"),
    ):
        room = f"{space}/rooms/lobby"
        marker = f"{uuid.uuid4().hex}-space-only.txt"
        directory = workspace / "spaces" / space_id
        directory.mkdir(parents=True)
        (directory / marker).touch()
        demos.append((owner, room, label, marker))
        call(outsider, "GET", f"{room}/events?after=0", expect=(404,),
             label="the other owner cannot see this Space's transcript")
        call(outsider, "POST", f"{room}/demo", body={}, expect=(404,),
             label="the other owner cannot start a Demo in this Space")

    runner_env = env | {
        "METRO_RUNNER_TOKEN": org_token,
        "METRO_SERVER_URL": f"http://{HOST}:{port}",
        "METRO_WORKSPACE": str(workspace),
    }

    def start_runner():
        children.append(start_child(stack, "shared organization runner", runner, state, runner_env))

    start_runner()
    chat_calls = fixture.chat_calls
    exercise_demos(call, demos, children, fixture, deadline)
    require(fixture.chat_calls == chat_calls, "a deterministic Demo called the model")

    # Pause the real runner to inspect one in-flight claim, then resume normal work.
    child, _ = children.pop()
    stop_child(child)
    call(alice, "POST", f"{demos[0][1]}/demo", body={}, label="queue work before detaching")
    require(worker_post(port, deadline, personal_token)[0] == 204,
            "an unattached runner was allowed to claim Space work")
    while True:
        status, job = worker_post(port, deadline, org_token)
        if status == 200:
            require(job["space_id"] == alice_id, "the claim did not name Alice's Space")
            break
        require(status == 204, "shared runner failed to claim queued work")
        time.sleep(min(0.05, remaining(deadline)))
    result_path = f"{route('/v1/runner/jobs', job['id'])}/result"
    result = {"ok": True, "output": "must not be accepted"}
    require(worker_post(port, deadline, personal_token, result_path, body=result)[0] == 404,
            "another runner could complete Alice's job")
    call(alice, "DELETE", f"{alice_space}/runner", expect=(204,))
    require(worker_post(port, deadline, org_token, result_path, body=result)[0] == 404,
            "the shared runner could complete a detached claim")
    call(alice, "POST", f"{alice_space}/rooms/lobby/demo", body={}, expect=(409,),
         label="detached Space no longer dispatches runner jobs")
    start_runner()
    exercise_demos(call, [demos[1]], children, fixture, deadline)
    child, _ = children.pop()
    stop_child(child)
    attach(alice, alice_space)
    call(bob, "DELETE", route(RUNNERS, org_id), expect=(403,))
    call(alice, "DELETE", route(RUNNERS, org_id), expect=(204,))
    require(space_runner(alice, alice_space) is None and space_runner(bob, bob_space) is None,
            "deleting the organization runner did not detach both Spaces")
    require(worker_post(port, deadline, org_token)[0] == 401,
            "deleted runner credential still authenticated")
    check_db_redacted(state / "settings.sqlite", (org_token, personal_token, dedicated))


def settings(server, runner, timeout):
    alice, bob, provider_key = (token() for _ in range(3))
    with temp_state() as directory, ExitStack() as stack:
        state = Path(directory)
        db_path = state / "settings.sqlite"
        env = clean_env(directory)
        deadline = time.monotonic() + timeout
        keyed = serve_fixture(stack, ModelFixture("fixture/alice", provider_key))
        manual = serve_fixture(stack, ModelFixture("fixture/bob:local", can_list=False))
        alternate = serve_fixture(stack, ModelFixture("fixture/bob-alternate"))
        fallback = serve_fixture(stack, ModelFixture("fixture/fallback"))
        port = server_port()
        children = [start_child(stack, "settings server", server, state, env | {
            "METRO_BIND": f"{HOST}:{port}",
            "METRO_DB": str(db_path),
            "METRO_DEV_USERS": json.dumps({"alice": alice, "bob": bob}),
            "METRO_ORG_ADMINS": json.dumps(["alice"]),
            "METRO_MODEL": fallback.model,
            "METRO_MODEL_BASE_URL": fallback.base_url,
            "METRO_PROVIDER_MASTER_KEY": base64.b64encode(secrets.token_bytes(32)).decode("ascii"),
            "METRO_PROVIDER_ALLOWED_BASE_URLS": json.dumps([
                keyed.base_url, manual.base_url, alternate.base_url,
            ]),
        })]
        wait_for_server(port, children, keyed, deadline)
        call = partial(request, port, deadline)
        exercise_providers(call, alice, bob, keyed, manual, alternate, db_path, children, deadline)
        exercise_runners(stack, call, alice, bob, runner, state, env, port, children, keyed, deadline)
        check_processes(children, keyed)
        check_processes(children, manual)
        check_processes(children, alternate)
        require((keyed.list_calls, keyed.chat_calls, manual.list_calls, manual.chat_calls,
                 alternate.list_calls, alternate.chat_calls) == (1, 2, 1, 2, 0, 2)
                and keyed.requests == manual.requests == 3 and alternate.requests == 2
                and fallback.requests == 0,
                "unexpected fixture traffic")
    print("PASS: private provider settings, live listing, default and per-turn Chat selection, "
          "organization runner protocol and Space workspace isolation.")


if __name__ == "__main__":
    signal.signal(signal.SIGTERM, terminate)
    raise SystemExit(main(settings, description=__doc__))
