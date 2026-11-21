#!/usr/bin/env python3
"""Local Metro smoke test. Run with python3 tests/e2e/smoke.py after building both binaries."""

import argparse
from contextlib import ExitStack
from functools import partial
import http.client
from http.server import BaseHTTPRequestHandler, HTTPServer
import json
import math
import os
from pathlib import Path
import re
import secrets
import signal
import socket
import subprocess
import sys
import tempfile
import threading
import time
from urllib.parse import quote
import uuid


HOST = "127.0.0.1"
SPACES = "/v1/spaces"
CLAIM = "/v1/runner/claim"
MODEL = "metro-smoke-model"
MODEL_KEY = "local-smoke-not-a-real-key"
MAX_BODY = 1024 * 1024
ROOT = Path(__file__).resolve().parents[2]


def require(condition, message):
    if not condition:
        raise RuntimeError(message)


def remaining(deadline):
    seconds = deadline - time.monotonic()
    if seconds <= 0:
        raise TimeoutError("smoke test deadline expired")
    return seconds


def content_parts(value):
    """Text from a Chat Completions message with string or text-part content."""
    if isinstance(value, str):
        yield value
    elif isinstance(value, list):
        for part in value:
            if isinstance(part, dict) and part.get("type") == "text":
                yield part["text"]


def listing_call(tools):
    paths = {"path", "relative_path", "directory", "dir", "subdir", "folder", "root"}
    file_words = {"file", "files", "directory", "directories", "dir", "folder", "folders"}
    exact, fallback = [], []
    for tool in tools:
        if tool.get("type") != "function":
            continue
        function = tool["function"]
        # Match the workspace.list alias across separators but keep the wire name.
        if re.fullmatch(r"workspace[_.-]list", function["name"], re.IGNORECASE):
            exact.append(function)
            continue
        name_words = set(re.findall(r"[a-z]+", function["name"].lower()))
        description_words = set(re.findall(r"[a-z]+", function.get("description", "").lower()))
        words = name_words | description_words
        if "mcp" in words or "tools" in name_words:
            continue
        properties = function.get("parameters", {}).get("properties", {})
        if words & {"list", "lists", "listing", "enumerate", "enumerates"} and (
            words & file_words or ("workspace" in words and paths.intersection(properties))
        ):
            fallback.append(function)
    candidates = exact or fallback
    require(
        len(candidates) == 1,
        f"expected one workspace listing tool, got {len(candidates)}",
    )
    function = candidates[0]
    schema = function.get("parameters", {})
    properties = schema.get("properties", {})
    arguments = {}
    for name in schema.get("required", []):
        prop = properties[name]
        if name in paths:
            arguments[name] = "."
        elif "default" in prop:
            arguments[name] = prop["default"]
        elif name == "recursive":
            arguments[name] = False
        else:
            raise RuntimeError("cannot supply an advertised required listing argument")
    # An optional path need not have a documented default; explicitly request the root.
    for name, prop in properties.items():
        if name in paths and name not in arguments and prop.get("type") == "string":
            arguments[name] = "."
            break
    return {"name": function["name"], "arguments": json.dumps(arguments)}


class OpenAIModel(HTTPServer):
    def __init__(self):
        super().__init__((HOST, 0), ModelHandler)
        self.stage = 0
        self.requests = 0
        self.error = None

    def get_request(self):
        connection, address = super().get_request()
        connection.settimeout(2)
        return connection, address

    def complete(self, request):
        require(self.error is None, self.error)
        require(request.get("model") == MODEL, "unexpected model name")
        require(not request.get("stream"), "expected a non-streaming chat completion")
        message = self.respond(request)
        self.stage += 1
        return {
            "id": f"chatcmpl-smoke-{self.stage}",
            "object": "chat.completion",
            "created": int(time.time()),
            "model": MODEL,
            "choices": [{"index": 0, "message": message,
                         "finish_reason": "tool_calls" if "tool_calls" in message else "stop"}],
            "usage": {"prompt_tokens": 1, "completion_tokens": 1, "total_tokens": 2},
        }


def tool_reply(call_id, function):
    return {
        "role": "assistant",
        "content": None,
        "tool_calls": [{"id": call_id, "type": "function", "function": function}],
    }


def text_reply(text):
    return {"role": "assistant", "content": text}


def addressed_turns(message):
    # Rig can batch consecutive user turns into one provider user message.
    content = message.get("content")
    parts = content.splitlines() if isinstance(content, str) else list(content_parts(content))
    require(len(parts) % 2 == 0, "model user message had unpaired context or text")
    yield from zip(parts[::2], parts[1::2])


