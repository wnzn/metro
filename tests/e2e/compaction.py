#!/usr/bin/env python3
"""Conversation compaction through the real HTTP API and a scripted model."""

import base64
from contextlib import ExitStack
import json
import re
import secrets
import signal
import threading

from intelligence import Vendor
from messaging_fixture import Fixture, activity, role_text
from settings import serve_fixture
from smoke import main, require, temp_state, terminate, text_reply

CHAT = "/v1/chat"
HISTORY = 40  # Normal metro-server's default, not a test-only override.
AUTO = "automarker"
ROOM = "roommarker"
PERSONAL = "chatmarker"
FAILURE = "failuremarker"
SUMMARIES = {marker: f"**Decision:** {marker}summary" for marker in (AUTO, ROOM, PERSONAL)}
ANSWER = "**Follow-up answer.**"


def note(marker, index):
    return f"{marker} event {index:02d}."


class Embeddings(Vendor):
    def embed(self, body):
        inputs = body["input"]
        if isinstance(inputs, str):
            inputs = [inputs]
        require(not any(summary in text for text in inputs for summary in SUMMARIES.values()),
                "a compaction summary was sent to the semantic index")
        return {"object": "list", "model": self.model, "data": [
            {"object": "embedding", "index": i,
             "embedding": [1.0, 0.0] if "event 01." in text else [0.0, 1.0]}
            for i, text in enumerate(inputs)
        ], "usage": {"prompt_tokens": 1, "total_tokens": 1}}


class Script:
    def __init__(self):
        self.summaries = []
        self.runs = {}
        self.held = threading.Event()
        self.release = threading.Event()

    def __call__(self, payload):
        system = "\n".join(role_text(payload, "system"))
        users = role_text(payload, "user")
        if "Summarize the conversation below" in system:
            text = "\n".join(users)
            marker, = [m for m in (*SUMMARIES, FAILURE) if note(m, 1) in text]
            require(not payload.get("tools") and re.search(
                rf"(alice|bob) \(\1\) at [^\n]+: {re.escape(note(marker, 1))}", text),
                "summary model lacked speaker IDs or was offered tools")
            self.summaries.append(marker)
            if marker in (AUTO, FAILURE):
                require(note(marker, 2) not in text, "auto compacted more than the one excess event")
            else:
                require(note(marker, 6) in text and text.rstrip().endswith(f"Focus: {marker} decisions"),
                        "manual summary missed the earlier messages or requested focus")
            return None if marker == FAILURE else text_reply(SUMMARIES[marker])
        labels = re.findall(r"fixture:([a-z_]+)!", users[-1])
        require(labels, "unexpected conversation model request")
        label = labels[-1]  # Rig can combine consecutive user turns.
        self.runs[label] = (system, users)
        if label.endswith("_held"):
            self.held.set()
            require(self.release.wait(12), "held completion was not released")
        return text_reply("METRO_NO_REPLY" if label == "threshold" else ANSWER)


def seed(f, user, place, marker, count):
    fields = {"save_note": True} if place == CHAT else {}
    return [f.post(user, place, note(marker, i), **fields) for i in range(1, count + 1)]


def ask(f, user, place, label, wait=True):
    fields = {} if place == CHAT else {"invoke_agent": True}
    event = f.post(user, place, f"fixture:{label}!", **fields)
    if wait:
        f.phase(user, place, event["id"])
    return event


def marker(event, key, mode, author, first, last, count):
    meta = event["compaction"]
    require(event["kind"] == "compaction" and event["text"] == SUMMARIES[key]
            and event["actor"] == "system" and not event.get("actor_id")
            and meta["from_event_id"] == first["id"] and meta["through_event_id"] == last["id"]
            and meta["trigger"] == mode and meta["by"] == meta["by_name"] == author
            and meta["events"] == count and meta.get("previous") is None
            and meta.get("focus") == (f"{key} decisions" if mode == "manual" else None),
            "compaction marker didn't record the summary, actor and exact covered range")


def history(script, label, key, through, raw_expected=(), raw_absent=()):
    system, users = script.runs[label]
    require("`compaction` turn is a model-written summary" in system
            and SUMMARIES[key] in users[0] and '"kind":"compaction"' in users[0]
            and f'"through_event_id":{through}' in users[0],
            "later model history didn't start with an untrusted summary and its range")
    whole = "\n".join(users)
    require(all(text in whole for text in raw_expected) and not any(text in whole for text in raw_absent),
            "later model history lost recent events or repeated compacted events")


def searchable(f, user, place, key, original_event, summary):
    def results(query):
        return f.call(user, "POST", "/v1/search/quick",
                      {"query": query, "scope": "all", "mode": "conversations"})["messages"]

    require(not results(f"{key}summary")
            and any(hit["event_id"] == original_event["id"] for hit in results(original_event["text"])),
            "quick search indexed a summary or lost its original message")
    scope = {"scope": "chat"} if place == CHAT else {"scope": "space", "space_id": place.split("/")[3]}
    require(f.call(user, "POST", "/v1/search/index", scope)["done"],
            "semantic index did not finish indexing the fixture")
    result = f.call(user, "POST", "/v1/search", scope | {"query": original_event["text"]})
    require(result["ranking"] == "embeddings" and not result["partial"]
            and any(hit["event_id"] == original_event["id"] for hit in result["results"])
            and not any(hit["event_id"] == summary["id"] for hit in result["results"]),
            "semantic search indexed a summary or lost its original message")


def originals_readable(f, reader, place, original_events):
    rows = {e["id"]: e["text"] for e in f.events(reader, place) if e["kind"] == "message"}
    require(all(rows.get(e["id"]) == e["text"] for e in original_events),
            "compaction changed or removed original transcript messages")


