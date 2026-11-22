#!/usr/bin/env python3
"""Test personal on-device BYOK with metro-server, metro-connector and a loopback vendor."""

from concurrent.futures import ThreadPoolExecutor
from contextlib import ExitStack
from functools import partial
import hmac
import http.client
from http.server import BaseHTTPRequestHandler, HTTPServer
import json
import os
from pathlib import Path
import secrets
import signal
import stat
import subprocess
import threading
import time
from urllib.parse import quote
import uuid

from smoke import (
    HOST, MAX_BODY, check_processes, clean_env, content_parts, main, read_events, remaining,
    request, require, server_port, stop_child, temp_state, terminate, text_reply, tool_reply,
    wait_for_server,
)

KIND = "device-openai-compatible"
PROVIDERS = "/v1/providers"
CLAIM = "/v1/device/claim"
CHAT = "/v1/chat"


class VendorHandler(BaseHTTPRequestHandler):
    def log_message(self, *_):
        pass

    def do_GET(self):
        self.dispatch("GET")

    def do_POST(self):
        self.dispatch("POST")

    def dispatch(self, method):
        vendor = self.server
        try:
            credential = (self.headers.get("Authorization") or "").encode()
            require(hmac.compare_digest(credential, vendor.authorization),
                    "vendor request did not use the companion's local credential")
            if method == "GET":
                require(self.path == "/v1/models", "unexpected vendor listing route")
                vendor.list_calls += 1
                payload = {"object": "list", "data": [{"id": vendor.model, "object": "model"}]}
            else:
                require(self.path == "/v1/chat/completions", "unexpected vendor completion route")
                size = int(self.headers.get("Content-Length", "0"))
                require(0 < size <= MAX_BODY, "invalid vendor completion request size")
                payload = vendor.complete(json.loads(self.rfile.read(size)))
            status = 200
        except Exception as exc:
            # Assertion messages are fixed; never print a request or exception payload.
            if vendor.error is None:
                vendor.error = str(exc) if type(exc) is RuntimeError else "invalid vendor request"
            status, payload = 400, {"error": {"message": "vendor fixture rejected request"}}
        data = json.dumps(payload).encode()
        try:
            self.send_response(status)
            self.send_header("Content-Type", "application/json")
            self.send_header("Content-Length", str(len(data)))
            self.end_headers()
            self.wfile.write(data)
        except OSError:
            if vendor.error is None:
                vendor.error = "vendor response connection closed early"


class VendorFixture(HTTPServer):
    def __init__(self):
        super().__init__((HOST, 0), VendorHandler)
        self.base_url = f"http://{HOST}:{self.server_port}/v1"
        self.key = f"local-device-test-{secrets.token_hex(32)}"
        self.authorization = f"Bearer {self.key}".encode()
        self.model = f"fixture/{uuid.uuid4().hex}"
        self.prompt = f"Please acknowledge my test message {uuid.uuid4().hex}"
        self.answer = f"Device fixture reply {uuid.uuid4().hex}"
        self.tool_id = f"call_{uuid.uuid4().hex}"
        self.connections = 0
        self.list_calls = 0
        self.chat_calls = 0
        self.error = None

    def get_request(self):
        connection, address = super().get_request()
        self.connections += 1
        connection.settimeout(2)
        return connection, address

    def handle_error(self, _request, _address):
        # BaseServer's fallback would print a traceback to the test terminal.
        if self.error is None:
            self.error = "vendor HTTP handler failed"

    def complete(self, request):
        require(request["model"] == self.model and not request.get("stream"),
                "wrong model or mode in the relayed vendor completion")
        messages = request["messages"]
        user_messages = [msg for msg in messages if msg["role"] == "user"]
        require(user_messages and self.prompt in "\n".join(
            content_parts(user_messages[-1].get("content"))),
            "the vendor did not receive the intended personal Chat message")
        require(self.chat_calls < 2, "unexpected vendor completion after tool replay")
        if self.chat_calls == 0:
            names = {tool["function"]["name"] for tool in request.get("tools", [])
                     if tool.get("type") == "function"}
            require(names == {"react_to_message"}, "unexpected tools on the device Chat route")
            message = tool_reply(self.tool_id, {
                "name": "react_to_message", "arguments": json.dumps({"emoji": "👀"}),
            })
        else:
            replies = [msg for msg in messages if msg["role"] == "tool"
                       and msg.get("tool_call_id") == self.tool_id]
            require(len(replies) == 1 and any(content_parts(replies[0].get("content"))),
                    "the vendor did not receive Metro's reaction tool result")
            message = text_reply(self.answer)
        self.chat_calls += 1
        return {
            "id": f"chatcmpl-device-{self.chat_calls}",
            "object": "chat.completion",
            "created": int(time.time()),
            "model": self.model,
            "choices": [{"index": 0, "message": message,
                         "finish_reason": "tool_calls" if "tool_calls" in message else "stop"}],
            "usage": {"prompt_tokens": 1, "completion_tokens": 1, "total_tokens": 2},
        }