def addressed_to(turn, text, audience, mentions):
    # Metro puts its JSON annotation before the separate, untrusted user text.
    prefix = "metro-context: "
    context, body = turn
    if not context.startswith(prefix) or body != text:
        return False
    try:
        data = json.loads(context[len(prefix):])
    except ValueError:
        return False
    return (isinstance(data, dict) and data.get("kind") == "message"
            and data.get("speaker_id") == "alice" and data.get("speaker") == "alice"
            and data.get("audience") == audience and data.get("mentions") == mentions)


class ScriptedModel(OpenAIModel):
    def __init__(self, prompt, human_message, filename, other_filename, final_text):
        super().__init__()
        self.prompt = prompt
        self.human_message = human_message
        self.filename = filename
        self.other_filename = other_filename
        self.final_text = final_text
        self.call_id = f"call_{uuid.uuid4().hex}"

    def respond(self, request):
        messages = request["messages"]
        if self.stage == 0:
            turns = [turn for message in messages if message["role"] == "user"
                     for turn in addressed_turns(message)]
            require(
                turns and addressed_to(turns[-1], self.prompt, "agent", [{"kind": "agent"}]),
                "the model's latest message was not Alice's agent-directed turn with recipients",
            )
            require(
                len(turns) >= 2 and addressed_to(
                    turns[-2], self.human_message, "users", [{"kind": "user", "id": "bob"}]
                ),
                "model history did not label Alice's earlier human-only turn as addressed to Bob",
            )
            return tool_reply(self.call_id, listing_call(request["tools"]))
        elif self.stage == 1:
            results = [
                text for msg in messages
                if msg["role"] == "tool" and msg.get("tool_call_id") == self.call_id
                for text in content_parts(msg.get("content"))
            ]
            require(
                any(self.filename in text for text in results),
                "workspace listing tool result was missing Alice's runner-only file",
            )
            require(
                all(self.other_filename not in text for text in results),
                "workspace listing tool result leaked Bob's runner-only file",
            )
            return text_reply(self.final_text)
        raise RuntimeError("unexpected additional model request")


class ModelHandler(BaseHTTPRequestHandler):
    def log_message(self, *_):
        pass

    def do_POST(self):
        self.server.requests += 1
        try:
            require(self.path == "/v1/chat/completions", "unexpected model POST path")
            size = int(self.headers["Content-Length"])
            require(0 < size <= MAX_BODY, f"invalid model request size: {size}")
            body = self.server.complete(json.loads(self.rfile.read(size)))
            status = 200
        except Exception as exc:
            # Never echo model request fields or unexpected exception values.
            detail = str(exc) if type(exc) is RuntimeError else "invalid completion request"
            if self.server.error is None:
                self.server.error = f"model double: {detail}"
            body, status = {"error": {"message": detail}}, 400
        data = json.dumps(body).encode()
        self.send_response(status)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(data)))
        self.end_headers()
        self.wfile.write(data)


def server_port():
    # Metro takes a numeric port, so release this ephemeral port just before launch.
    with socket.socket() as sock:
        sock.bind((HOST, 0))
        return sock.getsockname()[1]


def stop_child(child):
    if child.poll() is None:
        child.terminate()
        try:
            child.wait(timeout=2)
        except subprocess.TimeoutExpired:
            child.kill()
            child.wait(timeout=2)


def start_child(stack, label, binary, cwd, env):
    # Child diagnostics can contain bearer tokens (including unknown tokens on
    # a failing issuance response). Do not retain or reprint those streams.
    child = subprocess.Popen(
        [str(binary)], cwd=cwd, env=env, stdin=subprocess.DEVNULL,
        stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL,
    )
    stack.callback(stop_child, child)
    return child, label


def check_processes(children, model):
    require(model.error is None, model.error)
    for child, label in children:
        code = child.poll()
        require(code is None, f"{label} exited with status {code}")


def wait_for_server(port, children, model, deadline):
    while True:
        check_processes(children, model)
        try:
            with socket.create_connection((HOST, port), timeout=min(0.3, remaining(deadline))):
                return
        except OSError:
            time.sleep(min(0.05, remaining(deadline)))


