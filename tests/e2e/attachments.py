#!/usr/bin/env python3
"""Black-box end-to-end test of library files attached to chat messages.

Run with: python3 tests/e2e/attachments.py [--bin-dir DIR] [--timeout SECONDS]

Needs only the real `metro-server` binary, with dev-token users, a
temporary on-disk SQLite database and a scripted loopback model.

1. Alice's attachment-only Chat send starts a direct agent turn. Saving
   a note in Chat and sending the Space file in a room do not start one.
2. Bob, a member, reads the room event and opens the attached file.
3. The server restarts on the same database; the event and file are still
   there, with the same metadata.
4. Bob is removed from the Space: the room and the file are denied (404).
5. Alice deletes the file: the message stays, and its attachment reads
   {"removed": true}, naming nothing.

Contract: POST .../messages accepts `asset_ids`; events carry
`attachments: [{asset_id, name, mime_type, kind, size_bytes} | {removed: true}]`,
absent when empty. Bytes open via GET /v1/library/assets/{id}/content only.
Tokens are random per run and never printed.
"""

import http.client
import json
import secrets
import signal
import sys
import time
from contextlib import ExitStack
from functools import partial
from pathlib import Path
from urllib.parse import quote

from schedules import ModelStub, serve_stub
from smoke import (
    HOST, SPACES, clean_env, create_space, main, remaining, request, require, server_port,
    start_child, stop_child, temp_state, terminate, wait_for_server,
)

ASSETS = "/v1/library/assets"
CHAT = "/v1/chat"
FIELDS = {"asset_id", "name", "mime_type", "kind", "size_bytes"}


class Server:
    def __init__(self, stack, binary, directory, tokens, deadline, model):
        self.stack, self.binary, self.directory = stack, binary, Path(directory)
        self.tokens, self.deadline = tokens, deadline
        self.env = clean_env(directory)
        self.model = model
        self.child = None
        self.start()

    def start(self):
        self.port = server_port()
        self.child = start_child(self.stack, "attachments server", self.binary, self.directory,
                                 self.env | {
                                     "METRO_BIND": f"{HOST}:{self.port}",
                                     "METRO_DB": str(self.directory / "metro.sqlite"),
                                     "METRO_DEV_USERS": json.dumps(self.tokens),
                                     "METRO_MODEL_BASE_URL": self.model.base_url,
                                     "METRO_MODEL": self.model.model,
                                     "METRO_MODEL_API_KEY": self.model.api_key,
                                 })
        wait_for_server(self.port, [self.child], self.model, self.deadline)

    def restart(self):
        stop_child(self.child[0])
        self.start()

    def call(self, user, method, path, **kw):
        return request(self.port, self.deadline, self.tokens[user], method, path, **kw)

    def raw(self, user, method, path, body=None, mime=None):
        """-> (status, bytes) for uploads and file content."""
        conn = http.client.HTTPConnection(HOST, self.port, timeout=remaining(self.deadline))
        try:
            headers = {"Authorization": f"Bearer {self.tokens[user]}"}
            if mime:
                headers["Content-Type"] = mime
            conn.request(method, path, body, headers)
            with conn.getresponse() as response:
                return response.status, response.read(1024 * 1024)
        finally:
            conn.close()

    def upload(self, user, name, data, space=None):
        query = f"scope=space&space_id={quote(space, safe='')}" if space else "scope=personal"
        status, body = self.raw(user, "POST", f"{ASSETS}?{query}&name={quote(name)}", data,
                                "text/plain")
        require(status == 201, f"upload {name}: unexpected HTTP {status}")
        return json.loads(body)["id"]

    def events(self, user, path, expect=None):
        data = self.call(user, "GET", f"{path}/events?after=0", read_json=True, expect=expect,
                         label=f"{user} reads {path.split('/')[-1]} events")
        return data["events"]


def attached(event):
    items = event.get("attachments", [])
    for item in items:
        require(set(item) == FIELDS or item == {"removed": True},
                f"unexpected attachment fields {sorted(item)}")
    return items


def find(events, eid):
    matches = [e for e in events if e["id"] == eid]
    require(len(matches) == 1, f"event {eid} missing")
    return matches[0]