def start_vendor(stack):
    fixture = stack.enter_context(VendorFixture())
    thread = threading.Thread(target=fixture.serve_forever, kwargs={"poll_interval": 0.05})
    thread.start()
    stack.callback(thread.join, 3)
    stack.callback(fixture.shutdown)
    return fixture


def private_file(path, value):
    """Create a private input for the companion without putting it in argv or stdin."""
    with os.fdopen(os.open(path, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600), "w") as stream:
        os.fchmod(stream.fileno(), 0o600)
        stream.write(f"{value}\n")


def assert_db_has_no_vendor_key(db, key):
    require(db.is_file() and db.stat().st_size > 0, "Metro did not create a SQLite database")
    for path in (db, Path(f"{db}-wal"), Path(f"{db}-journal")):
        if path.is_file():
            require(key.encode() not in path.read_bytes(), "vendor credential found in Metro SQLite")


def start_server_with_log_guard(stack, binary, cwd, env, vendor_key):
    """Scan child output in memory, retain no log text, and never echo diagnostics."""
    child = subprocess.Popen(
        [str(binary)], cwd=cwd, env=env, stdin=subprocess.DEVNULL,
        stdout=subprocess.PIPE, stderr=subprocess.STDOUT,
    )
    seen = threading.Event()
    encoded = vendor_key.encode()

    def drain():
        tail = b""
        with child.stdout as output:
            while chunk := output.read1(8192):
                window = tail + chunk
                if encoded in window:
                    seen.set()
                tail = window[-(len(encoded) - 1):]

    reader = threading.Thread(target=drain)
    reader.start()

    def finish():
        stop_child(child)
        reader.join(3)
        require(not reader.is_alive(), "server output did not close during cleanup")
        require(not seen.is_set(), "vendor credential found in Metro server output")

    stack.callback(finish)
    return child, "device test server"


def start_connector(stack, binary, cwd, env, args):
    child = subprocess.Popen(
        [str(binary), *args], cwd=cwd, env=env, stdin=subprocess.DEVNULL,
        stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL,
    )
    stack.callback(stop_child, child)
    return child


def wait_exit(child, deadline, expected, label):
    try:
        status = child.wait(timeout=remaining(deadline))
    except subprocess.TimeoutExpired:
        raise TimeoutError(f"{label} did not exit before the deadline") from None
    require(status == expected, f"{label}: unexpected exit status {status}")


def create_connection(call, owner, name):
    connection = call(owner, "POST", PROVIDERS, body={"name": name, "provider_id": KIND},
                      expect=(201,), read_json=True)
    require(connection["provider_id"] == KIND and connection["execution"] == "device"
            and connection["has_key"] is False and "base_url" not in connection,
            "Metro did not create a keyless device connection")
    item_id = connection["id"]
    require(isinstance(item_id, str) and bool(item_id), "missing device connection ID")
    return item_id, f"{PROVIDERS}/{quote(item_id, safe='')}"


def issue_token(call, owner, status_path):
    credential = call(owner, "POST", status_path, body={}, expect=(201,), read_json=True)["token"]
    require(isinstance(credential, str) and credential.startswith("metro_pc_")
            and credential.isascii() and not any(char.isspace() for char in credential),
            "invalid device credential returned by Metro")
    return credential


def assert_hidden(data, values):
    encoded = json.dumps(data)
    require(not any(value in encoded for value in values),
            "a human API response contained private device settings")


def wait_online(call, user, status, children, vendor, deadline):
    while True:
        check_processes(children, vendor)
        state = call(user, "GET", status, read_json=True)
        require(state["paired"] is True, "issued device token was not paired")
        if state["online"] is True:
            require(type(state["last_seen_secs"]) is int and state["last_seen_secs"] >= 0,
                    "online device status had no valid last-seen time")
            return state
        time.sleep(min(0.05, remaining(deadline)))


def wait_chat(call, user, event_id, children, vendor, deadline):
    while True:
        check_processes(children, vendor)
        events = read_events(call, user, CHAT, event_id)
        require(not any(event["kind"] == "error" for event in events), "device Chat emitted an error")
        if any((event["kind"], event["actor"], event["text"]) ==
               ("agent", "agent", vendor.answer) for event in events):
            reactions = [json.loads(event["text"]) for event in events if event["kind"] == "reaction"]
            require(any(r.get("target_event_id") == event_id and r.get("emoji") == "👀"
                        and r.get("present") is True and r.get("reactor_kind") == "agent"
                        for r in reactions), "Chat did not apply the relayed reaction tool")
            return events
        time.sleep(min(0.05, remaining(deadline)))


