#!/usr/bin/env python3
"""Bounded Linux PTY QA against real Metro and a synthetic loopback model."""

import argparse
import base64
from contextlib import ExitStack
import errno
import fcntl
from functools import partial
from http.client import HTTPConnection
from http.server import BaseHTTPRequestHandler, HTTPServer
import json
import os
from pathlib import Path
import re
import secrets
import select
import signal
import struct
import subprocess
import sys
import tempfile
import termios
import threading
import time

ROOT = Path(__file__).resolve().parents[3]
sys.path.insert(0, str(ROOT / "tests" / "e2e"))
from smoke import (  # noqa: E402
    HOST, MAX_BODY, clean_env, create_space, read_events, remaining, request,
    require, start_child, stop_child, wait_for_server, server_port,
)

F1, F2, F3, F4, F5 = b"\x1bOP", b"\x1bOQ", b"\x1bOR", b"\x1bOS", b"\x1b[15~"
F7 = b"\x1b[18~"
UP, DOWN, ENTER = b"\x1b[A", b"\x1b[B", b"\r"
ENTER_SCREEN, EXIT_SCREEN = b"\x1b[?1049h", b"\x1b[?1049l"
CLEAR = b"\x1b[2J"
SGR = re.compile(rb"\x1b\[[0-9;:]*m")
SET_COLOR = re.compile(rb"\x1b\[(?:38|48);(?:2|5);")


class Endpoint(BaseHTTPRequestHandler):
    def log_message(self, *_):
        pass

    def reply(self, code, data, headers=()):
        body = json.dumps(data).encode()
        self.send_response(code)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(body)))
        for key, value in headers:
            self.send_header(key, value)
        self.end_headers()
        self.wfile.write(body)

    def do_POST(self):
        endpoint = self.server
        try:
            size = int(self.headers["Content-Length"])
            require(0 < size <= MAX_BODY, "bad fixture request body")
            require(self.path == "/v1/chat/completions", "wrong fixture route")
            request = json.loads(self.rfile.read(size))
            model = request["model"]
            require(model in {"qa-default", "qa-alternate"}, "unexpected fixture model")
            endpoint.calls.append(model)
            answer = f"Verified reply {len(endpoint.calls)}"
            self.reply(200, {
                "id": "qa-completion", "object": "chat.completion",
                "created": int(time.time()), "model": model,
                "choices": [{
                    "index": 0, "message": {"role": "assistant", "content": answer},
                    "finish_reason": "stop",
                }],
                "usage": {"prompt_tokens": 1, "completion_tokens": 1, "total_tokens": 2},
            })
        except Exception:
            endpoint.error = "Scripted model received an invalid request"
            self.reply(400, {"error": {"message": endpoint.error}})

    def do_GET(self):
        endpoint = self.server
        if self.path == "/v1/me" and endpoint.redirect:
            endpoint.redirect_calls += 1
            self.reply(302, {}, [("Location", f"http://{HOST}:{endpoint.server_port}/unexpected")])
        else:
            endpoint.unexpected += 1
            self.reply(404, {})


class Loopback(HTTPServer):
    def __init__(self):
        super().__init__((HOST, 0), Endpoint)
        self.calls = []
        self.error = None
        self.redirect = False
        self.redirect_calls = 0
        self.unexpected = 0

    def get_request(self):
        conn, addr = super().get_request()
        conn.settimeout(2)
        return conn, addr


