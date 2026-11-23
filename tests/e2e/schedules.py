#!/usr/bin/env python3
"""Black-box end-to-end test of Metro scheduled agent prompts.

Run with: python3 tests/e2e/schedules.py [--bin-dir DIR] [--timeout SECONDS]

Needs only the real `metro-server` binary. Everything else is local: dev-token
users, a temporary on-disk SQLite database, and three scripted
OpenAI-compatible model stubs on loopback (one per user connection plus the
server fallback), so every model call is attributable and countable. No
external service, OpenChamber scheduler, CLI model, or stored-timestamp edit is
involved. Time is the real clock: once-schedules are created a few seconds in
the future and waited for with bounded polling.

Contract exercised (as given to QA; response shapes are recorded, see the
"recorded response shapes" block printed at the end):

    GET|POST /v1/schedules            GET /v1/schedules/{id}/history
    PATCH|DELETE /v1/schedules/{id}
    body: name, prompt, space_id, room_id,
          schedule {kind:'once', at:<RFC3339>}
                 | {kind:'cron', expression:<5 fields>, timezone:<IANA>},
          enabled

A schedule without space_id/room_id targets the owner's private Chat.

Exact wrappers: GET /v1/schedules -> {tasks:[...]}; history -> {runs:[...]}. History `status`
`dispatched` is a success of *dispatch* only; the answer itself is verified independently from the
transcript and the model stubs. A scheduled prompt is an explicit request: no classifier call.

Product rules asserted: recurring at most every 15 minutes (too frequent -> 400), once strictly in
the future and within 366 days, a 5-minute catch-up grace, `revoked` (task paused + a notice in the
owner's personal Chat) when the owner left the Space. The default run validates cron without
waiting for a recurring firing; --live-recurring checks that separately on the real clock.
Pure cron expectations are self-checked against seeded instants, including DST, before any I/O.
"""

import argparse
import base64
from contextlib import ExitStack
from datetime import datetime, timedelta, timezone
import http.client
from http.server import BaseHTTPRequestHandler, HTTPServer
import json
import math
import os
from pathlib import Path
import re
import secrets
import signal
import subprocess
import sys
import threading
import time
from urllib.parse import quote
from zoneinfo import ZoneInfo

from smoke import (
    HOST, MAX_BODY, SPACES, check_processes, clean_env, content_parts, remaining, require,
    server_port, start_child, stop_child, temp_state, terminate, wait_for_server,
)

ROOT = Path(__file__).resolve().parents[2]
SCHEDULES = "/v1/schedules"
CHAT = "/v1/chat"
UTC = timezone.utc
MARKER = re.compile(r"SCHED-[a-z0-9-]+-[0-9a-f]{8}\b")
# History records what the scheduler did, not how inference ended: `dispatched` means the
# occurrence was handed to the agent (a success *of dispatch*). Whether the model actually
# answered is checked independently, from the transcript and the model stubs' call counts.
DISPATCHED = "dispatched"
SHAPES = {}
NOTES = []


# ---------------------------------------------------------------- recording

def shape_of(value):
    """Key/type skeleton of a JSON value. Never contains any value text."""
    if isinstance(value, dict):
        return {key: shape_of(item) for key, item in sorted(value.items())}
    if isinstance(value, list):
        return [shape_of(value[0])] if value else []
    return type(value).__name__ if value is not None else "null"


def record(label, status, data):
    SHAPES.setdefault(label, {"status": status, "shape": shape_of(data)})


def note(message):
    NOTES.append(message)


# ---------------------------------------------------------------- model stub

class ModelStub(HTTPServer):
    """Scripted OpenAI-compatible chat endpoint that counts calls by marker.

    The marker is the last `SCHED-...` token of the last user message, so
    history that merely repeats earlier prompts is never mistaken for a call.
    It answers every request with plain text `ACK <marker>` and never emits
    tool calls.
    """

    def __init__(self, label, model, api_key):
        super().__init__((HOST, 0), StubHandler)
        self.label = label
        self.model = model
        self.api_key = api_key
        self.base_url = f"http://{HOST}:{self.server_port}/v1"
        self.error = None
        self.lock = threading.Lock()
        self.calls = {}
        self.unmarked = 0
        # Optional hold: an Event that, while unset, keeps an accepted request unanswered.
        self.gate = None
        self.arrived = threading.Event()

    def get_request(self):
        connection, address = super().get_request()
        connection.settimeout(5)
        return connection, address

    def count(self, marker):
        with self.lock:
            return self.calls.get(marker, 0)

    def total(self):
        with self.lock:
            return sum(self.calls.values()) + self.unmarked

    def snapshot(self):
        with self.lock:
            return dict(self.calls), self.unmarked

    def complete(self, authorization, request):
        require(authorization == f"Bearer {self.api_key}", f"{self.label}: wrong credentials")
        require(request.get("model") == self.model, f"{self.label}: wrong model selected")
        require(not request.get("stream"), f"{self.label}: unexpected streaming request")
        users = [m for m in request.get("messages", []) if m.get("role") == "user"]
        require(users, f"{self.label}: request without a user message")
        found = MARKER.findall("\n".join(content_parts(users[-1].get("content"))))
        with self.lock:
            if found:
                self.calls[found[-1]] = self.calls.get(found[-1], 0) + 1
            else:
                self.unmarked += 1
        if self.gate is not None:
            self.arrived.set()
            self.gate.wait(90)  # bounded; released by the test (or at cleanup)
        text = f"ACK {found[-1]}" if found else "ACK none"
        return {
            "id": f"chatcmpl-sched-{self.label}",
            "object": "chat.completion",
            "created": int(time.time()),
            "model": self.model,
            "choices": [{"index": 0, "message": {"role": "assistant", "content": text},
                         "finish_reason": "stop"}],
            "usage": {"prompt_tokens": 1, "completion_tokens": 1, "total_tokens": 2},
        }


class StubHandler(BaseHTTPRequestHandler):
    def log_message(self, *_):
        pass

    def do_GET(self):
        # Connection setup may list models; offer exactly the stub's model.
        stub = self.server
        if self.path == "/v1/models":
            self.reply(200, {"object": "list", "data": [{"id": stub.model, "object": "model"}]})
        else:
            self.reply(404, {"error": {"message": "not found"}})

    def do_POST(self):
        stub = self.server
        try:
            require(self.path == "/v1/chat/completions", f"{stub.label}: unexpected POST path")
            size = int(self.headers["Content-Length"])
            require(0 < size <= MAX_BODY, f"{stub.label}: invalid request size")
            body = stub.complete(self.headers.get("Authorization"), json.loads(self.rfile.read(size)))
            status = 200
        except Exception as exc:
            detail = str(exc) if type(exc) is RuntimeError else "invalid completion request"
            stub.error = stub.error or f"model stub: {detail}"
            body, status = {"error": {"message": detail}}, 400
        self.reply(status, body)

    def reply(self, status, body):
        data = json.dumps(body).encode()
        self.send_response(status)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(data)))
        self.end_headers()
        self.wfile.write(data)


def serve_stub(stack, stub):
    stack.enter_context(stub)
    thread = threading.Thread(target=stub.serve_forever, kwargs={"poll_interval": 0.05})
    thread.start()
    stack.callback(thread.join, 3)
    stack.callback(stub.shutdown)
    return stub