def claim(port, deadline, credential):
    connection = http.client.HTTPConnection(HOST, port, timeout=remaining(deadline))
    try:
        connection.request("POST", CLAIM, "{}", {
            "Authorization": f"Bearer {credential}",
            "Content-Type": "application/json",
        })
        with connection.getresponse() as reply:
            require(reply.status in (200, 204), f"unexpected idle/claim HTTP {reply.status}")
            if reply.status == 204:
                return None
            data = reply.read(MAX_BODY + 1)
            require(len(data) <= MAX_BODY, "oversized device claim")
            try:
                return json.loads(data)
            except ValueError:
                raise RuntimeError("invalid device claim JSON") from None
    finally:
        connection.close()


def exercise_rotation(call, port, deadline, alice, models, status, old, foreign, server, vendor):
    # The real connector is deliberately paused. Hold a claimed listing request
    # open while testing result ownership and invalidation of its generation.
    require(claim(port, deadline, old) is None, "unexpected device work before rotation test")

    def listing_claim(pending):
        while True:
            check_processes([server], vendor)
            require(not pending.done(), "test listing ended before being claimed")
            job = claim(port, deadline, old)
            if job is not None:
                require(job["operation"] == "models" and job["request"] == {},
                        "wrong device listing claim")
                return f"/v1/device/jobs/{quote(job['id'], safe='')}/result"
            time.sleep(min(0.05, remaining(deadline)))

    with ThreadPoolExecutor(max_workers=1) as executor:
        pending = executor.submit(call, alice, "POST", models, body={}, read_json=True)
        result_path = listing_claim(pending)
        response = {"models": [vendor.model]}
        call(old, "POST", result_path, body={"ok": True, "response": response}, expect=(204,),
             label="current device result is accepted")
        call(old, "POST", result_path, body={"ok": True, "response": {"models": ["duplicate"]}},
             expect=(204,), label="duplicate delivery is acknowledged without replacing the result")
        require(pending.result(timeout=remaining(deadline)) == response,
                "a duplicate device result replaced the original")

        pending = executor.submit(
            call, alice, "POST", models, body={}, expect=(502,), label="rotated in-flight listing",
        )
        result_path = listing_claim(pending)
        require(claim(port, deadline, foreign) is None, "Bob's device claimed another user's job")
        result = {"ok": True, "response": {"models": ["foreign-result-must-not-win"]}}
        call(foreign, "POST", result_path, body=result, expect=(404,),
             label="Bob's valid device cannot complete Alice's job")
        new = issue_token(call, alice, status)
        require(new != old and new != foreign, "rotation reused a device credential")
        pending.result(timeout=remaining(deadline))
        call(old, "POST", CLAIM, body={}, expect=(401,), label="revoked device cannot claim")
        call(old, "POST", result_path, body=result, expect=(401,),
             label="revoked device cannot submit a result")
        # 410 is reserved for expiry in the caller's generation. Rotation
        # hides the old generation's job even from the new credential.
        call(new, "POST", result_path, body=result, expect=(404,),
             label="new credential cannot submit for the old generation")
        require(claim(port, deadline, new) is None, "rotated device inherited stale work")
        return new