def start_server(stack, server, state, env, users, model, deadline):
    stack.enter_context(model)
    thread = threading.Thread(target=model.serve_forever, kwargs={"poll_interval": 0.05})
    thread.start()
    stack.callback(thread.join, 3)
    stack.callback(model.shutdown)
    port = server_port()
    children = [start_child(stack, "server", server, state, env | {
        "METRO_BIND": f"{HOST}:{port}",
        "METRO_DB": str(state / "metro.sqlite"),
        "METRO_DEV_USERS": json.dumps(users),
        "METRO_MODEL_BASE_URL": f"http://{HOST}:{model.server_port}/v1",
        "METRO_MODEL": MODEL,
        "METRO_MODEL_API_KEY": MODEL_KEY,
    })]
    wait_for_server(port, children, model, deadline)
    return port, children


def request(port, deadline, token, method, path, *, body=None, expect=None, read_json=False,
            label=None):
    action = label or f"{method} {path}"
    connection = http.client.HTTPConnection(HOST, port, timeout=remaining(deadline))
    try:
        connection.connect()
        sock = connection.sock
        headers = {"Authorization": f"Bearer {token}", "Accept": "application/json"}
        if body is not None:
            headers["Content-Type"] = "application/json"
            body = json.dumps(body)
        sock.settimeout(remaining(deadline))
        connection.request(method, path, body, headers)
        sock.settimeout(remaining(deadline))
        with connection.getresponse() as response:
            ok = 200 <= response.status < 300 if expect is None else response.status in expect
            require(ok, f"{action}: unexpected HTTP {response.status}")
            if not read_json:
                return
            data = bytearray()
            while True:
                sock.settimeout(remaining(deadline))
                part = response.read1(65536)
                if not part:
                    try:
                        return json.loads(data)
                    except ValueError:
                        raise RuntimeError(f"{action}: invalid JSON response") from None
                data.extend(part)
                require(len(data) <= MAX_BODY, f"{action}: oversized response")
    except TimeoutError:
        raise TimeoutError(f"{action}: timed out") from None
    except http.client.HTTPException:
        raise RuntimeError(f"{action}: invalid HTTP response") from None
    finally:
        connection.close()


def read_events(call, user, room, after=0):
    data = call(user, "GET", f"{room}/events?after={after}", read_json=True,
                label="GET transcript events")
    require(isinstance(data, dict) and isinstance(data.get("events"), list),
            "expected {events: [...]} from GET transcript events")
    return data["events"]


def alice_turn(event, text, audience, mentions):
    if (event["kind"], event["actor"], event["text"]) != ("message", "alice", text):
        return False
    require(event.get("audience") == audience and event.get("mentions") == mentions,
            f"Alice's {audience} turn had incorrect audience or mentions metadata")
    return True


def exercise_rig(call, room, alice, bob, children, model, deadline):
    user_mentions = [{"kind": "user", "id": "bob"}]
    call(alice, "POST", f"{room}/messages",
         body={"text": model.human_message, "invoke_agent": False, "mentions": user_mentions},
         label="Alice directs a human-only message to Bob")
    quiet_until = time.monotonic() + 0.35
    bob_saw_human_turn = False
    while True:
        check_processes(children, model)
        require(model.requests == 0, "Alice's Bob-only mention started an agent call")
        events = read_events(call, bob, room)
        bob_saw_human_turn |= any(alice_turn(event, model.human_message, "users", user_mentions)
                                  for event in events)
        require(not any(event["kind"] in {"agent", "tool_call", "tool_result", "error"}
                        for event in events), "a Bob-only mention started agent work")
        window_left = quiet_until - time.monotonic()
        if bob_saw_human_turn and window_left <= 0:
            break
        time.sleep(min(0.05, window_left if window_left > 0 else 0.05, remaining(deadline)))

    agent_mentions = [{"kind": "agent"}]
    call(alice, "POST", f"{room}/messages",
         body={"text": model.prompt, "invoke_agent": False},
         label="Alice invokes the agent via @agent")
    missing = {"Alice's post", "the final assistant reply"}
    try:
        while missing:
            check_processes(children, model)
            for event in read_events(call, bob, room):
                if alice_turn(event, model.prompt, "agent", agent_mentions):
                    missing.discard("Alice's post")
                if (event["kind"], event["actor"], event["text"]) == (
                    "agent", "agent", model.final_text
                ):
                    missing.discard("the final assistant reply")
                if event["kind"] == "error":
                    raise RuntimeError("Bob received a room error during Alice's agent invocation")
            if missing:
                time.sleep(min(0.1, remaining(deadline)))
    except TimeoutError as exc:
        raise RuntimeError(
            f"Bob did not see {' and '.join(sorted(missing))} before the deadline "
            f"(model requests completed: {model.stage}/2)"
        ) from exc