def require_no_new_calls(stubs, before, label):
    for stub, (prior, prior_unmarked) in zip(stubs, before):
        calls, unmarked = stub.snapshot()
        if (calls, unmarked) != (prior, prior_unmarked):
            changes = sorted((key.rsplit("-", 1)[0], calls.get(key, 0) - prior.get(key, 0))
                             for key in calls.keys() | prior.keys() if calls.get(key) != prior.get(key))
            raise RuntimeError(f"{label}: {stub.label} call changes {changes}, "
                               f"unmarked {unmarked - prior_unmarked:+d}")


# ---------------------------------------------------------------- server

class Server:
    """One metro-server on a fixed port and database, restartable in place."""

    def __init__(self, stack, binary, state, env, port, watch):
        self.stack, self.binary, self.state, self.env = stack, binary, state, env
        self.port, self.watch = port, watch
        self.child = None
        self.stopped_on_purpose = False

    def start(self, deadline):
        self.stopped_on_purpose = False
        # Opt-in diagnostics: METRO_E2E_SERVER_LOG=<path> appends the server's own log (mode 600).
        # It is off by default because child output can carry sensitive values.
        log_path = os.environ.get("METRO_E2E_SERVER_LOG")
        sink = subprocess.DEVNULL
        if log_path:
            sink = os.fdopen(os.open(log_path, os.O_WRONLY | os.O_CREAT | os.O_APPEND, 0o600), "ab")
            self.stack.callback(sink.close)
        self.child = subprocess.Popen(
            [str(self.binary)], cwd=self.state, env=self.env, stdin=subprocess.DEVNULL,
            stdout=sink, stderr=sink,
        )
        self.stack.callback(stop_child, self.child)
        wait_for_server(self.port, [(self.child, "metro-server")], self.watch, deadline)

    def stop(self, kill=False):
        self.stopped_on_purpose = True
        if self.child is None:
            return
        if kill and self.child.poll() is None:
            self.child.kill()  # SIGKILL: a crash, not a clean shutdown
            self.child.wait(timeout=5)
        else:
            stop_child(self.child)
        self.child = None

    def alive(self):
        """False only when the process died without the test stopping it."""
        return self.stopped_on_purpose or (self.child is not None and self.child.poll() is None)


def send(port, deadline, token, method, path, body=None):
    """(status, parsed JSON or None). Raises only on transport problems."""
    connection = http.client.HTTPConnection(HOST, port, timeout=min(10, remaining(deadline)))
    try:
        headers = {"Accept": "application/json"}
        if token is not None:
            headers["Authorization"] = f"Bearer {token}"
        payload = None
        if body is not None:
            payload = json.dumps(body)
            headers["Content-Type"] = "application/json"
        connection.request(method, path, payload, headers)
        with connection.getresponse() as response:
            raw = response.read(MAX_BODY + 1)
            require(len(raw) <= MAX_BODY, f"{method} {path}: oversized response")
            data = None
            if raw.strip():
                try:
                    data = json.loads(raw)
                except ValueError:
                    raise RuntimeError(f"{method} {path}: invalid JSON (HTTP {response.status})") from None
            return response.status, data
    except (TimeoutError, OSError, http.client.HTTPException) as exc:
        raise RuntimeError(f"{method} {path}: transport failure ({type(exc).__name__})") from None
    finally:
        connection.close()


def error_text(data):
    if isinstance(data, dict) and isinstance(data.get("error"), str):
        return data["error"][:160]
    return ""


class Api:
    def __init__(self, server, deadline):
        self.server, self.deadline = server, deadline

    def raw(self, token, method, path, body=None):
        return send(self.server.port, self.deadline, token, method, path, body)

    def ok(self, token, method, path, body=None, expect=(200, 201, 202, 204), label=None):
        status, data = self.raw(token, method, path, body)
        require(status in expect,
                f"{label or method + ' ' + path}: HTTP {status}, expected {expect} {error_text(data)}")
        return status, data

    def rejected(self, token, method, path, body, expect, label):
        status, data = self.raw(token, method, path, body)
        require(status in expect, f"{label}: HTTP {status}, expected {expect} {error_text(data)}")
        return status


# ---------------------------------------------------------------- time and cron

def rfc3339(moment):
    return moment.astimezone(UTC).strftime("%Y-%m-%dT%H:%M:%SZ")


def parse_time(value):
    require(isinstance(value, str), f"timestamp was {type(value).__name__}, not an RFC 3339 string")
    moment = datetime.fromisoformat(value)
    require(moment.tzinfo is not None, "timestamp had no UTC offset")
    return moment.astimezone(UTC)


def field_matches(spec, value, low, high):
    for part in spec.split(","):
        step = 1
        if "/" in part:
            part, step_text = part.split("/")
            step = int(step_text)
        if part == "*":
            start, end = low, high
        elif "-" in part:
            start, end = (int(x) for x in part.split("-"))
        else:
            start = end = int(part)
            if step != 1:
                end = high
        if start <= value <= end and (value - start) % step == 0:
            return True
    return False


def next_cron(expression, tz_name, after):
    """First minute strictly after `after` matching a 5-field cron in `tz_name`."""
    minute, hour, dom, month, dow = expression.split()
    tz = ZoneInfo(tz_name)
    moment = after.astimezone(UTC).replace(second=0, microsecond=0) + timedelta(minutes=1)
    for _ in range(60 * 24 * 40):
        local = moment.astimezone(tz)
        cron_dow = (local.weekday() + 1) % 7
        if (field_matches(minute, local.minute, 0, 59) and field_matches(hour, local.hour, 0, 23)
                and field_matches(month, local.month, 1, 12)
                and (field_matches(dow, cron_dow, 0, 6) or (cron_dow == 0 and field_matches(dow, 7, 0, 7)))
                and field_matches(dom, local.day, 1, 31)):
            return moment
        moment += timedelta(minutes=1)
    raise RuntimeError("no cron match within 40 days (test helper)")


NEXT_KEYS = ("next_run_at", "next_due_at", "next_due", "next_run", "next_fire_at", "next_at")


def next_due(view):
    """The schedule's next due instant (aware datetime) or None when there is none."""
    for key in NEXT_KEYS:
        if key in view:
            return None if view[key] is None else parse_time(view[key])
    candidates = [key for key in view if key.startswith("next")]
    require(len(candidates) == 1,
            f"no next-due field in schedule (keys: {sorted(view)}); expected one of {NEXT_KEYS}")
    return None if view[candidates[0]] is None else parse_time(view[candidates[0]])


# ---------------------------------------------------------------- response helpers

def listify(data, label, *preferred):
    if isinstance(data, list):
        return data
    require(isinstance(data, dict), f"{label}: expected an object or array")
    for key in preferred:
        if isinstance(data.get(key), list):
            return data[key]
    lists = [value for value in data.values() if isinstance(value, list)]
    require(len(lists) == 1, f"{label}: could not find the list (keys: {sorted(data)})")
    return lists[0]


def wrapped(data, label, key):
    """The documented exact wrapper: {key: [...]}."""
    require(isinstance(data, dict) and isinstance(data.get(key), list),
            f"{label}: expected {{{key}: [...]}}, got "
            f"{sorted(data) if isinstance(data, dict) else type(data).__name__}")
    return data[key]


def sid(view):
    require(isinstance(view, dict) and isinstance(view.get("id"), str) and view["id"],
            "schedule response had no string id")
    return view["id"]


def item(schedule_id):
    return f"{SCHEDULES}/{quote(schedule_id, safe='')}"


def once(at):
    return {"kind": "once", "at": rfc3339(at)}


def cron(expression, tz="UTC"):
    return {"kind": "cron", "expression": expression, "timezone": tz}


def marker(label):
    return f"SCHED-{label}-{secrets.token_hex(4)}"