def automatic(f, script):
    _, room = f.space("alice", "Automatic compaction", "bob")
    messages = seed(f, "alice", room, AUTO, HISTORY)
    threshold = ask(f, "alice", room, "threshold")
    require(not script.summaries and note(AUTO, 1) in "\n".join(script.runs["threshold"][1]),
            "compaction triggered at, not above, the event count limit")
    after = ask(f, "alice", room, "auto")
    [summary] = [e for e in f.events("bob", room, after["id"]) if e["kind"] == "compaction"]
    marker(summary, AUTO, "auto", "alice", messages[0], messages[0], 1)
    ask(f, "bob", room, "auto_followup")
    require(script.summaries == [AUTO], "the next run unexpectedly wrote another summary")
    for label in ("auto", "auto_followup"):
        history(script, label, AUTO, messages[0]["id"], (threshold["text"], note(AUTO, HISTORY)),
                (note(AUTO, 1),))
    originals_readable(f, "bob", room, messages)
    searchable(f, "bob", room, AUTO, messages[0], summary)


def manual(f, script, user, reader, place, key, outsider=None):
    endpoint = place + "/compact"
    require(f.call(user, "POST", endpoint, {}, 409)["code"] == "nothing_new",
            "empty transcript allowed manual compaction")
    original = seed(f, user, place, key, 6)
    label = key + "_held"
    script.held.clear()
    script.release.clear()
    held = ask(f, user, place, label, wait=False)
    try:
        f.wait("the in-flight reply to reach the model", lambda: script.held.is_set() or None)
        require(f.call(user, "POST", endpoint, {}, 409)["code"] == "reply_running",
                "manual compaction ran while a reply was active")
    finally:
        script.release.set()
    finished = f.phase(user, place, held["id"])
    answer, = [e for e in f.events(user, place, held["id"]) if e["kind"] == "agent"]
    if outsider:
        f.call(outsider, "POST", endpoint, {"focus": f"{key} decisions"}, 404)
    before = len(script.summaries)
    summary = f.call(user, "POST", endpoint, {"focus": f"{key} decisions"}, 201)
    marker(summary, key, "manual", user, original[0], answer, 8)
    require(f.events(reader, place, finished["id"]) == [summary] and script.summaries[before:] == [key],
            "manual compaction produced an extra reply/run or the wrong summary")
    require(f.call(user, "POST", endpoint, {}, 409)["code"] == "nothing_new",
            "manual compaction counted the summarized messages as new")
    fields = {"save_note": True} if place == CHAT else {}
    recent = f.post(reader, place, f"Fresh {key} note.", **fields)
    next_label = key + "_followup"
    ask(f, reader, place, next_label)
    history(script, next_label, key, answer["id"], (recent["text"],), tuple(e["text"] for e in original))
    originals_readable(f, reader, place, original)
    searchable(f, reader, place, key, original[0], summary)


def failed_auto(f, script):
    _, room = f.space("alice", "Failed automatic compaction", "bob")
    messages = seed(f, "alice", room, FAILURE, HISTORY + 1)
    before = len(script.summaries)
    trigger = ask(f, "alice", room, "auto_failure")
    events = f.events("bob", room, trigger["id"])
    [status] = [e for e in events if e["kind"] == "status"]
    require(script.summaries[before:] == [FAILURE]
            and not any(e["kind"] == "compaction" for e in f.events("bob", room))
            and status["text"] == "Couldn't summarize earlier messages; the agent used the most recent messages."
            and any(e["kind"] == "agent" and e["text"] == ANSWER for e in events),
            "automatic failure stored a summary, exposed a reason or prevented the normal reply")
    system, users = script.runs["auto_failure"]
    text = "\n".join(users)
    require("`compaction` turn is a model-written summary" not in system
            and messages[0]["text"] not in text and messages[1]["text"] in text,
            "failed compaction didn't fall back to the normal history window")
    run = f"/v1/runs/{activity(status)['id']}"
    mine = f.call("alice", "GET", run)["compaction_failure"]["message"]
    require("HTTP 400" in mine
            and not f.call("bob", "GET", run).get("compaction_failure")
            and "compaction_failure" not in str(f.call("bob", "GET", "/v1/runs")),
            "the auto-compaction diagnostic was missing or not initiator-only")
    originals_readable(f, "bob", room, messages)


def exercise(binary, timeout):
    with temp_state() as directory, ExitStack() as stack:
        script = Script()
        embeddings = serve_fixture(stack, Embeddings("embeddings", "compaction-search"))
        f = Fixture(stack, directory, binary, timeout, script, {
            "METRO_PROVIDER_MASTER_KEY": base64.b64encode(secrets.token_bytes(32)).decode(),
            "METRO_PROVIDER_ALLOWED_BASE_URLS": json.dumps([embeddings.base_url]),
        }, (embeddings,))
        stack.callback(script.release.set)
        for user in ("alice", "bob"):
            profile = f.call(user, "POST", "/v1/providers", {
                "name": embeddings.tag, "provider_id": "custom-embeddings", "model": embeddings.model,
                "base_url": embeddings.base_url, "api_key": embeddings.key, "is_default": True,
            }, 201)
            f.call(user, "PUT", "/v1/model-routing", {"embedding_id": profile["id"]})
        automatic(f, script)
        _, room = f.space("alice", "Manual compaction", "bob")
        manual(f, script, "bob", "alice", room, ROOM, outsider="carol")
        manual(f, script, "alice", "alice", CHAT, PERSONAL)
        failed_auto(f, script)
        f.check()
    print("PASS: count-triggered compaction, manual room/Chat, history and search, private auto-failure note")


if __name__ == "__main__":
    signal.signal(signal.SIGTERM, terminate)
    raise SystemExit(main(exercise, ("metro-server",), __doc__))