def create_space(call, user, name):
    data = call(user, "POST", SPACES, body={"name": name}, read_json=True)
    require(isinstance(data, dict)
            and isinstance(data.get("id"), str) and data["id"]
            and data.get("name") == name and data.get("role") == "owner"
            and data.get("runner_paired") is False, f"invalid create response for {name}")
    return data["id"]


def visible_spaces(call, user):
    data = call(user, "GET", SPACES, read_json=True)
    require(isinstance(data, dict) and isinstance(data.get("spaces"), list),
            "expected {spaces: [...]} from GET /v1/spaces")
    return {space["id"] for space in data["spaces"]}


def issue_token(call, user, route, label):
    data = call(user, "POST", route, body={}, read_json=True, label=label)
    token = data.get("token") if isinstance(data, dict) else None
    require(isinstance(token, str) and token.isascii() and token.isprintable()
            and token and not any(char.isspace() for char in token),
            f"{label}: invalid runner token response")
    return token


def exercise_demos(call, demos, children, model, deadline):
    # Queue both listings while both runners are live; their room names coincide.
    pending = []
    for user, room, label, marker in demos:
        event = call(user, "POST", f"{room}/demo", body={}, read_json=True,
                     label=f"start {label} Demo")
        require(isinstance(event, dict) and type(event.get("id")) is int and event["id"] > 0,
                f"{label} Demo did not return an event ID")
        pending.append((user, room, label, marker, event["id"]))

    while pending:
        try:
            check_processes(children, model)
            for demo in pending[:]:
                user, room, label, marker, after = demo
                for event in read_events(call, user, room, after):
                    kind = event["kind"]
                    require(kind != "error", f"{label} Demo emitted a room error")
                    if kind != "tool_result":
                        continue
                    header, _, output = event["text"].partition("\n")
                    require(event["actor"] == "runner" and header == "workspace.list ok",
                            f"{label} Demo did not get a successful runner listing")
                    try:
                        listing = json.loads(output)
                    except ValueError:
                        raise RuntimeError(f"{label} Demo listing was not JSON") from None
                    require(isinstance(listing, dict) and listing.get("truncated") is False
                            and isinstance(listing.get("entries"), list)
                            and [entry["name"] for entry in listing["entries"]] == [marker],
                            f"{label} Demo did not list exactly its own workspace marker")
                    pending.remove(demo)
                    break
            if pending:
                time.sleep(min(0.1, remaining(deadline)))
        except TimeoutError as exc:
            labels = ", ".join(demo[2] for demo in pending)
            raise RuntimeError(f"timed out waiting for Demo results: {labels}") from exc


def temp_state():
    preferred_tmp = Path("/tmp/opencode")
    temp_root = preferred_tmp if os.access(preferred_tmp, os.W_OK | os.X_OK) else None
    return tempfile.TemporaryDirectory(prefix="metro-smoke-", dir=temp_root)


def clean_env(directory):
    # Do not inherit proxy, OIDC, MCP, model, or user credentials/config.
    return {"PATH": os.defpath, "HOME": directory}