class Plan:
    """A schedule the test created, with what must be true of its execution."""

    def __init__(self, owner, label, view, prompt_marker, prompt, target):
        self.owner, self.label, self.view = owner, label, view
        self.id, self.marker, self.prompt, self.target = sid(view), prompt_marker, prompt, target


class Users:
    def __init__(self, tokens):
        self.tokens = tokens

    def __getitem__(self, name):
        return self.tokens[name]


def create(api, user, label, schedule, *, space=None, room=None, enabled=True, name=None,
           expect=(201, 200), prompt_marker=None):
    prompt_marker = prompt_marker or marker(label)
    prompt = f"{prompt_marker} please acknowledge this scheduled request"
    body = {"name": name or f"e2e {label}", "prompt": prompt, "schedule": schedule,
            "enabled": enabled}
    if space is not None:
        body["space_id"], body["room_id"] = space, room
    status, data = api.ok(user, "POST", SCHEDULES, body, expect, f"create schedule {label}")
    record("POST /v1/schedules", status, data)
    return Plan(user, label, data, prompt_marker, prompt, (space, room)), body


def check_view(view, body, label):
    require(isinstance(view, dict), f"{label}: schedule was not an object")
    sid(view)
    require(view.get("name") == body["name"], f"{label}: name was not echoed")
    require(view.get("prompt") == body["prompt"], f"{label}: prompt was not echoed")
    require(view.get("enabled") is body["enabled"], f"{label}: enabled was not echoed")
    schedule = view.get("schedule")
    require(isinstance(schedule, dict) and schedule.get("kind") == body["schedule"]["kind"],
            f"{label}: schedule.kind was not echoed")
    if schedule["kind"] == "once":
        require(parse_time(schedule.get("at")) == parse_time(body["schedule"]["at"]),
                f"{label}: schedule.at changed instant")
    else:
        require(schedule.get("expression") == body["schedule"]["expression"]
                and schedule.get("timezone") == body["schedule"]["timezone"],
                f"{label}: cron expression or timezone was not echoed")
    require(view.get("space_id") == body.get("space_id") and view.get("room_id") == body.get("room_id"),
            f"{label}: target was not echoed")


def list_schedules(api, user):
    status, data = api.ok(user, "GET", SCHEDULES, expect=(200,), label="list schedules")
    record("GET /v1/schedules", status, data)
    return wrapped(data, "GET /v1/schedules", "tasks")


def find_in_list(api, user, schedule_id):
    return next((s for s in list_schedules(api, user) if s.get("id") == schedule_id), None)


def history(api, user, schedule_id, expect=(200,)):
    status, data = api.raw(user, "GET", f"{item(schedule_id)}/history")
    require(status in expect, f"history of {schedule_id}: HTTP {status}, expected {expect}")
    if status != 200:
        return None
    record("GET /v1/schedules/{id}/history", status, data)
    entries = wrapped(data, "GET /v1/schedules/{id}/history", "runs")
    for entry in entries:
        require(isinstance(entry, dict), "history entry was not an object")
    return entries


def entry_error(entry):
    """The human-readable reason recorded on a history entry, or ''."""
    for key in ("detail", "error", "message", "reason"):
        if isinstance(entry.get(key), str) and entry[key].strip():
            return entry[key]
    return ""


def entry_state(entry):
    for key in ("status", "state", "outcome", "result"):
        if isinstance(entry.get(key), str):
            return entry[key].lower()
    return None


# ---------------------------------------------------------------- transcripts

def chat_events(api, user):
    status, data = api.ok(user, "GET", f"{CHAT}/events?after=0", expect=(200,), label="chat events")
    return listify(data, "chat events", "events")


def room_events(api, user, space, room):
    path = f"{SPACES}/{quote(space, safe='')}/rooms/{quote(room, safe='')}/events?after=0"
    status, data = api.ok(user, "GET", path, expect=(200,), label="room events")
    return listify(data, "room events", "events")


def results(events, prompt_marker):
    return [e for e in events if e.get("kind") == "agent" and e.get("text") == f"ACK {prompt_marker}"]


def prompt_messages(events, prompt_marker):
    return [e for e in events if e.get("kind") == "message" and prompt_marker in e.get("text", "")]


def lifecycle(events, phase):
    return [e for e in events if e.get("kind") == "activity"
            and isinstance(e.get("activity"), dict) and e["activity"].get("phase") == phase]


def poll(predicate, what, deadline, limit, watch, step=0.25):
    """Poll until predicate() is truthy, at most `limit` seconds (and the global deadline)."""
    stop = min(deadline, time.monotonic() + limit)
    while True:
        for guard in watch:
            guard()
        value = predicate()
        if value:
            return value
        if time.monotonic() >= stop:
            raise RuntimeError(f"timed out waiting for {what}")
        time.sleep(min(step, max(0.0, stop - time.monotonic())))


# ---------------------------------------------------------------- test phases

def selfcheck_cron():
    """The helper that predicts next-due must itself be right, DST included (seeded, offline)."""
    def at(text):
        return datetime.fromisoformat(text).astimezone(UTC)

    cases = [
        # Manila has no DST: 08:30 local is 00:30Z every day.
        ("30 8 * * *", "Asia/Manila", "2026-11-22T12:00:00+00:00", "2026-11-23T00:30:00+00:00"),
        # New York: EST (UTC-5) before 2027-03-14, EDT (UTC-4) after.
        ("0 9 * * *", "America/New_York", "2027-03-13T15:00:00+00:00", "2027-03-14T13:00:00+00:00"),
        ("0 9 * * *", "America/New_York", "2027-03-13T01:00:00+00:00", "2027-03-13T14:00:00+00:00"),
        ("0 9 * * *", "America/New_York", "2026-11-01T12:00:00+00:00", "2026-11-01T14:00:00+00:00"),
        ("0 9 * * *", "America/New_York", "2026-11-01T15:00:00+00:00", "2026-11-02T14:00:00+00:00"),
        # Weekdays only: Friday evening UTC rolls to Monday.
        ("0 9 * * 1-5", "UTC", "2026-11-20T10:00:00+00:00", "2026-11-23T09:00:00+00:00"),
        ("*/15 * * * *", "UTC", "2026-11-22T12:07:30+00:00", "2026-11-22T12:15:00+00:00"),
        ("0 9 * * 1-5", "America/New_York", "2026-11-22T12:00:00+00:00", "2026-11-23T14:00:00+00:00"),
    ]
    for expression, tz, after, expected in cases:
        got = next_cron(expression, tz, at(after))
        require(got == at(expected), f"test helper wrong for {expression} {tz} after {after}: {got}")
    # The old quarter-hour fixture fired inside the offline-connection check at 19:00Z.
    require(validation_cron("*/15 * * * *", "7/15 * * * *", ("UTC",),
                            at("2026-11-23T18:59:29+00:00")) == "7/15 * * * *",
            "validation fixture did not move away from the 19:00 UTC boundary")
    require(validation_cron("30 8 * * *", "45 8 * * *", ("Asia/Manila", "UTC"),
                            at("2026-11-23T08:29:59+00:00")) == "45 8 * * *",
            "validation fixture was too close to firing after a timezone update")