class Pty:
    def __init__(self, stack, binary, origin, token_path, env, deadline, forbidden):
        self.deadline = deadline
        self.forbidden = forbidden
        self.data = b""
        self.tail = b""
        self.master, self.slave = os.openpty()
        stack.callback(os.close, self.master)
        stack.callback(os.close, self.slave)
        self.saved = termios.tcgetattr(self.slave)
        self.resize(36, 132)
        command = [str(binary), "--server", origin]
        if token_path:
            command += ["--token-file", str(token_path)]
        # setsid --ctty makes no-echo, SIGWINCH and SSH-like PTY semantics real.
        self.child = subprocess.Popen(
            ["setsid", "--ctty", *command], env=env, stdin=self.slave,
            stdout=self.slave, stderr=self.slave,
        )
        stack.callback(stop_child, self.child)

    def resize(self, rows, cols):
        fcntl.ioctl(self.slave, termios.TIOCSWINSZ, struct.pack("HHHH", rows, cols, 0, 0))

    def send(self, data):
        require(len(data) < 4096, "fixture tried an oversized PTY input")
        require(os.write(self.master, data) == len(data), "incomplete PTY input")

    def paste(self, text):
        self.send(b"\x1b[200~" + text.encode() + b"\x1b[201~")

    def read(self, timeout):
        if select.select([self.master], [], [], timeout)[0]:
            try:
                chunk = os.read(self.master, 65_536)
            except OSError as exc:
                if exc.errno == errno.EIO:
                    return
                raise
            combined = self.tail + chunk
            require(not any(secret in combined for secret in self.forbidden),
                    "PTY output exposed a token or untrusted escape sequence")
            self.tail = combined[-128:]
            self.data = (self.data + chunk)[-262_144:]

    def wait(self, text, *, fresh=False, absent=None):
        wanted = text.encode() if isinstance(text, str) else text
        unwanted = absent.encode() if isinstance(absent, str) else absent
        deadline = min(self.deadline, time.monotonic() + 8)
        if fresh:
            self.data = b""
        while True:
            within = self.frame()
            if wanted in within and (unwanted is None or unwanted not in within):
                return
            require(self.child.poll() is None, f"TUI exited before rendering {text!r}")
            if time.monotonic() >= deadline:
                raise RuntimeError(f"TUI did not render {text!r} before the deadline")
            self.read(min(0.08, remaining(deadline)))

    def frame(self):
        return SGR.sub(b"", self.data.rsplit(CLEAR, 1)[-1])

    def require_rgb(self, text, fg, bg):
        code = lambda plane, rgb: f"\x1b[{plane};2;{';'.join(map(str, rgb))}m".encode()
        raw = self.data.rsplit(CLEAR, 1)[-1]
        colored = code(38, fg) + code(48, bg)
        require(re.search(re.escape(colored) + rb"(?:\x1b\[1m)?" + re.escape(text.encode()), raw)
                is not None, f"missing expected foreground/background pair on {text!r}")

    def exit(self, code, *, screen=True):
        self.child.wait(timeout=min(3, remaining(self.deadline)))
        self.read(0.05)
        require(self.child.returncode == code, f"unexpected TUI exit status: {self.child.returncode}")
        require(termios.tcgetattr(self.slave) == self.saved, "terminal mode was not restored")
        if screen:
            require(EXIT_SCREEN in self.data and b"\x1b[?2004l" in self.data,
                    "alternate screen or paste mode was not restored")
        else:
            require(ENTER_SCREEN not in self.data, "unsafe token config entered the TUI")


def token_file(directory, name, token, mode=0o600):
    path = directory / name
    with os.fdopen(os.open(path, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600), "w") as stream:
        # Synthetic fixtures live in a private temporary directory. Set
        # the exact test mode on the fd even when the caller uses umask 077.
        os.fchmod(stream.fileno(), mode)
        stream.write(token + "\n")
    return path


def check_message(call, user, place, text, mentions):
    events = read_events(call, user, place)
    found = [e for e in events if e["kind"] == "message" and e["text"] == text]
    require(len(found) == 1 and found[0]["mentions"] == mentions,
            "Metro stored duplicate messages or incorrect recipients")