def journey(server_binary, timeout):
    tokens = {user: secrets.token_hex(24) for user in ("alice", "bob")}
    with temp_state() as directory, ExitStack() as stack:
        model = serve_stub(stack, ModelStub("attachments", "attachments-fixture", secrets.token_hex(24)))
        s = Server(stack, server_binary, directory, tokens, time.monotonic() + timeout, model)
        sid = create_space(s.call, "alice", "Attachments e2e")
        space = f"{SPACES}/{quote(sid, safe='')}"
        room = f"{space}/rooms/lobby"
        s.call("alice", "POST", f"{space}/members", body={"user_id": "bob"}, expect=(201,),
               label="alice adds bob")

        mine = s.upload("alice", "private-note.txt", b"only alice")
        shared_bytes = b"shared plan for the room\n"
        shared = s.upload("alice", "plan.txt", shared_bytes, space=sid)

        # 1. Attach: personal file in Chat, Space file in the room; never crossed.
        chat = s.call("alice", "POST", f"{CHAT}/messages", read_json=True, expect=(202,),
                      body={"text": "", "asset_ids": [mine]}, label="attachment-only Chat send")
        require([a["asset_id"] for a in attached(chat)] == [mine] and chat["text"] == "",
                "Chat send did not echo its attachment")
        require(chat["mentions"] == [{"kind": "agent"}] and chat["audience"] == "agent"
                and chat.get("addressing_note") is None, "attachment-only send was not direct")
        while True:
            events = s.events("alice", CHAT)
            require(model.error is None and not any(e["kind"] == "error" for e in events),
                    "attachment-only Chat turn failed")
            if any(e.get("activity", {}).get("phase") == "completed" for e in events):
                break
            time.sleep(min(0.05, remaining(s.deadline)))
        started = [e["activity"] for e in events if e.get("activity", {}).get("phase") == "started"]
        require(len(started) == 1 and started[0]["route"]["addressed_by"] == "explicit"
                and "pick" not in started[0] and any(e["text"] == "ACK none" for e in events),
                "attachment-only Chat turn lacked a directly attributed reply")
        require(s.events("bob", CHAT) == [], "another user's Chat received the turn")
        msg = s.call("alice", "POST", f"{room}/messages", read_json=True, expect=(202,),
                     body={"text": "see the plan", "asset_ids": [shared]}, label="room send")
        meta = attached(msg)
        require(meta == [{"asset_id": shared, "name": "plan.txt", "mime_type": "text/plain",
                          "kind": "text", "size_bytes": len(shared_bytes)}],
                "room send did not carry authoritative metadata")
        s.call("alice", "POST", f"{room}/messages", expect=(404,),
               body={"text": "x", "asset_ids": [mine]}, label="personal file into a room")
        s.call("alice", "POST", f"{CHAT}/messages", expect=(404,),
               body={"text": "x", "asset_ids": [shared]}, label="Space file into Chat")
        s.call("bob", "POST", f"{room}/messages", expect=(404,),
               body={"text": "x", "asset_ids": [shared]}, label="bob attaches alice's file")
        s.call("alice", "POST", f"{CHAT}/messages", expect=(400,),
               body={"text": "", "invoke_agent": True}, label="Chat send with no text or files")
        note = s.call("alice", "POST", f"{CHAT}/messages", read_json=True, expect=(202,),
                      body={"text": "", "asset_ids": [mine], "save_note": True},
                      label="attachment-only Save note")
        time.sleep(0.5)
        require(s.events("alice", CHAT)[-1]["id"] == note["id"]
                and note["audience"] == "room" and note["mentions"] == []
                and [a["asset_id"] for a in attached(note)] == [mine] and model.total() == 1,
                "Save note or the room send started other work")

        # 2. Another member reads the event and opens the file.
        require(attached(find(s.events("bob", room), msg["id"])) == meta,
                "bob did not see the attachment")
        status, body = s.raw("bob", "GET", f"{ASSETS}/{shared}/content")
        require(status == 200 and body == shared_bytes, "bob could not open the attached file")
        status, _ = s.raw("bob", "GET", f"{ASSETS}/{mine}/content")
        require(status == 404, "bob opened alice's personal file")

        # 3. Restart on the same database.
        s.restart()
        require(attached(find(s.events("bob", room), msg["id"])) == meta,
                "the room attachment did not survive a restart")
        require([a["asset_id"] for a in attached(s.events("alice", CHAT)[0])] == [mine],
                "the Chat attachment did not survive a restart")
        status, body = s.raw("bob", "GET", f"{ASSETS}/{shared}/content")
        require(status == 200 and body == shared_bytes, "file bytes changed after a restart")

        # 4. Revoked membership denies the room and the file.
        s.call("alice", "DELETE", f"{space}/members", body={"user_id": "bob"},
               label="alice removes bob")
        s.call("bob", "GET", f"{room}/events?after=0", expect=(404,),
               label="revoked member cannot read the room transcript")
        status, _ = s.raw("bob", "GET", f"{ASSETS}/{shared}/content")
        require(status == 404, "a removed member still opened the file")

        # 5. Delete: the message stays, with an anonymous tombstone.
        status, _ = s.raw("alice", "DELETE", f"{ASSETS}/{shared}")
        require(status == 204, f"delete: unexpected HTTP {status}")
        kept = find(s.events("alice", room), msg["id"])
        require(kept["text"] == "see the plan" and attached(kept) == [{"removed": True}],
                "deletion did not leave the message with a tombstone")
        require(shared not in json.dumps(kept) and "plan.txt" not in json.dumps(kept),
                "the tombstone named the deleted file")
        s.call("alice", "POST", f"{room}/messages", expect=(404,),
               body={"text": "again", "asset_ids": [shared]}, label="attach a deleted file")
        require(s.child[0].poll() is None, "the server exited")
    print("PASS: attachments delivered in Chat and a room, read by a member, kept across a "
          "restart, denied after removal, and tombstoned on delete; only Chat Send started a turn.")


if __name__ == "__main__":
    signal.signal(signal.SIGTERM, terminate)
    raise SystemExit(main(journey, binaries=("metro-server",), description=__doc__))