def exercise_validation(api, users, alice_space, bob_only_space, now):
    alice, bob = users["alice"], users["bob"]
    good = {"name": "validation probe", "prompt": f"{marker('invalid')} must never run",
            "schedule": once(now + timedelta(days=3)), "enabled": False}
    rejected = (400, 422)
    probes = [
        ("a once time in the past", {**good, "schedule": once(now - timedelta(minutes=5))}),
        ("an offset-less once time", {**good, "schedule": {
            "kind": "once", "at": (now + timedelta(days=3)).strftime("%Y-%m-%dT%H:%M:%S")}}),
        ("a non-date once time", {**good, "schedule": {"kind": "once", "at": "tomorrow"}}),
        ("a 6-field cron", {**good, "schedule": cron("0 0 9 * * *")}),
        ("a cron every minute", {**good, "schedule": cron("* * * * *")}),
        ("a cron every 5 minutes", {**good, "schedule": cron("*/5 * * * *")}),
        ("a cron every 10 minutes", {**good, "schedule": cron("*/10 * * * *")}),
        ("a cron with two runs 10 minutes apart", {**good, "schedule": cron("0,10 * * * *")}),
        ("a once time beyond 366 days", {**good, "schedule": once(now + timedelta(days=400))}),
        ("an unknown extra field", {**good, "surprise": True}),
        ("a 4-field cron", {**good, "schedule": cron("0 9 * *")}),
        ("an out-of-range cron", {**good, "schedule": cron("61 9 * * *")}),
        ("an unknown timezone", {**good, "schedule": cron("0 9 * * *", "Mars/Olympus")}),
        ("an empty timezone", {**good, "schedule": cron("0 9 * * *", "")}),
        ("an unknown schedule kind", {**good, "schedule": {"kind": "weekly", "at": rfc3339(now)}}),
        ("a missing schedule", {k: v for k, v in good.items() if k != "schedule"}),
        ("an empty prompt", {**good, "prompt": "   "}),
        ("an empty name", {**good, "name": ""}),
        ("a room without a Space", {**good, "room_id": "lobby"}),
        ("a Space without a room", {**good, "space_id": alice_space}),
    ]
    for label, body in probes:
        api.rejected(alice, "POST", SCHEDULES, body, rejected, f"reject {label}")
    # Another person's Space, and an unknown room in one's own Space, look the same: absent.
    api.rejected(alice, "POST", SCHEDULES,
                 {**good, "space_id": bob_only_space, "room_id": "lobby"}, (404,),
                 "non-member Alice schedules into Bob's Space")
    api.rejected(alice, "POST", SCHEDULES,
                 {**good, "space_id": alice_space, "room_id": "r-doesnotexist"}, (404,),
                 "schedule into an unknown room")
    api.rejected(alice, "POST", SCHEDULES,
                 {**good, "space_id": "s-doesnotexist", "room_id": "lobby"}, (404,),
                 "schedule into an unknown Space")
    for method, path, body in (("GET", SCHEDULES, None), ("POST", SCHEDULES, good)):
        for bad in (None, "not-a-token"):
            api.rejected(bad, method, path, body, (401,), f"unauthenticated {method} {path}")
    # None of the rejected probes may have left a schedule behind.
    require(list_schedules(api, alice) == [] and list_schedules(api, bob) == [],
            "a rejected create left a schedule behind")


def validation_cron(expression, alternate, zones, now):
    """Keep validation-only fixtures away from a firing, even across a minute boundary."""
    safe_after = now + timedelta(minutes=5)
    for candidate in (expression, alternate):
        if all(next_cron(candidate, tz, now) > safe_after for tz in zones):
            return candidate
    raise RuntimeError(f"no safe validation window for cron {expression} in {', '.join(zones)}")


def check_cron_due(view, expression, tz, before, after, label):
    due = next_due(view)
    expected = {next_cron(expression, tz, before), next_cron(expression, tz, after)}
    require(due in expected, f"{label}: next due {due} is not the next {expression} "
                             f"in {tz} (expected {sorted(rfc3339(e) for e in expected)})")
    return due


def exercise_cron(api, users):
    alice = users["alice"]
    # Alternate phases preserve each cadence and zone when its usual slot is
    # close. Manila needs a safe window in both its initial and updated zones.
    cases = [
        ("cron-utc-15min", "*/15 * * * *", "7/15 * * * *", ("UTC",)),
        ("cron-manila", "30 8 * * *", "45 8 * * *", ("Asia/Manila", "UTC")),
        ("cron-newyork-weekdays", "0 9 * * 1-5", "15 9 * * 1-5", ("America/New_York",)),
        ("cron-utc-daily", "15 3 * * *", "30 3 * * *", ("UTC",)),
    ]
    made = []
    for label, expression, alternate, zones in cases:
        expression = validation_cron(expression, alternate, zones, datetime.now(UTC))
        tz = zones[0]
        before = datetime.now(UTC)
        plan, body = create(api, alice, label, cron(expression, tz), enabled=True)
        after = datetime.now(UTC)
        check_view(plan.view, body, label)
        due = check_cron_due(plan.view, expression, tz, before, after, label)
        listed = find_in_list(api, alice, plan.id)
        require(listed is not None and next_due(listed) == due,
                 f"{label}: list disagreed about next due")
        made.append(plan)

    manila = made[1]
    # Update: timezone change recomputes next due; invalid update changes nothing.
    expression = manila.view["schedule"]["expression"]
    before = datetime.now(UTC)
    status, data = api.ok(alice, "PATCH", item(manila.id),
                          {"schedule": cron(expression, "UTC")}, (200,), "PATCH cron timezone")
    after = datetime.now(UTC)
    record("PATCH /v1/schedules/{id}", status, data)
    view = data if isinstance(data, dict) and "id" in data else find_in_list(api, alice, manila.id)
    require(view is not None and view["schedule"] == cron(expression, "UTC"),
            "timezone update was not applied without changing the expression")
    check_cron_due(view, expression, "UTC", before, after, "updated timezone")
    api.rejected(alice, "PATCH", item(manila.id), {"schedule": cron(expression, "Not/AZone")},
                  (400, 422), "PATCH with an invalid timezone")
    api.rejected(alice, "PATCH", item(manila.id), {"schedule": cron("* * * *", "UTC")},
                 (400, 422), "PATCH with an invalid cron")
    unchanged = find_in_list(api, alice, manila.id)
    require(unchanged["schedule"] == view["schedule"] and next_due(unchanged) == next_due(view),
            "a rejected update changed the schedule")

    # Pause / resume of a recurring schedule.
    pause_target = made[0]
    api.ok(alice, "PATCH", item(pause_target.id), {"enabled": False}, (200,), "pause cron")
    paused = find_in_list(api, alice, pause_target.id)
    require(paused["enabled"] is False, "paused recurring schedule still enabled")
    if next_due(paused) is not None:  # next_due() itself requires the field to exist
        note("paused cron schedules keep reporting a next_due time")
    before = datetime.now(UTC)
    api.ok(alice, "PATCH", item(pause_target.id), {"enabled": True}, (200,), "resume cron")
    after = datetime.now(UTC)
    resumed = find_in_list(api, alice, pause_target.id)
    require(resumed["enabled"] is True, "resumed recurring schedule is still paused")
    check_cron_due(resumed, pause_target.view["schedule"]["expression"], "UTC", before, after, "resumed cron")

    # These are validation probes, not part of the later dispatch/replay tests.
    for plan in made:
        require(not history(api, alice, plan.id), f"{plan.label}: validation cron unexpectedly ran")
        api.ok(alice, "DELETE", item(plan.id), expect=(204,), label=f"remove {plan.label}")
    require(list_schedules(api, alice) == [], "recurring validation fixture was left behind")