def claim_device(port, deadline, token, model):
    conn = HTTPConnection(HOST, port, timeout=min(2, remaining(deadline)))
    try:
        conn.request("POST", "/v1/device/claim", json.dumps({"approved_models": [model]}), {
            "Content-Type": "application/json", "Authorization": f"Bearer {token}",
        })
        with conn.getresponse() as response:
            require(response.status in (200, 204), "fixture relay poll failed")
            if response.status == 204:
                return None
            data = response.read(MAX_BODY + 1)
            require(len(data) <= MAX_BODY, "oversized fixture relay claim")
            return json.loads(data)
    finally:
        conn.close()


def main(server, tui):
    deadline = time.monotonic() + 50
    alice, bob, invalid = [secrets.token_hex(24) for _ in range(3)]
    escape = b"\x1b]52;qa-not-a-terminal-command\x07"
    forbidden = [s.encode() for s in [alice, bob, invalid]] + [escape]
    with tempfile.TemporaryDirectory(prefix="metro-tui-qa-") as directory, ExitStack() as stack:
        state = Path(directory)
        endpoint = stack.enter_context(Loopback())
        thread = threading.Thread(target=endpoint.serve_forever, kwargs={"poll_interval": 0.05})
        thread.start()
        stack.callback(thread.join, 2)
        stack.callback(endpoint.shutdown)
        base = f"http://{HOST}:{endpoint.server_port}/v1"
        env = clean_env(directory) | {"TMPDIR": os.environ["TMPDIR"], "TERM": "xterm-256color"}
        port = server_port()
        children = [start_child(stack, "server", server, state, env | {
            "METRO_BIND": f"{HOST}:{port}",
            "METRO_DB": str(state / "metro.sqlite"),
            "METRO_DEV_USERS": json.dumps({"alice": alice, "bob": bob}),
            "METRO_PROVIDER_MASTER_KEY": base64.b64encode(secrets.token_bytes(32)).decode(),
            "METRO_PROVIDER_ALLOWED_BASE_URLS": json.dumps([base]),
        })]
        wait_for_server(port, children, endpoint, deadline)
        call = partial(request, port, deadline)
        origin = f"http://{HOST}:{port}"
        chat = "/v1/chat"
        space = f"/v1/spaces/{create_space(call, alice, 'QA Space')}"
        room = f"{space}/rooms/lobby"
        create_space(call, bob, "Invisible QA Space")
        call(alice, "POST", f"{space}/members", body={"user_id": "bob"})
        connections = {}
        for name, model in [("QA Default", "qa-default"), ("QA Alternate", "qa-alternate")]:
            response = call(alice, "POST", "/v1/providers", body={
                "provider_id": "custom", "name": name, "model": model, "base_url": base,
            }, read_json=True)
            connections[model] = f"/v1/providers/{response['id']}"
        call(alice, "PATCH", connections["qa-default"], body={"is_default": True})
        call(alice, "POST", f"{chat}/messages", body={
            "text": f"Untrusted display {escape.decode()} {alice}", "invoke_agent": False,
        })
        private = token_file(state, "alice-token", alice)
        # Proxy variables are intentionally pointed at the model. The TUI
        # must go directly to Metro, not use them (including on loopback).
        proxy = f"http://{HOST}:{endpoint.server_port}"
        tui_env = env | {"HTTP_PROXY": proxy, "http_proxy": proxy, "ALL_PROXY": proxy}

        ui = Pty(stack, tui, origin, private, tui_env | {"COLORTERM": "truecolor"}, deadline, forbidden)
        ui.wait("Untrusted display", absent="Invisible QA Space")
        ui.wait("+ QA Space")
        ui.wait("[Signed in]")
        slate, ink = (248, 250, 255), (11, 15, 25)
        ui.require_rgb(" metro ", slate, (10, 88, 255))
        ui.require_rgb(" | alice", slate, (8, 27, 59))
        ui.require_rgb(" [Signed in] ", ink, (16, 185, 129))
        ui.require_rgb("> Personal Chat", slate, (10, 88, 255))
        require(not endpoint.calls, "entering TUI started model work")
        ui.send(F1)
        ui.wait("KEYBOARD HELP")
        ui.wait("F7          Hide / show sidebar")
        ui.send(F1)
        ui.wait("Enter Send")
        ui.paste("Save this note")
        ui.wait("Enter Ask agent")
        ui.wait("F3 Save note")
        ui.send(F3)
        ui.wait("alice → everyone")
        ui.wait("Enter Send")
        check_message(call, alice, chat, "Save this note", [])
        require(not endpoint.calls, "Save note unexpectedly asked the agent")
        ui.paste("A personal question")
        ui.wait("Enter Ask agent")
        ui.send(ENTER)
        ui.wait("Verified reply 1")
        ui.require_rgb("[AI]", ink, (139, 92, 246))
        check_message(call, alice, chat, "A personal question", [{"kind": "agent"}])
        require(endpoint.calls == ["qa-default"], "primary Ask did not use the default")

        ui.paste("Unsent chat draft")
        ui.wait("Unsent chat draft")
        ui.send(b"\t" + DOWN + ENTER)
        ui.wait("# lobby")
        ui.send(DOWN + ENTER)
        ui.wait("QA Space / lobby", absent="Unsent chat draft")
        ui.wait("No messages yet.")
        ui.paste("@bob human-only")
        ui.wait("@bob human-only")
        ui.wait("F7 Hide sidebar")
        ui.send(b"\t" + UP)
        ui.wait("CONVERSATIONS  <")
        ui.wait("> − QA Space")
        ui.send(F7)
        ui.wait("F7 Show sidebar", absent="CONVERSATIONS")
        ui.wait(b"\x1b[3;1HQA Space / lobby")  # the conversation uses the reclaimed width
        ui.wait("@bob human-only")

        ui.resize(14, 60)
        ui.wait("QA Space / lobby", absent="F7 Show sidebar")
        ui.send(F7 + b"\t")
        ui.wait("CONVERSATIONS  <", absent="QA Space / lobby")
        ui.wait("> − QA Space")
        ui.wait("Enter open · Tab draft · F1 help", absent="F7 Hide sidebar")
        ui.send(b"\t")
        ui.wait("@bob human-only", absent="CONVERSATIONS")
        ui.resize(36, 132)
        ui.wait("F7 Show sidebar", absent="CONVERSATIONS")
        ui.send(F7)
        ui.wait("F7 Hide sidebar", absent="CONVERSATIONS  <")
        ui.wait("> − QA Space")
        ui.send(F7)
        ui.wait("F7 Show sidebar", absent="CONVERSATIONS")
        ui.send(b"\t")
        ui.wait("CONVERSATIONS  <")
        ui.wait("> − QA Space")
        ui.send(b"\t")
        ui.wait("@bob human-only", absent="CONVERSATIONS  <")
        require(not read_events(call, bob, room) and endpoint.calls == ["qa-default"],
                "sidebar navigation sent a draft or started a model")
        ui.send(ENTER)
        ui.wait("alice → @bob")
        check_message(call, bob, room, "@bob human-only", [{"kind": "user", "id": "bob"}])
        require(endpoint.calls == ["qa-default"], "people-only room Send started a model")
        ui.paste("A plain room post")
        ui.send(F3)
        ui.wait("A plain room post")
        ui.wait("alice → everyone")
        check_message(call, bob, room, "A plain room post", [])
        ui.paste("A room question")
        ui.send(F2)
        ui.wait("Verified reply 2")
        check_message(call, bob, room, "A room question", [{"kind": "agent"}])
        require(endpoint.calls == ["qa-default", "qa-default"], "room Ask did not use the default")

        ui.send(b"\t\x1b[H" + ENTER)
        ui.wait("Unsent chat draft", absent="A room question")
        ui.send(F4)
        ui.wait("qa-alternate · QA Alternate")
        # Discover order from the frame; this is a black-box keyboard test.
        frame = ui.frame()
        step = 1 if frame.rfind(b"qa-alternate") < frame.rfind(b"qa-default") else 2
        ui.send(DOWN * step + ENTER)
        ui.wait("Model:")
        ui.wait("Unsent chat draft")
        ui.send(F2)
        ui.wait("Verified reply 3")
        require(endpoint.calls[-1] == "qa-alternate", "selected model fell back to default")
        call(alice, "DELETE", connections["qa-alternate"])
        ui.paste("Draft after revocation")
        ui.send(F2)
        ui.wait("Model or thinking level unavailable.")
        ui.require_rgb("[Warning]", ink, (245, 158, 11))
        ui.wait("Draft after revocation")
        require(len(endpoint.calls) == 3, "revoked selection started model work")
        require(not any(e["text"] == "Draft after revocation"
                        for e in read_events(call, alice, chat)), "revoked selection sent or replayed a POST")
        ui.send(F4)
        ui.wait("qa-default · QA Default")
        ui.send(DOWN + ENTER)
        ui.wait("QA Default")
        ui.send(F2)
        ui.wait("Verified reply 4")
        check_message(call, alice, chat, "Draft after revocation", [{"kind": "agent"}])
        require(endpoint.calls[-1] == "qa-default", "reselected connection was not used")

        # Exercise a real advertised thinking level without signing into a
        # vendor: pair a synthetic device that reports inventory and returns
        # one scripted OpenAI-format completion directly to Metro's relay.
        relay_model = "gpt-6.2-sol"
        relay = call(alice, "POST", "/v1/providers", body={
            "provider_id": "device-openai-oauth", "name": "QA Relay", "model": relay_model,
        }, read_json=True)
        relay_path = f"/v1/providers/{relay['id']}"
        device = call(alice, "POST", f"{relay_path}/device-token", body={}, read_json=True)["token"]
        forbidden.append(device.encode())
        require(claim_device(port, deadline, device, relay_model) is None, "unexpected initial relay job")
        ui.send(F4)
        ui.wait("Thinking: high")
        frame = ui.frame()
        rows = [part for part in frame.split(b"\x1b[") if b";1H  " in part and b"> " not in part]
        choice = next(i for i, part in enumerate(rows, 1) if b"Thinking: high" in part)
        ui.send(DOWN * choice + ENTER)
        ui.wait("Thinking: high", absent="MODEL & THINKING")
        ui.paste("Requested thinking")
        ui.send(F2)
        while (job := claim_device(port, deadline, device, relay_model)) is None:
            time.sleep(min(0.05, remaining(deadline)))
        require(job["operation"] == "complete" and job["request"]["model"] == relay_model
                and job["request"]["reasoning_effort"] == "high",
                "explicit thinking did not reach the selected model")
        call(device, "POST", f"/v1/device/jobs/{job['id']}/result", body={
            "ok": True,
            "response": {
                "id": "qa-relay-completion", "object": "chat.completion",
                "created": int(time.time()), "model": relay_model,
                "choices": [{"index": 0, "finish_reason": "stop",
                             "message": {"role": "assistant", "content": "Verified thinking reply"}}],
            },
        }, expect=(204,))
        ui.wait("Verified thinking reply")
        check_message(call, alice, chat, "Requested thinking", [{"kind": "agent"}])

        ui.resize(20, 100)
        ui.send(b"\x1b[5~")
        ui.wait("Scrolled up")
        ui.require_rgb("[Info]", ink, (6, 182, 212))
        ui.send(b"\x1b[6~" * 8)
        ui.wait("Verified thinking reply", absent="Scrolled up")

        ui.paste(f"Accidental credential: {alice}")
        ui.send(F3)
        ui.wait("Remove the sign-in token from the draft.")
        require(not any(e["text"].startswith("Accidental credential:")
                        for e in read_events(call, alice, chat)), "TUI sent its credential in a draft")
        ui.resize(6, 32)
        ui.wait("Resize · F10 quit")
        ui.resize(24, 84)
        ui.wait("Tab Browse")
        ui.send(b"\x03")
        ui.exit(0)

        hidden = Pty(stack, tui, origin, None, tui_env, deadline, forbidden)
        hidden.wait("Metro token:")
        hidden.send(alice.encode() + ENTER)
        hidden.wait("| alice")
        require(b"\x1b[38;5;231m\x1b[48;5;27m" in hidden.data, "missing indexed wordmark colors")
        require(b"\x1b[38;2;" not in hidden.data, "indexed terminal received RGB colors")
        hidden.child.send_signal(signal.SIGINT)
        hidden.exit(0)

        interrupted = Pty(stack, tui, origin, None, tui_env, deadline, forbidden)
        interrupted.wait("Metro token:")
        interrupted.child.send_signal(signal.SIGINT)
        interrupted.exit(1, screen=False)

        for stopping_signal in (signal.SIGHUP, signal.SIGTERM):
            stopping = Pty(stack, tui, origin, private, tui_env, deadline, forbidden)
            stopping.wait("| alice")
            stopping.child.send_signal(stopping_signal)
            stopping.exit(0)

        wrong = Pty(stack, tui, origin, token_file(state, "invalid-token", invalid),
                    tui_env | {"COLORTERM": "24bit"}, deadline, forbidden)
        wrong.wait("HTTP 401")
        wrong.require_rgb("[Error]", ink, (239, 68, 68))
        wrong.send(b"\x03")
        wrong.exit(1)

        for mono_env in ({"NO_COLOR": "1", "COLORTERM": "truecolor"}, {"TERM": "xterm"}):
            mono = Pty(stack, tui, origin, private, tui_env | mono_env, deadline, forbidden)
            mono.resize(12, 34)
            mono.wait("Tab Browse · F1 Help")
            mono.wait("| alice")
            mono.send(b"\t")
            mono.wait("CONVERSATIONS  <")
            mono.wait("> Personal Chat")
            require(b"\x1b[7m> Personal Chat" in mono.data, "monochrome selection lacks inverse")
            mono.send(F1)
            mono.wait("KEYBOARD HELP")
            require(SET_COLOR.search(mono.data) is None, "monochrome terminal received color")
            mono.send(b"\x03")
            mono.exit(0)

        dumb = Pty(stack, tui, origin, private, tui_env | {"TERM": "dumb"}, deadline, forbidden)
        dumb.exit(1, screen=False)
        require(b"TERM=dumb lacks cursor addressing" in dumb.data and b"\x1b" not in dumb.data,
                "dumb terminal received control sequences or no explanation")

        exposed = Pty(stack, tui, origin, token_file(state, "exposed-token", invalid, 0o644),
                      tui_env, deadline, forbidden)
        exposed.exit(1, screen=False)
        require(b"private (chmod 600)" in exposed.data, "nonprivate token file was not rejected")

        endpoint.redirect = True
        redirect = Pty(stack, tui, proxy, private, tui_env, deadline, forbidden)
        redirect.wait("HTTP 302")
        redirect.send(b"\x03")
        redirect.exit(1)
        require(endpoint.redirect_calls == 1 and endpoint.unexpected == 0,
                "TUI followed redirect or used the environment proxy")
        require(endpoint.error is None and len(endpoint.calls) == 4,
                "scripted model rejected an unexpected request")


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--bin-dir", type=Path, default=ROOT / "target" / "debug")
    args = parser.parse_args()
    try:
        main((args.bin_dir / "metro-server").resolve(), (args.bin_dir / "metro-tui").resolve())
    except (OSError, RuntimeError, TimeoutError, subprocess.TimeoutExpired) as exc:
        print(f"metro-tui PTY QA failed: {exc}", file=sys.stderr)
        sys.exit(1)
    print("metro-tui PTY QA passed (real isolated Metro, scripted model, no real credentials).")