def smoke(server, runner, timeout):
    prompt = f"@agent List the workspace files. Smoke request {uuid.uuid4().hex}"
    human_message = f"@bob Human-only smoke message {uuid.uuid4().hex}"
    alice_marker = f"alice-runner-only-{uuid.uuid4().hex}.txt"
    bob_marker = f"bob-runner-only-{uuid.uuid4().hex}.txt"
    final_text = f"Smoke assistant reply {uuid.uuid4().hex}"
    alice, bob = (secrets.token_hex(24) for _ in range(2))
    with temp_state() as directory:
        with ExitStack() as stack:
            state = Path(directory)
            alice_workspace = state / "alice-workspace"
            bob_workspace = state / "bob-workspace"
            for workspace, marker in ((alice_workspace, alice_marker), (bob_workspace, bob_marker)):
                workspace.mkdir()
                (workspace / marker).touch()
            model = ScriptedModel(prompt, human_message, alice_marker, bob_marker, final_text)
            env = clean_env(directory)
            deadline = time.monotonic() + timeout
            port, children = start_server(stack, server, state, env, {"alice": alice, "bob": bob},
                                          model, deadline)
            call = partial(request, port, deadline)

            alice_id = create_space(call, alice, "Alice smoke Space")
            bob_id = create_space(call, bob, "Bob private Space")
            require(alice_id != bob_id, "both Spaces were assigned the same ID")
            require(visible_spaces(call, alice) == {alice_id}, "Alice could see Bob's private Space")
            require(visible_spaces(call, bob) == {bob_id}, "Bob could see Alice's Space before joining")
            alice_space, bob_space = (f"{SPACES}/{quote(space, safe='')}" for space in (alice_id, bob_id))
            alice_room, bob_room = (f"{space}/rooms/lobby" for space in (alice_space, bob_space))
            call(alice, "POST", f"{alice_space}/members", body={"user_id": "bob"},
                 label="Alice adds Bob as a member")
            require(visible_spaces(call, bob) == {alice_id, bob_id}, "Bob could not see both Spaces")
            require(visible_spaces(call, alice) == {alice_id},
                    "Alice could see Bob's private Space after adding him")

            alice_route, bob_route = (f"{space}/runner-token" for space in (alice_space, bob_space))
            old_token = issue_token(call, alice, alice_route, "issue Alice's initial runner token")
            bob_token = issue_token(call, bob, bob_route, "issue Bob's runner token")
            call(bob, "POST", alice_route, body={}, expect=(403,),
                 label="member Bob attempts to rotate Alice's runner token")
            call(old_token, "POST", CLAIM, body={}, expect=(204,),
                 label="Alice's original runner token can claim before rotation")
            alice_token = issue_token(call, alice, alice_route, "rotate Alice's runner token")
            require(len({old_token, alice_token, bob_token}) == 3, "runner credentials were reused")
            call(old_token, "POST", CLAIM, body={}, expect=(401,),
                 label="Alice's revoked runner token attempts to claim")

            for label, token, workspace in (
                ("Alice's runner", alice_token, alice_workspace),
                ("Bob's runner", bob_token, bob_workspace),
            ):
                children.append(start_child(stack, label, runner, state, env | {
                    "METRO_RUNNER_TOKEN": token,
                    "METRO_SERVER_URL": f"http://{HOST}:{port}",
                    "METRO_WORKSPACE": str(workspace),
                }))
            call(alice, "GET", f"{bob_room}/events?after=0", expect=(404,),
                 label="non-member Alice attempts to read Bob's lobby")
            call(alice, "POST", f"{bob_room}/messages", expect=(404,),
                 body={"text": "non-member write probe", "invoke_agent": False},
                 label="non-member Alice attempts to post to Bob's lobby")
            call(alice, "POST", f"{bob_room}/demo", body={}, expect=(404,),
                 label="non-member Alice attempts to run Bob's Demo")

            exercise_rig(call, alice_room, alice, bob, children, model, deadline)
            exercise_demos(call, (
                (bob, alice_room, "Alice's Space", alice_marker),
                (bob, bob_room, "Bob's Space", bob_marker),
            ), children, model, deadline)
            check_processes(children, model)
            require(model.requests == model.stage == 2, "expected exactly two Rig model requests")
    print("PASS: Space access and token rotation enforced; each runner listed only its own marker; "
          "Bob-only mention stayed human; @agent invoked Rig and Bob saw its reply.")


def main(run=smoke, binaries=("metro-server", "metro-runner"), description=__doc__):
    parser = argparse.ArgumentParser(description=description)
    parser.add_argument("--bin-dir", type=Path, default=ROOT / "target" / "debug")
    parser.add_argument("--timeout", type=float, default=30, help="overall deadline in seconds")
    args = parser.parse_args()
    if not math.isfinite(args.timeout) or args.timeout <= 0:
        parser.error("--timeout must be finite and positive")
    paths = [args.bin_dir.resolve() / name for name in binaries]
    for binary in paths:
        if not binary.is_file() or not os.access(binary, os.X_OK):
            parser.error(f"missing executable {binary}; build the required binary or set --bin-dir")
    try:
        run(*paths, args.timeout)
    except (KeyError, TypeError, AttributeError):
        print("FAIL: unexpected response shape", file=sys.stderr)
        return 1
    except (RuntimeError, OSError, ValueError, http.client.HTTPException) as exc:
        print(f"FAIL: {exc}", file=sys.stderr)
        return 1
    except KeyboardInterrupt:
        return 130
    return 0


def terminate(signum, _frame):
    # Raise so the ExitStack closes our subprocesses, sockets and temporary state.
    raise SystemExit(128 + signum)


if __name__ == "__main__":
    signal.signal(signal.SIGTERM, terminate)
    raise SystemExit(main())