def exercise_fires(api, users, stubs, state, children, deadline, alice_space, watch):
    """Create the once-schedules, break some of them on purpose, and wait for the rest."""
    alice, bob, carol = users["alice"], users["bob"], users["carol"]
    t0 = datetime.now(UTC)
    plans = {}

    def make(user, label, offset, **kw):
        plan, body = create(api, user, label, once(t0 + timedelta(seconds=offset)), **kw)
        check_view(plan.view, body, label)
        due = next_due(plan.view)
        require(due == parse_time(body["schedule"]["at"]),
                f"{label}: next due for a once schedule was not its `at` time")
        plans[label] = plan
        return plan

    # Staggered so everything shares a few scheduler ticks.
    make(alice, "chat-once", 6)
    make(bob, "bob-chat-once", 6)
    make(carol, "carol-chat-once", 7)
    make(bob, "room-once", 5, space=alice_space, room="lobby")
    make(bob, "room-removed", 16, space=alice_space, room="lobby")
    make(alice, "cancelled", 8)
    make(alice, "paused", 8)
    make(alice, "pause-resume", 12)
    make(alice, "updated", 40)
    last_due = 21
    sentinel = make(alice, "sentinel", last_due)

    # Cancel and pause now; update the late one to a new prompt, moved earlier (+08:00 offset).
    status, _ = api.ok(alice, "DELETE", item(plans["cancelled"].id), expect=(200, 202, 204),
                       label="cancel schedule")
    gone = api.raw(alice, "GET", SCHEDULES)
    require(all(s.get("id") != plans["cancelled"].id for s in wrapped(gone[1], "list", "tasks")),
            "a cancelled schedule is still listed")
    api.rejected(alice, "DELETE", item(plans["cancelled"].id), None, (404,), "cancel twice")
    api.ok(alice, "PATCH", item(plans["paused"].id), {"enabled": False}, (200,), "pause once")
    api.ok(alice, "PATCH", item(plans["pause-resume"].id), {"enabled": False}, (200,), "pause before resume")
    paused = find_in_list(api, alice, plans["pause-resume"].id)
    require(paused["enabled"] is False, "pause did not stick")
    api.ok(alice, "PATCH", item(plans["pause-resume"].id), {"enabled": True}, (200,), "resume once")
    resumed = find_in_list(api, alice, plans["pause-resume"].id)
    require(resumed["enabled"] is True and next_due(resumed) == next_due(plans["pause-resume"].view),
            "resuming a pending once schedule changed its due time")

    old_marker = plans["updated"].marker
    new_marker = marker("updated-new")
    new_prompt = f"{new_marker} the updated scheduled request"
    new_at = (t0 + timedelta(seconds=10)).astimezone(timezone(timedelta(hours=8)))
    patch = {"prompt": new_prompt, "name": "e2e updated renamed",
             "schedule": {"kind": "once", "at": new_at.isoformat(timespec="seconds")}}
    status, data = api.ok(alice, "PATCH", item(plans["updated"].id), patch, (200,), "update once")
    view = data if isinstance(data, dict) and "id" in data else find_in_list(api, alice, plans["updated"].id)
    require(view["prompt"] == new_prompt and view["name"] == "e2e updated renamed"
            and next_due(view) == parse_time(patch["schedule"]["at"]),
            "update did not apply prompt, name and the +08:00 time")
    plans["updated"].marker, plans["updated"].prompt = new_marker, new_prompt

    # Once his first Room run has fired, remove Bob before his second is due: membership is
    # rechecked at execution time, not only at creation.
    poll(lambda: results(room_events(api, alice, alice_space, "lobby"), plans["room-once"].marker),
         "the Room schedule reply", deadline, 30, watch)
    require(datetime.now(UTC) < parse_time(plans["room-removed"].view["schedule"]["at"]) - timedelta(seconds=1),
            "test timing: Bob would not be removed before his second Room task is due")
    api.ok(users["alice"], "DELETE", f"{SPACES}/{quote(alice_space, safe='')}/members",
           {"user_id": "bob"}, (200,), "Alice removes Bob")
    api.rejected(bob, "POST", SCHEDULES,
                 {"name": "after removal", "prompt": f"{marker('late')} no", "enabled": False,
                  "space_id": alice_space, "room_id": "lobby", "schedule": once(t0 + timedelta(days=2))},
                 (404,), "removed Bob schedules into the Space")

    # Isolation: Bob cannot see, change, cancel or read the history of Alice's schedules.
    mine = plans["chat-once"]
    for method, path, body in (
        ("PATCH", item(mine.id), {"enabled": False}),
        ("DELETE", item(mine.id), None),
        ("GET", f"{item(mine.id)}/history", None),
    ):
        api.rejected(bob, method, path, body, (404,), f"Bob {method} Alice's schedule")
        api.rejected(carol, method, path, body, (404,), f"Carol {method} Alice's schedule")
    require(find_in_list(api, alice, mine.id)["enabled"] is True, "another user's call changed Alice's schedule")
    mine_by_user = {
        "alice": {"chat-once", "paused", "pause-resume", "updated", "sentinel"},
        "bob": {"bob-chat-once", "room-once", "room-removed"},
        "carol": {"carol-chat-once"},
    }
    for name, labels in mine_by_user.items():
        ids = {s.get("id") for s in list_schedules(api, users[name])}
        want = {plans[label].id for label in labels}
        require(want <= ids, f"{name}'s list is missing their own schedule")
        others = {p.id for key, p in plans.items() if key not in labels}
        require(not (ids & others), f"{name}'s list contains someone else's schedule")
        require(ids == want, f"{name}'s list has unexpected schedules")

    # Wait for the sentinel: everything due earlier has been processed by then.
    chat_alice = lambda: chat_events(api, alice)
    poll(lambda: results(chat_alice(), sentinel.marker), "the sentinel schedule to fire",
         deadline, last_due + 40, watch)
    fired_by = datetime.now(UTC)
    require(fired_by >= parse_time(sentinel.view["schedule"]["at"]) - timedelta(seconds=1),
            "a once schedule fired before its time")
    # Everything that should fire; poll bounded for the slower ones, then quiet period.
    should_fire = [plans[k] for k in ("chat-once", "pause-resume", "updated", "sentinel")]
    poll(lambda: all(results(chat_alice(), p.marker) for p in should_fire),
         "Alice's scheduled replies", deadline, 30, watch)
    poll(lambda: results(chat_events(api, bob), plans["bob-chat-once"].marker),
         "Bob's private Chat reply", deadline, 30, watch)
    poll(lambda: results(chat_events(api, carol), plans["carol-chat-once"].marker),
         "Carol's private Chat reply", deadline, 30, watch)
    poll(lambda: results(room_events(api, alice, alice_space, "lobby"), plans["room-once"].marker),
         "the Room schedule reply", deadline, 30, watch)
    # Quiet period: a late duplicate or a late "removed" run would show up here.
    quiet = time.monotonic() + 3
    while time.monotonic() < quiet:
        for guard in watch:
            guard()
        time.sleep(0.25)
    return plans, old_marker


