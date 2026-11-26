#!/usr/bin/env python3
"""System One decisions after a message is stored, and deleting messages, through a real server.

A scripted System One takes DELAY seconds to answer. The script measures how
long a classified message takes to come back and to be visible to another
member, before (`defer_decisions` off: the POST waits on System One) and
after (`defer_decisions: true`), then checks deletion permissions, polling
and that a deleted message never reaches the model.
"""

import base64
from contextlib import ExitStack
import json
import secrets
import signal
import time

from intelligence import Vendor
from messaging_fixture import Fixture, activity
from settings import serve_fixture
from smoke import main, require, temp_state, terminate, text_reply

DELAY = 1.0
SECRET = "the vault code is 7319"


class SlowClassifier(Vendor):
    def decide(self, body):
        time.sleep(DELAY)
        options = set(body["questions"]["agent"]["criteria"])
        require("agent_1" in options, "classifier wasn't offered the Space's agent")
        probabilities = {"agent_1": 0.9} | ({"no_agent": 0.1} if "no_agent" in options else {})
        return 200, {"model": self.model, "answers": {"agent": {
            "type": "choice", "choice": "agent_1", "confidence": 0.9, "probabilities": probabilities,
        }}, "usage": {"input_tokens": 1, "output_tokens": 0}}


def completion(payload):
    require(SECRET not in json.dumps(payload), "a deleted message reached the model")
    return text_reply("ok")


def decisions(f, user, lobby, eid, status=200):
    return f.call(user, "POST", f"{lobby}/messages/{eid}/decisions", {}, status)


def latency(f, classifier, sid, lobby):
    f.call("bob", "PUT", f"/v1/spaces/{sid}/prefs", {"classify_unaddressed": True})
    start = time.monotonic()
    before = f.post("bob", lobby, "first question?")
    before_s = time.monotonic() - start
    require(before_s >= DELAY and "decisions_pending" not in before,
            "the synchronous POST didn't wait on System One")
    f.phase("bob", lobby, before["id"])

    start = time.monotonic()
    after = f.post("bob", lobby, "second question?", defer_decisions=True)
    after_s = time.monotonic() - start
    seen = any(e["id"] == after["id"] for e in f.events("alice", lobby, before["id"]))
    seen_s = time.monotonic() - start
    require(after.get("decisions_pending") is True and after["mentions"] == [] and seen,
            "a deferred message wasn't stored and visible at once")
    require(after_s < DELAY / 2, f"the deferred POST still waited ({after_s:.3f}s)")
    decisions(f, "alice", lobby, after["id"], 404)
    decisions(f, "carol", lobby, after["id"], 404)
    decided = decisions(f, "bob", lobby, after["id"])
    require(decided["id"] == after["id"] and decided["classified"] is True
            and decided["addressing_note"] is None, "the sender's decision outcome was wrong")
    decisions(f, "bob", lobby, after["id"], 404)
    started = f.phase("bob", lobby, after["id"], "started")
    require("pick" not in activity(started) and activity(started).get("agent"),
            "the deferred decision didn't start the picked agent, shown plainly")
    f.phase("bob", lobby, after["id"])
    require(len(classifier.posts()) == 2, "a message was classified more than once")
    require(not any("addressing_note" in e or "classified" in e for e in f.events("bob", lobby)),
            "a sender note reached the transcript")
    f.call("bob", "PUT", f"/v1/spaces/{sid}/prefs", {"classify_unaddressed": False})
    print(f"latency to the message with System One answering in {DELAY * 1000:.0f} ms: "
          f"before {before_s * 1000:.0f} ms; after {after_s * 1000:.0f} ms "
          f"(visible to another member after {seen_s * 1000:.0f} ms)")


