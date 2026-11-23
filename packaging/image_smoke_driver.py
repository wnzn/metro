#!/usr/bin/env python3
"""In-network client for packaging/image_smoke.py. Standard library only.

Runs in a throwaway container on the smoke's internal Docker network and talks
to the server container by name. It reads the synthetic user tokens from
/secrets/users.json, writes issued runner tokens to mode-600 files and prints
only non-secret JSON (Space ids) on stdout.

  setup  health, 401s, Spaces, a dedicated token for Bob's Space and a
         personal registered runner attached to two of Alice's Spaces
  demo   one deterministic Demo per Space; each must be claimed by its runner
         and list exactly that Space's marker file
"""

import http.client
import json
import os
import sys
import time
from urllib.parse import quote

SERVER = os.environ.get("SMOKE_SERVER", "server")
PORT = 8787
DEADLINE = time.monotonic() + float(os.environ.get("SMOKE_TIMEOUT", "90"))
MAX_BODY = 1024 * 1024


class Fail(Exception):
    pass


def require(cond, message):
    if not cond:
        raise Fail(message)


def remaining():
    left = DEADLINE - time.monotonic()
    require(left > 0, "driver deadline expired")
    return left


def call(token, method, path, body=None, expect=None, read=False, label=None):
    label = label or f"{method} {path.split('?')[0]}"
    conn = http.client.HTTPConnection(SERVER, PORT, timeout=min(10, remaining()))
    try:
        headers = {"Accept": "application/json"}
        if token is not None:
            headers["Authorization"] = f"Bearer {token}"
        data = None
        if body is not None:
            headers["Content-Type"] = "application/json"
            data = json.dumps(body)
        conn.request(method, path, data, headers)
        resp = conn.getresponse()
        raw = resp.read(MAX_BODY + 1)
        ok = 200 <= resp.status < 300 if expect is None else resp.status in expect
        # Never echo response bodies: an error body could quote a credential.
        require(ok, f"{label}: unexpected HTTP {resp.status}")
        require(len(raw) <= MAX_BODY, f"{label}: oversized response")
        if read:
            try:
                return json.loads(raw)
            except ValueError:
                raise Fail(f"{label}: invalid JSON") from None
        return resp.status
    except (OSError, http.client.HTTPException) as exc:
        raise Fail(f"{label}: {type(exc).__name__}") from None
    finally:
        conn.close()


def wait_healthy():
    while True:
        try:
            if call(None, "GET", "/healthz", expect=(200,)) == 200:
                return
        except Fail:
            pass
        remaining()
        time.sleep(0.25)


def write_secret(path, value):
    fd = os.open(path, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
    with os.fdopen(fd, "w") as f:
        f.write(value + "\n")


def token_of(data, label):
    token = data.get("token") if isinstance(data, dict) else None
    require(isinstance(token, str) and token.isascii() and token.isprintable() and token
            and not any(c.isspace() for c in token), f"{label}: invalid token response")
    return token


def space_path(sid):
    return f"/v1/spaces/{quote(sid, safe='')}"


def create_space(user, name):
    data = call(user, "POST", "/v1/spaces", body={"name": name}, read=True, label="create Space")
    require(isinstance(data, dict) and isinstance(data.get("id"), str) and data["id"]
            and data.get("role") == "owner", "invalid create Space response")
    return data["id"]


def setup(users):
    alice, bob = users["alice"], users["bob"]
    wait_healthy()
    call(None, "GET", "/v1/spaces", expect=(401,), label="anonymous GET /v1/spaces")
    call("metro-image-smoke-not-a-token", "GET", "/v1/spaces", expect=(401,),
         label="unknown bearer GET /v1/spaces")
    call("metro-image-smoke-not-a-token", "POST", "/v1/runner/claim", body={}, expect=(401,),
         label="unknown runner token claims")

    a = create_space(bob, "Image smoke dedicated")
    b = create_space(alice, "Image smoke registered one")
    c = create_space(alice, "Image smoke registered two")
    require(len({a, b, c}) == 3, "two Spaces share an id")

    dedicated = token_of(call(bob, "POST", f"{space_path(a)}/runner-token", body={}, read=True,
                              label="issue dedicated runner token"), "dedicated token")
    created = call(alice, "POST", "/v1/runners", body={"name": "Image smoke runner", "visibility": "personal"},
                   expect=(201,), read=True, label="create personal runner")
    registered = token_of(created, "registered runner")
    runner_id = created["runner"]["id"]
    for sid in (b, c):
        call(alice, "PUT", f"{space_path(sid)}/runner", body={"runner_id": runner_id},
             label="attach personal runner")
    # Access is per Space: outsiders get 404 and cannot attach or read.
    call(alice, "GET", f"{space_path(a)}/rooms/lobby/events?after=0", expect=(404,),
         label="outsider reads the dedicated Space")
    call(bob, "GET", f"{space_path(b)}/rooms/lobby/events?after=0", expect=(404,),
         label="outsider reads a registered Space")
    call(bob, "PUT", f"{space_path(a)}/runner", body={"runner_id": runner_id}, expect=(404,),
         label="outsider attaches someone else's personal runner")
    require(len({dedicated, registered} | set(users.values())) == 4, "credentials were reused")
    write_secret("/out/dedicated/token", dedicated)
    write_secret("/out/registered/token", registered)
    print(json.dumps({"dedicated": a, "registered": [b, c]}))


def demo(users, plan):
    alice, bob = users["alice"], users["bob"]
    jobs = [(bob, plan["dedicated"], plan["markers"][plan["dedicated"]])]
    jobs += [(alice, sid, plan["markers"][sid]) for sid in plan["registered"]]
    pending = []
    for user, sid, marker in jobs:
        room = f"{space_path(sid)}/rooms/lobby"
        event = call(user, "POST", f"{room}/demo", body={}, read=True, label="start Demo")
        require(isinstance(event, dict) and type(event.get("id")) is int, "Demo returned no event id")
        pending.append((user, room, sid, marker, event["id"]))
    done = []
    while pending:
        for job in pending[:]:
            user, room, sid, marker, after = job
            data = call(user, "GET", f"{room}/events?after={after}", read=True, label="read events")
            for event in data["events"]:
                require(event["kind"] != "error", "a Demo emitted a room error")
                if event["kind"] != "tool_result":
                    continue
                header, _, output = event["text"].partition("\n")
                require(event["actor"] == "runner" and header == "workspace.list ok",
                        "a Demo did not get a successful runner listing")
                listing = json.loads(output)
                names = [entry["name"] for entry in listing["entries"]]
                # No marker: the runner must create the Space root itself, empty.
                require(names == ([marker] if marker else []),
                        "a runner listed something other than its own Space's files")
                pending.remove(job)
                done.append(sid)
                break
        if pending:
            remaining()
            time.sleep(0.2)
    print(json.dumps({"listed": done}))


def main():
    with open("/secrets/users.json") as f:
        users = json.load(f)
    try:
        if sys.argv[1] == "setup":
            setup(users)
        elif sys.argv[1] == "demo":
            demo(users, json.loads(sys.argv[2]))
        else:
            raise Fail("unknown phase")
    except (Fail, KeyError, TypeError, ValueError) as exc:
        print(f"FAIL: {exc if isinstance(exc, Fail) else type(exc).__name__}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