def verify_outcomes(api, users, stubs, plans, old_marker, alice_space, label):
    """Counts, transcripts and durable history after any point of the run."""
    alice, bob, carol = users["alice"], users["bob"], users["carol"]
    stub_a, stub_b, stub_f = stubs
    fired = {
        stub_a: ["chat-once", "pause-resume", "updated", "sentinel"],
        stub_b: ["bob-chat-once", "room-once"],
        stub_f: ["carol-chat-once"],
    }
    never = ["paused", "cancelled", "room-removed"]
    for stub, labels in fired.items():
        for key in labels:
            require(stub.count(plans[key].marker) == 1,
                    f"{label}: '{key}' reached {stub.label} {stub.count(plans[key].marker)} times, expected 1")
    for key in never:
        for stub in stubs:
            require(stub.count(plans[key].marker) == 0, f"{label}: '{key}' called {stub.label} but must not")
    for stub in stubs:
        require(stub.count(old_marker) == 0, f"{label}: the pre-update prompt ran on {stub.label}")
    # Charged to the owner's connection only: nothing leaked across stubs, and the
    # failed occurrence of the removed member fell back to nobody (including METRO_MODEL).
    for stub in stubs:
        mine = {plans[k].marker for k in fired[stub]}
        calls, unmarked = stub.snapshot()
        require(set(calls) == mine and unmarked == 0,
                f"{label}: {stub.label} saw calls it should not have ({sorted(calls)}, unmarked {unmarked})")

    events = {"alice": chat_events(api, alice), "bob": chat_events(api, bob), "carol": chat_events(api, carol)}
    for owner, keys in (("alice", fired[stub_a]), ("bob", ["bob-chat-once"]), ("carol", fired[stub_f])):
        for key in keys:
            plan = plans[key]
            require(len(results(events[owner], plan.marker)) == 1,
                    f"{label}: '{key}' posted {len(results(events[owner], plan.marker))} results, expected 1")
            require(len(prompt_messages(events[owner], plan.marker)) <= 1,
                    f"{label}: '{key}' posted its prompt more than once")
        errors = [e for e in events[owner] if e.get("kind") == "error"]
        if owner == "bob":
            # The one expected notice: his Room task was revoked when he left the Space.
            require(len(errors) == 1 and "did not run" in errors[0].get("text", "")
                    and plans["room-removed"].view["name"] in errors[0]["text"],
                    f"{label}: Bob's Chat should hold exactly the revoked-task notice, "
                    f"got {len(errors)} error events")
        else:
            require(not errors, f"{label}: {owner}'s Chat has an error event")
        starts, ends = lifecycle(events[owner], "started"), lifecycle(events[owner], "completed")
        require(len(starts) == len(ends) == len(keys),
                f"{label}: {owner}'s Chat has {len(starts)} started / {len(ends)} completed turns "
                f"for {len(keys)} scheduled runs")
        require(len({e["activity"]["id"] for e in starts}) == len(keys), f"{label}: run ids were reused")
    for key in never:
        require(not prompt_messages(events["alice"], plans[key].marker), f"{label}: '{key}' posted a prompt")
        require(not results(events["alice"], plans[key].marker), f"{label}: '{key}' posted a result")
    # Every Chat is private: nobody's marker appears in someone else's transcript.
    for owner, others in (("alice", ("bob", "carol")), ("bob", ("alice", "carol")), ("carol", ("alice", "bob"))):
        text = json.dumps(events[owner])
        for other in others:
            for key in (k for k, p in plans.items() if p.owner == users[other] and p.target == (None, None)):
                require(plans[key].marker not in text, f"{label}: {other}'s schedule appeared in {owner}'s Chat")

    # Room target: ran once, as a visible agent turn; the removed member's did not.
    room = room_events(api, alice, alice_space, "lobby")
    require(len(results(room, plans["room-once"].marker)) == 1, f"{label}: Room schedule result count was not 1")
    require(len(prompt_messages(room, plans["room-once"].marker)) <= 1, f"{label}: Room prompt posted twice")
    require(not results(room, plans["room-removed"].marker)
            and not prompt_messages(room, plans["room-removed"].marker),
            f"{label}: a schedule of a removed member posted into the Room")
    require(not [e for e in room if e.get("kind") == "agent"
                 and plans["room-removed"].marker in e.get("text", "")], f"{label}: removed member's run answered")

    # Durable history: exactly one entry per run, owner-only.
    for key in sum(fired.values(), []):
        plan = plans[key]
        entries = history(api, plan.owner, plan.id)
        require(len(entries) == 1, f"{label}: history of '{key}' has {len(entries)} entries, expected 1")
        state = entry_state(entries[0])
        require(state == DISPATCHED,
                f"{label}: history of '{key}' did not record a dispatch (state {state!r})")
    paused_entries = history(api, plans["paused"].owner, plans["paused"].id)
    require(not paused_entries, f"{label}: a paused schedule has history entries")
    # AGENTS.md: a failed trigger check "fails visibly": the occurrence is recorded once,
    # not as a success, and is not retried.
    removed_entries = history(api, plans["room-removed"].owner, plans["room-removed"].id)
    require(len(removed_entries) == 1,
            f"{label}: the removed member's occurrence left {len(removed_entries)} history entries, "
            "expected exactly 1 visible failure")
    require(entry_state(removed_entries[0]) == "revoked",
            f"{label}: the removed member's occurrence was not recorded as 'revoked' "
            f"(state {entry_state(removed_entries[0])!r})")
    require(entry_error(removed_entries[0]),
            f"{label}: the removed member's failure has no clear error text in history")
    note(f"removed member's occurrence is recorded as {entry_state(removed_entries[0])!r}")
    require(not [e for e in room_events(api, alice, alice_space, "lobby")
                 if plans["room-removed"].marker in e.get("text", "")],
            f"{label}: the removed member's prompt or failure text reached the Room")
    for owner, stolen in (("bob", plans["chat-once"]), ("carol", plans["bob-chat-once"]), ("alice", plans["carol-chat-once"])):
        api.rejected(users[owner], "GET", f"{item(stolen.id)}/history", None, (404,),
                     f"{owner} reads another owner's history")
    # Prompt text never leaks via history to others (checked by 404 above), and the
    # removed member's schedule is still listed for its owner (not silently deleted)
    # or reported disabled/removed; record which.
    removed = find_in_list(api, bob, plans["room-removed"].id)
    require(removed is not None and removed.get("enabled") is False and next_due(removed) is None,
            f"{label}: a revoked task must stay listed for its owner, paused, with no next run")


def exercise_unusable_default(api, users, stubs, deadline, watch):
    """No silent fallback: a default connection that cannot be used fails the occurrence.

    Dave's default chat connection is a paired-but-offline `device-openai-compatible`
    profile (token issued, connector never started), so it exists and is selected but is
    unusable. His scheduled prompt must not reach any model stub, in particular not the
    METRO_MODEL fallback, and the refusal must be visible: either a failed history entry
    with a clear error, or (if dispatched first) a failed turn with an error in his Chat.
    """
    dave = users["dave"]
    before = [stub.snapshot() for stub in stubs]
    _, conn = api.ok(dave, "POST", "/v1/providers", {
        "name": "offline device", "provider_id": "device-openai-compatible",
        "model": "manual-offline-model", "is_default": True}, (201,), "Dave's device connection")
    require(conn.get("is_default") is True, "Dave's device connection did not become the default")
    cid = quote(conn["id"], safe="")
    api.ok(dave, "POST", f"/v1/providers/{cid}/device-token", {}, (201,), "pair Dave's device")
    _, paired = api.ok(dave, "GET", f"/v1/providers/{cid}/device-token", expect=(200,),
                       label="Dave's device status")
    require(paired.get("paired") is True and paired.get("online") is False,
            "Dave's device was not paired-but-offline")
    plan, body = create(api, dave, "dave-offline", once(datetime.now(UTC) + timedelta(seconds=6)))
    check_view(plan.view, body, "dave-offline")
    poll(lambda: history(api, dave, plan.id), "Dave's refused occurrence to be recorded",
         deadline, 45, watch)
    time.sleep(3)  # quiet period: a retry or late second occurrence would show up here
    entries = history(api, dave, plan.id)
    require(len(entries) == 1, f"Dave's occurrence has {len(entries)} history entries, expected 1")
    state, events = entry_state(entries[0]), chat_events(api, dave)
    require(all(stub.count(plan.marker) == 0 for stub in stubs),
            "Dave's offline prompt reached a model (silent fallback)")
    require_no_new_calls(stubs, before, "unexpected model traffic during the offline-default check")
    require(not results(events, plan.marker), "Dave's offline connection produced an answer")
    errors = [e for e in events if e.get("kind") == "error" and (e.get("text") or "").strip()]
    if state == DISPATCHED:
        require(errors and lifecycle(events, "failed"),
                "dispatched but offline: the transcript shows no error and no failed turn")
        note("unusable default connection: history 'dispatched', failure visible in the Chat transcript")
    else:
        require(state is not None and entry_error(entries[0]),
                f"refused occurrence has no clear state/error in history (state {state!r})")
        note(f"unusable default connection: refused at dispatch, history {state!r} "
             f"({entry_error(entries[0])[:80]!r})")
    return plan