def deletion(f, classifier, sid, lobby):
    people = {"mentions": [{"kind": "user", "id": "alice"}], "infer_text_mentions": False}
    secret = f.post("bob", lobby, SECRET, **people)
    theirs = f.post("alice", lobby, "alice's own words")
    f.call("alice", "PUT", f"{lobby}/events/{secret['id']}/reactions", {"emoji": "👀"})
    event = lambda eid: f"{lobby}/events/{eid}"
    f.call("carol", "DELETE", event(secret["id"]), status=404)
    f.call("bob", "DELETE", event(theirs["id"]), status=403)
    f.call("bob", "DELETE", f"{lobby}/events/999999999", status=404)
    cursor = f.events("alice", lobby)[-1]["id"]
    calls = len(f.model.calls)
    gone = f.call("bob", "DELETE", event(secret["id"]))
    marker, = gone["events"]
    require(json.loads(marker["text"]) == {"target_event_id": secret["id"], "tombstone": False},
            "unexpected deletion event")
    require([e["id"] for e in f.events("alice", lobby, cursor)] == [marker["id"]],
            "another member's poll didn't carry the deletion")
    require(not any(e["id"] == secret["id"] or (e["kind"] == "reaction" and str(secret["id"]) in e["text"])
                    for e in f.events("alice", lobby)), "the message or its reaction stayed")
    require(len(f.model.calls) == calls and len(classifier.posts()) == 2,
            "deleting called a model")

    ask = f.post("alice", lobby, "@agent what was the code?")
    f.phase("alice", lobby, ask["id"])
    reply = next(e for e in f.events("alice", lobby, ask["id"]) if e["kind"] == "agent")
    f.call("bob", "DELETE", event(reply["id"]), status=403)
    f.call("alice", "DELETE", event(reply["id"]))
    f.call("alice", "DELETE", event(theirs["id"]))

    note = f.post("bob", "/v1/chat", "a private note", save_note=True)
    f.call("alice", "DELETE", f"/v1/chat/events/{note['id']}", status=404)
    f.call("bob", "DELETE", f"/v1/chat/events/{note['id']}")
    require(not any(e["id"] == note["id"] for e in f.events("bob", "/v1/chat")),
            "the Chat note stayed")

    f.call("alice", "GET", "/v1/me")
    dm = "/v1/dms/" + f.call("bob", "POST", "/v1/dms", {"user_id": "alice"}, 201)["id"]
    sent = f.post("bob", dm, "dm words", 201)
    f.call("alice", "DELETE", f"{dm}/events/{sent['id']}", status=403)
    f.call("carol", "DELETE", f"{dm}/events/{sent['id']}", status=404)
    f.call("bob", "DELETE", f"{dm}/events/{sent['id']}")
    require([e["kind"] for e in f.call("alice", "GET", f"{dm}/events?deletions=1")["events"]]
            == ["message_deleted"] and f.events("alice", dm) == [],
            "the DM deletion didn't reach the peer")


def exercise(binary, timeout):
    with temp_state() as directory, ExitStack() as stack:
        classifier = serve_fixture(stack, SlowClassifier("systemone", "lifecycle-classifier"))
        f = Fixture(stack, directory, binary, timeout, completion, {
            "METRO_PROVIDER_MASTER_KEY": base64.b64encode(secrets.token_bytes(32)).decode(),
            "METRO_PROVIDER_ALLOWED_BASE_URLS": json.dumps([classifier.base_url]),
        }, (classifier,))
        profile = f.call("bob", "POST", "/v1/providers", {
            "name": classifier.tag, "provider_id": "custom-systemone", "model": classifier.model,
            "base_url": classifier.base_url, "api_key": classifier.key, "is_default": True,
        }, 201)
        f.call("bob", "PUT", "/v1/model-routing", {"classifier_id": profile["id"]})
        sid, lobby = f.space("alice", "Lifecycle Lab", "bob")
        latency(f, classifier, sid, lobby)
        deletion(f, classifier, sid, lobby)
    print("PASS: deferred System One decisions, message deletion permissions, polling and model history")


if __name__ == "__main__":
    signal.signal(signal.SIGTERM, terminate)
    raise SystemExit(main(exercise, ("metro-server",), __doc__))