def device_connector(server_binary, connector, timeout):
    alice, bob = (secrets.token_hex(32) for _ in range(2))
    with temp_state() as directory, ExitStack() as stack:
        state = Path(directory)
        deadline = time.monotonic() + timeout
        vendor = start_vendor(stack)
        port = server_port()
        db = state / "metro.sqlite"
        env = clean_env(directory)
        server = start_server_with_log_guard(stack, server_binary, state, env | {
            "METRO_BIND": f"{HOST}:{port}",
            "METRO_DB": str(db),
            "METRO_DEV_USERS": json.dumps({"alice": alice, "bob": bob}),
            "RUST_LOG": "metro_server=debug",
        }, vendor.key)
        wait_for_server(port, [server], vendor, deadline)
        call = partial(request, port, deadline)

        catalog = call(alice, "GET", "/v1/provider-catalog", read_json=True)
        matching = [row for row in catalog["providers"] if row["id"] == KIND]
        require(len(matching) == 1 and all(matching[0][key] == value for key, value in {
            "execution": "device", "format": "device", "api": "metro-connector",
            "requires_key": False, "allow_custom_url": False, "lists_models": True,
        }.items()), "Metro's catalog did not advertise the on-device adapter correctly")
        alice_id, alice_path = create_connection(call, alice, "Alice local device")
        bob_id, bob_path = create_connection(call, bob, "Bob local device")
        status, models = f"{alice_path}/device-token", f"{alice_path}/models"
        for user, expected_id in ((alice, alice_id), (bob, bob_id)):
            listed = call(user, "GET", PROVIDERS, read_json=True)
            require([item["id"] for item in listed["connections"]] == [expected_id],
                    "device connection lists were not private")
        for method, path, body in (
            ("GET", status, None), ("POST", status, {}), ("DELETE", status, None),
            ("POST", models, {}), ("PATCH", alice_path, {"model": vendor.model, "is_default": True}),
        ):
            call(bob, method, path, body=body, expect=(404,), label="Bob cannot access Alice's device")
        call(bob, "POST", CLAIM, body={}, expect=(401,), label="human credential is not a device token")
        old = issue_token(call, alice, status)
        foreign = issue_token(call, bob, f"{bob_path}/device-token")
        require(old != foreign, "Alice and Bob got the same device credential")
        initial = call(alice, "GET", status, read_json=True)
        require(initial["paired"] is True and initial["online"] is False
                and initial["last_seen_secs"] is None,
                "Metro showed a device online before it polled")
        assert_hidden(initial, (old, foreign, vendor.key, vendor.base_url))

        home = state / "connector-home"
        home.mkdir(mode=0o700)
        key_file, token_file = state / "vendor-key", state / "device-token"
        private_file(key_file, vendor.key)
        private_file(token_file, old)
        connector_env = env | {"METRO_CONNECTOR_HOME": str(home), "METRO_CONNECTOR_POLL_MS": "50"}
        setup = start_connector(stack, connector, state, connector_env, [
            "setup", "--profile", "work", "--server-url", f"http://{HOST}:{port}",
            "--kind", KIND, "--base-url", vendor.base_url, "--api-key-file", str(key_file),
            "--device-token-file", str(token_file), "--model", vendor.model, "--yes",
        ])
        wait_exit(setup, deadline, 0, "noninteractive device setup")
        key_file.unlink()
        token_file.unlink()
        profile = home / "profiles" / "work"
        require(stat.S_IMODE(profile.stat().st_mode) == 0o700, "device profile is not private")
        for name in ("profile.json", "device-token", "provider-key"):
            require(stat.S_IMODE((profile / name).stat().st_mode) == 0o600,
                    "device profile file is not private")
        require(vendor.connections == 0, "device setup unexpectedly contacted the vendor")

        run_args = ["run", "--profile", "work"]
        worker = start_connector(stack, connector, state, connector_env, run_args)
        children = [server, (worker, "device connector")]
        assert_hidden(wait_online(call, alice, status, children, vendor, deadline),
                      (old, foreign, vendor.key, vendor.base_url))
        listed = call(alice, "POST", models, body={}, read_json=True)
        require(listed.get("models") == [vendor.model] and vendor.list_calls == 1,
                "live device model listing did not reach the loopback vendor")
        updated = call(alice, "PATCH", alice_path, body={"model": vendor.model, "is_default": True},
                       read_json=True)
        require(updated["model"] == vendor.model and updated["is_default"] is True,
                "device model was not saved as Alice's default")
        trigger = call(alice, "POST", f"{CHAT}/messages", read_json=True,
                       body={"text": vendor.prompt, "invoke_agent": True})
        events = wait_chat(call, alice, trigger["id"], children, vendor, deadline)
        require(vendor.chat_calls == 2, "expected two relayed completions with a Rig tool call")
        require(not any(vendor.prompt in entry["text"] or vendor.answer in entry["text"]
                        for entry in read_events(call, bob, CHAT)), "Bob saw Alice's private Chat")
        assert_hidden((updated, events), (old, foreign, vendor.key, vendor.base_url))

        stop_child(worker)
        new = exercise_rotation(call, port, deadline, alice, models, status, old, foreign, server, vendor)
        revoked = start_connector(stack, connector, state, connector_env, run_args)
        wait_exit(revoked, deadline, 3, "connector with the rotated device token")
        require((vendor.list_calls, vendor.chat_calls, vendor.connections) == (1, 2, 3),
                "unexpected traffic reached the loopback vendor")
        for user in (alice, bob):
            assert_hidden(call(user, "GET", PROVIDERS, read_json=True),
                          (old, new, foreign, vendor.key, vendor.base_url))
        check_processes([server], vendor)
        assert_db_has_no_vendor_key(db, vendor.key)
    print("PASS: personal device listing, Rig reaction round trip, ownership and token rotation.")


if __name__ == "__main__":
    signal.signal(signal.SIGTERM, terminate)
    raise SystemExit(main(device_connector, binaries=("metro-server", "metro-connector"),
                          description=__doc__))