def exercise_restarts(api, users, stubs, server, plans, old_marker, alice_space, deadline, watch):
    alice, bob = users["alice"], users["bob"]
    before = [stub.snapshot() for stub in stubs]
    history_before = {k: history(api, p.owner, p.id) for k, p in plans.items()
                      if k in ("chat-once", "bob-chat-once", "room-once", "carol-chat-once")}

    # 1. Crash (SIGKILL) after everything finished: nothing may be replayed.
    server.stop(kill=True)
    server.start(deadline)
    for guard in watch:
        guard()
    # Re-adding the removed member must not revive the skipped run either.
    api.ok(alice, "POST", f"{SPACES}/{quote(alice_space, safe='')}/members", {"user_id": "bob"},
           (200, 201), "re-add Bob")
    quiet = time.monotonic() + 6
    while time.monotonic() < quiet:
        for guard in watch:
            guard()
        time.sleep(0.25)
    require_no_new_calls(stubs, before, "model calls happened after a crash restart (replay)")
    for key, plan in plans.items():
        if key == "cancelled":
            require(find_in_list(api, plan.owner, plan.id) is None, "a cancelled schedule came back after restart")
        else:
            require(find_in_list(api, plan.owner, plan.id) is not None, f"'{key}' did not survive the restart")
    for key, entries in history_before.items():
        require(history(api, plans[key].owner, plans[key].id) == entries, f"history of '{key}' changed across restart")
    verify_outcomes(api, users, stubs, plans, old_marker, alice_space, "after crash restart")

    # 2. A once schedule that falls due while the server is down: at most one run, ever.
    t = datetime.now(UTC)
    down, body = create(api, alice, "downtime", once(t + timedelta(seconds=5)))
    require(find_in_list(api, alice, down.id) is not None, "downtime schedule was not stored")
    server.stop()
    wait_until = parse_time(down.view["schedule"]["at"]) + timedelta(seconds=2)
    while datetime.now(UTC) < wait_until:
        for guard in watch:
            guard()
        time.sleep(0.25)
    require(stubs[0].count(down.marker) == 0, "a schedule fired while the server was down")
    server.start(deadline)
    # Due seconds ago, well inside the 5-minute catch-up grace: it must run, once.
    poll(lambda: stubs[0].count(down.marker) >= 1, "catch-up after restart", deadline, 20, watch)
    caught_up = True
    quiet = time.monotonic() + 6
    while time.monotonic() < quiet:
        for guard in watch:
            guard()
        time.sleep(0.25)
    runs = stubs[0].count(down.marker)
    require(runs == (1 if caught_up else 0), f"downtime schedule ran {runs} times")
    entries = history(api, alice, down.id)
    posted = results(chat_events(api, alice), down.marker)
    require(len(posted) == runs and len(entries) <= max(runs, 1),
            "downtime schedule recorded an unexpected number of results or history entries")
    if caught_up:
        require(len(entries) == 1 and entry_state(entries[0]) == DISPATCHED,
                "catch-up run was not recorded exactly once as dispatched")
    # 3. A clean restart again: still no replay of anything.
    marks = [stub.snapshot() for stub in stubs]
    server.stop()
    server.start(deadline)
    quiet = time.monotonic() + 5
    while time.monotonic() < quiet:
        for guard in watch:
            guard()
        time.sleep(0.25)
    require_no_new_calls(stubs, marks, "model calls happened after a clean restart (replay)")


# ---------------------------------------------------------------- driver

def schedules(server_binary, timeout):
    selfcheck_cron()
    tokens = {name: secrets.token_hex(24) for name in ("alice", "bob", "carol", "dave")}
    users = Users(tokens)
    api_keys = {name: secrets.token_hex(16) for name in ("alice", "bob", "fallback")}
    with temp_state() as directory, ExitStack() as stack:
        state = Path(directory)
        deadline = time.monotonic() + timeout
        stub_a = serve_stub(stack, ModelStub("alice-stub", "sched/alice", api_keys["alice"]))
        stub_b = serve_stub(stack, ModelStub("bob-stub", "sched/bob", api_keys["bob"]))
        stub_f = serve_stub(stack, ModelStub("fallback-stub", "sched/fallback", api_keys["fallback"]))
        stubs = (stub_a, stub_b, stub_f)
        port = server_port()
        env = clean_env(directory) | {
            "METRO_BIND": f"{HOST}:{port}",
            "METRO_DB": str(state / "schedules.sqlite"),
            "METRO_DEV_USERS": json.dumps(tokens),
            "METRO_MODEL": stub_f.model,
            "METRO_MODEL_BASE_URL": stub_f.base_url,
            "METRO_MODEL_API_KEY": api_keys["fallback"],
            "METRO_PROVIDER_MASTER_KEY": base64.b64encode(secrets.token_bytes(32)).decode("ascii"),
            "METRO_PROVIDER_ALLOWED_BASE_URLS": json.dumps([stub_a.base_url, stub_b.base_url]),
        }
        server = Server(stack, server_binary, state, env, port, stub_f)
        server.start(deadline)
        api = Api(server, deadline)

        def watch_stubs():
            for stub in stubs:
                require(stub.error is None, stub.error)

        def watch_server():
            require(server.alive(), "metro-server exited unexpectedly")

        watch = [watch_stubs, watch_server]

        # Each of Alice and Bob has her/his own default model connection; Carol uses the
        # server fallback. A scheduled run must bill its owner's connection.
        for user, stub, key in (("alice", stub_a, "alice"), ("bob", stub_b, "bob")):
            api.ok(tokens[user], "POST", "/v1/providers", {
                "name": f"{user} stub", "provider_id": "custom", "base_url": stub.base_url,
                "api_key": api_keys[key], "model": stub.model, "is_default": True,
            }, (201,), f"{user} model connection")
        status, data = api.ok(tokens["alice"], "POST", SPACES, {"name": "Schedules Space"}, (201, 200))
        alice_space = data["id"]
        status, data = api.ok(tokens["bob"], "POST", SPACES, {"name": "Bob private Space"}, (201, 200))
        bob_space = data["id"]
        api.ok(tokens["alice"], "POST", f"{SPACES}/{quote(alice_space, safe='')}/members",
               {"user_id": "bob"}, (200, 201), "add Bob to Alice's Space")
        require(list_schedules(api, tokens["alice"]) == [], "a fresh server listed schedules")

        now = datetime.now(UTC)
        before_validation = [stub.snapshot() for stub in stubs]
        exercise_validation(api, users, alice_space, bob_space, now)
        exercise_cron(api, users)
        require_no_new_calls(stubs, before_validation, "schedule validation reached a model")
        watch_stubs()
        plans, old_marker = exercise_fires(api, users, stubs, state, None, deadline, alice_space, watch)
        verify_outcomes(api, users, stubs, plans, old_marker, alice_space, "after firing")
        dave_plan = exercise_unusable_default(api, users, stubs, deadline, watch)
        exercise_restarts(api, users, stubs, server, plans, old_marker, alice_space, deadline, watch)
        require(len(history(api, users["dave"], dave_plan.id)) == 1,
                "the refused occurrence was retried or duplicated across restarts")
        watch_stubs()
        watch_server()
        require((state / "schedules.sqlite").is_file(), "the database was not a persisted file")

    print("recorded response shapes (keys and types only):")
    for label, entry in sorted(SHAPES.items()):
        print(f"  {label} -> HTTP {entry['status']} {json.dumps(entry['shape'], sort_keys=True)}")
    for line in NOTES:
        print(f"note: {line}")
    print("PASS: schedules validated; once schedules fired exactly once with run and result; "
          "recurring cron/IANA accepted with correct next due; pause/resume/update/cancel honored; "
          "Room schedule rechecked membership and did not run after removal; owner-only history; "
          "no replay after crash or clean restart.")


def live_recurring(server_binary, timeout):
    """Opt-in (--live-recurring): the first real firing of a recurring task, on the real clock.

    `<minute> * * * *` is hourly (legal under the 15-minute floor). The minute is the next UTC minute
    boundary with at least 3 s of margin, so the first occurrence is due within ~65 s and the
    following one an hour later, which is never waited for. The model stub holds its answer after
    accepting the request, so history can be read while inference is still running: `dispatched`
    must mean the prompt was accepted, not that the model finished.
    """
    token, key = secrets.token_hex(24), secrets.token_hex(16)
    with temp_state() as directory, ExitStack() as stack:
        state = Path(directory)
        deadline = time.monotonic() + timeout
        stub = serve_stub(stack, ModelStub("held-stub", "sched/held", key))
        stub.gate = threading.Event()
        stack.callback(stub.gate.set)  # runs before the stub shuts down: never leave it held
        port = server_port()
        env = clean_env(directory) | {
            "METRO_BIND": f"{HOST}:{port}",
            "METRO_DB": str(state / "recurring.sqlite"),
            "METRO_DEV_USERS": json.dumps({"alice": token}),
            "METRO_MODEL": stub.model,
            "METRO_MODEL_BASE_URL": stub.base_url,
            "METRO_MODEL_API_KEY": key,
        }
        server = Server(stack, server_binary, state, env, port, stub)
        server.start(deadline)
        api = Api(server, deadline)
        watch = [lambda: require(stub.error is None, stub.error),
                 lambda: require(server.alive(), "metro-server exited unexpectedly")]

        now = datetime.now(UTC)
        target = now.replace(second=0, microsecond=0) + timedelta(minutes=1)
        if target - now < timedelta(seconds=3):
            target += timedelta(minutes=1)
        expression = f"{target.minute} * * * *"
        plan, body = create(api, token, "live-recurring", cron(expression, "UTC"))
        check_view(plan.view, body, "live-recurring")
        require(next_due(plan.view) == target,
                f"first due {next_due(plan.view)} is not the next {expression} boundary {target}")

        # Wait for the real firing; the stub accepts and holds the model request.
        poll(stub.arrived.is_set, "the recurring task's first firing reaching the model",
             deadline, (target - datetime.now(UTC)).total_seconds() + 40, watch)
        require(datetime.now(UTC) >= target - timedelta(seconds=1), "fired before its due minute")
        require(stub.count(plan.marker) == 1, "model was not called exactly once")

        # Inference is still running: history must already say `dispatched`, with no answer yet.
        entries = poll(lambda: history(api, token, plan.id), "a history entry while inference is held",
                       deadline, 15, watch)
        require(len(entries) == 1 and entry_state(entries[0]) == DISPATCHED,
                f"while the model is held, history was {[entry_state(e) for e in entries]}; "
                f"`dispatched` must mean accepted, not finished")
        require(parse_time(entries[0]["due_at"]) == target, "history due_at is not the scheduled minute")
        require(not results(chat_events(api, token), plan.marker),
                "test timing: the answer arrived before it was released")
        held = find_in_list(api, token, plan.id)
        require(held["last_status"] == DISPATCHED and held["enabled"] is True,
                f"task last_status was {held['last_status']!r} while held")
        require(next_due(held) == target + timedelta(hours=1),
                f"next due after the first firing is {next_due(held)}, expected {target + timedelta(hours=1)}")

        # Release inference: the answer lands, nothing else changes.
        stub.gate.set()
        poll(lambda: results(chat_events(api, token), plan.marker), "the released model answer",
             deadline, 30, watch)
        time.sleep(3)  # a duplicate firing would show up now
        events = chat_events(api, token)
        require(len(results(events, plan.marker)) == 1 and stub.count(plan.marker) == 1,
                "the recurring task answered or called the model more than once")
        require(len(lifecycle(events, "started")) == len(lifecycle(events, "completed")) == 1,
                "expected exactly one completed agent turn")
        entries = history(api, token, plan.id)
        require(len(entries) == 1 and entry_state(entries[0]) == DISPATCHED,
                "history changed after the answer was released")
        done = find_in_list(api, token, plan.id)
        require(next_due(done) == target + timedelta(hours=1) and done["last_status"] == DISPATCHED
                and parse_time(done["last_run_at"]) >= target,
                "task state after the first run is wrong (next due must advance one hour)")
        api.ok(token, "DELETE", item(plan.id), expect=(204,), label="clean up the recurring task")
        require(find_in_list(api, token, plan.id) is None, "cleanup left the task behind")
        fired = datetime.now(UTC)
    print(f"PASS: recurring `{expression}` UTC first fired at its minute ({rfc3339(target)}), model "
          f"called once; history `dispatched` while inference was held, one run; next due advanced "
          f"to {rfc3339(target + timedelta(hours=1))}; cleaned up (finished {rfc3339(fired)}).")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--bin-dir", type=Path, default=ROOT / "target" / "debug")
    parser.add_argument("--timeout", type=float, default=150, help="overall deadline in seconds")
    parser.add_argument("--live-recurring", action="store_true",
                        help="instead of the main run, do only the opt-in live check of a real "
                             "recurring firing (up to ~2 minutes)")
    args = parser.parse_args()
    if not math.isfinite(args.timeout) or args.timeout <= 0:
        parser.error("--timeout must be finite and positive")
    binary = args.bin_dir.resolve() / "metro-server"
    if not binary.is_file() or not os.access(binary, os.X_OK):
        parser.error(f"missing executable {binary}; build metro-server or set --bin-dir")
    try:
        (live_recurring if args.live_recurring else schedules)(binary, args.timeout)
    except (KeyError, TypeError, AttributeError, IndexError) as exc:
        print(f"FAIL: unexpected response shape ({type(exc).__name__}: {exc})", file=sys.stderr)
        return 1
    except (RuntimeError, OSError, ValueError, http.client.HTTPException, TimeoutError) as exc:
        print(f"FAIL: {exc}", file=sys.stderr)
        return 1
    except KeyboardInterrupt:
        return 130
    return 0


if __name__ == "__main__":
    signal.signal(signal.SIGTERM, terminate)
    raise SystemExit(main())
