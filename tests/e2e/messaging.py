#!/usr/bin/env python3
"""Reply threads, forwarding and DMs through a real server and loopback models."""

import base64
from contextlib import ExitStack
import json
import secrets
import signal
import time

from intelligence import Vendor
from messaging_fixture import Fixture, activity, role_text
from settings import serve_fixture
from smoke import main, require, temp_state, terminate, text_reply

MARKDOWN = "### café\n\n- **Ready** `x < y`\n- [Metro](https://example.invalid/metro)"
ANCHOR = "**First answer**\n\n```text\nanchor æ\n```"
CLASSIFY = "Could you explain the fixture again?"


class Classifier(Vendor):
    def decide(self, body):
        state = body["state"]
        require(state["message"] == CLASSIFY, "classifier received an explicitly addressed or forwarded turn")
        require(state["space"]["name"] == "Messaging Lab" and state["room"]
                and state["previous"][-1]["text"] == ANCHOR
                and state["previous"][-1]["speaker_kind"] == "agent"
                and len(state["previous"]) <= 8
                and not any(e.get("anchor") for e in state["previous"])
                and isinstance(state["sender"], str) and isinstance(state["people"], list),
                "classifier did not get recent room context")
        question = body["questions"]
        require(set(question) == {"agent"} and question["agent"]["type"] == "choice"
                and set(question["agent"]["criteria"]) == {"agent_1", "no_agent"},
                "classifier should ask one Choice over the agent and No agent")
        return 200, {"model": self.model, "answers": {"agent": {
            "type": "choice", "choice": "no_agent", "confidence": 0.2,
            "probabilities": {"agent_1": 0.4, "no_agent": 0.6},
        }}, "usage": {"input_tokens": 42, "output_tokens": 0}}


def completion(payload):
    latest = role_text(payload, "user")[-1]
    if CLASSIFY in latest:
        return text_reply("METRO_NO_REPLY")
    if "fixture:anchor!" in latest:
        require(MARKDOWN in latest, "model didn't get raw Markdown")
        return text_reply(ANCHOR)
    require(any(f"fixture:{label}!" in latest for label in ("off", "on")),
            "unexpected room model call")
    require(ANCHOR in "\n".join(role_text(payload, "assistant"))
            and "reply thread under one message" in "\n".join(role_text(payload, "system")),
            "reply-thread model history lacked its anchor or thread preamble")
    return text_reply("**Thread answer**")


def quiet(f, user, place, after, models, classifier):
    end = min(f.deadline, time.monotonic() + 0.35)
    while True:
        f.check()
        require(len(f.model.calls) == models and len(f.vendors[1].posts()) == classifier,
                "unexpected model or classifier call")
        require(all(e["kind"] == "message" for e in f.events(user, place, after)),
                "a quiet message caused agent activity")
        if time.monotonic() >= end:
            return
        time.sleep(0.04)


def replies(f, sid, lobby):
    source = f.post("alice", lobby, f"@agent fixture:anchor!\n\n{MARKDOWN}")
    f.phase("bob", lobby, source["id"])
    anchor, = [e for e in f.events("bob", lobby, source["id"]) if e["kind"] == "agent"]
    require(anchor["text"] == ANCHOR, "agent Markdown did not round-trip")
    endpoint = f"{lobby}/events/{anchor['id']}/thread"
    thread = f.call("bob", "POST", endpoint, {}, 201)
    require(thread == f.call("alice", "POST", endpoint, {})
            and thread["anchor_event_id"] == anchor["id"] and thread["parent_room_id"] == "lobby",
            "get-or-create didn't return the same anchored thread")
    path = f"/v1/spaces/{sid}/rooms/{thread['id']}"
    f.call("alice", "POST", lobby + "/messages", {"text": "invalid", "reply_to": anchor["id"]}, 400)

    def classification(enabled):
        prefs = f.call("bob", "PUT", f"/v1/spaces/{sid}/prefs", {"classify_unaddressed": enabled})
        require(prefs["classify_unaddressed"] is enabled, "unexpected classification setting")

    target = anchor
    for label, enabled in (("off", False), ("on", True)):
        classification(enabled)
        # The people mention must not suppress the explicit reply-agent recipient.
        event = f.post("bob", path, f"@alice fixture:{label}!", reply_to=target["id"])
        require(event["reply_to"] == target["id"] and event["audience"] == "mixed"
                and {"kind": "agent", "agent_id": activity(anchor)["agent"]["id"],
                     "addressed_by": "reply"} in event["mentions"],
                "reply didn't explicitly address the correct agent")
        f.phase("alice", path, event["id"])
        target, = [e for e in f.events("alice", path, event["id"]) if e["kind"] == "agent"]
        require(target["text"] == "**Thread answer**", "reply-thread result missing")
    require(not f.vendors[1].posts(), "an explicit reply went through classification")
    f.call("alice", "POST", f"{path}/events/{target['id']}/thread", {}, 409)
    opted = f.post("bob", path, "@alice just a quote", reply_to=target["id"], reply_to_agent=False)
    require(opted["reply_to"] == target["id"] and opted["mentions"] == [{"kind": "user", "id": "alice"}],
            "opting out lost the quote or kept the reply-agent recipient")
    quiet(f, "alice", path, opted["id"], 3, 0)
    parent_events = f.events("alice", lobby, source["id"])
    hydrated = next(e["replies"] for e in parent_events if e["id"] == anchor["id"])
    require(hydrated["room_id"] == thread["id"] and hydrated["count"] == 5
            and hydrated["participant_ids"] == ["bob"] and hydrated["last_at"]
            and not any(e["kind"] == "thread" for e in parent_events),
            "parent lacked the reply summary or got a reply-thread mirror")

    unaddressed = f.post("bob", lobby, CLASSIFY)
    ended = f.phase("bob", lobby, unaddressed["id"])
    require(activity(ended)["outcome"] == "no_reply"
            and not any(e["kind"] in ("agent", "error", "reaction")
                        for e in f.events("bob", lobby, unaddressed["id"])),
            "a completed no-reply turn published a reply or error")
    require(len(f.model.calls) == 4 and len(f.vendors[1].posts()) == 1,
            "expected the laxer classifier to start one no-reply run")
    return source


def directory_and_forward(f, sid, lobby, source):
    f.call("carol", "PATCH", "/v1/me/profile", {"display_name": "Carolina Test"})
    people = f.call("bob", "GET", "/v1/directory?q=tEs")["users"]
    require(len(people) == 1 and people[0]["id"] == "carol"
            and people[0]["display_name"] == "Carolina Test"
            and set(people[0]) <= {"id", "display_name", "avatar_version"}
            and f.call("bob", "GET", "/v1/directory?q=carol")["users"] == people
            and f.call("bob", "GET", "/v1/directory?q=%25")["users"] == [],
            "directory search missed a listed nonmember, leaked fields, or treated % as a wildcard")
    first = f.call("bob", "GET", "/v1/directory?limit=1")
    second = f.call("bob", "GET", f"/v1/directory?limit=1&cursor={first['next_cursor']}")
    require({first["users"][0]["id"], second["users"][0]["id"]} == {"alice", "carol"}
            and not second.get("next_cursor"), "directory pagination included self or an unseen user")
    f.call("bob", "POST", "/v1/dms", {"user_id": "never"}, 404)
    f.call("bob", "POST", "/v1/dms", {"user_id": "bob"}, 400)

    origin = {"kind": "space", "space_id": sid, "room_id": "lobby", "event_id": source["id"]}
    room_id = f.call("alice", "POST", f"/v1/spaces/{sid}/rooms", {"name": "Copies"}, 201)["id"]
    copies = f"/v1/spaces/{sid}/rooms/{room_id}"
    f.call("bob", "PUT", f"/v1/spaces/{sid}/prefs", {"classify_unaddressed": True})
    dest = {"kind": "space", "space_id": sid, "room_id": room_id}
    peer = {"kind": "dm", "user_id": "carol"}
    comment = "@agent **just a forwarding comment**"

    def forward(user, destination, status=201):
        return f.call(user, "POST", "/v1/forward",
                      {"source": origin, "destination": destination, "comment": comment}, status)

    def copied(result, same_space):
        note, copy = result["events"]
        require(result["omitted_attachments"] == 0 and note["text"] == comment
                and copy["text"] == source["text"]
                and all(e["actor_id"] == "bob" and e["kind"] == "message" and not e["mentions"]
                        and e["audience"] == "room" and not e.get("reply_to") for e in (note, copy))
                and copy["forwarded"]["author_name"] == source["actor"]
                and copy["forwarded"]["author_kind"] == "person"
                and copy["forwarded"].get("same_space") == same_space,
                "forward didn't preserve Markdown and appropriate provenance without recipients")
        return [note["id"], copy["id"]]

    ids = copied(forward("bob", dest), {"room_id": "lobby", "event_id": source["id"]})
    quiet(f, "alice", copies, 0, 4, 1)
    require([e["id"] for e in f.events("alice", copies)] == ids,
            "room forward was not persisted in order")

    f.call("carol", "GET", lobby + "/events", status=404)
    ids = copied(forward("bob", peer), None)
    dm, = f.call("carol", "GET", "/v1/dms")["dms"]
    path = "/v1/dms/" + dm["id"]
    require(dm["peer"]["id"] == "bob" and dm["unread"] == 2
            and f.call("bob", "POST", "/v1/dms", {"user_id": "carol"})["id"] == dm["id"]
            and [e["id"] for e in f.events("carol", path)] == ids,
            "forward didn't create one shared DM with both events")
    f.call("alice", "GET", path + "/events", status=404)
    f.call("alice", "POST", path + "/messages", {"text": "outsider"}, 404)
    f.call("alice", "DELETE", path, status=404)
    f.call("carol", "PUT", path + "/read", {"last_read": ids[-1]}, 204)
    require(f.call("carol", "GET", "/v1/dms")["dms"][0]["unread"] == 0, "read cursor didn't clear unread")
    f.call("carol", "DELETE", path, status=204)
    require(f.call("carol", "GET", "/v1/dms")["dms"] == [] and f.events("carol", path) == []
            and [e["id"] for e in f.events("bob", path)] == ids,
            "one-sided hide didn't preserve the other participant's history")
    raw = f.post("bob", path, f"@agent\n\n{MARKDOWN}", 201, reply_to=ids[-1])
    require(raw["reply_to"] == ids[-1] and not raw["mentions"]
            and [e["text"] for e in f.events("carol", path)] == [raw["text"]]
            and f.call("carol", "GET", "/v1/dms")["dms"][0]["unread"] == 1,
            "DM reply didn't restore the conversation with only new history")
    quiet(f, "bob", path, 0, 4, 1)
    for user in ("bob", "carol"):
        f.call(user, "DELETE", path, status=204)
    f.call("bob", "GET", path + "/events", status=404)
    recreated = f.call("carol", "POST", "/v1/dms", {"user_id": "bob"}, 201)["id"]
    require(recreated != dm["id"] and not f.events("bob", "/v1/dms/" + recreated),
            "two-sided hide didn't purge the old DM")

    f.call("bob", "POST", f"/v1/spaces/{sid}/leave", {})
    f.call("bob", "GET", lobby + "/events", status=404)
    f.call("bob", "POST", f"{lobby}/events/{source['id']}/thread", {}, 404)
    require(forward("bob", peer, 404)["code"] == "forward_source_not_found",
            "forward source access wasn't rechecked")
    chat = f.post("bob", "/v1/chat", "Private candidate", save_note=True)
    refused = f.call("bob", "POST", "/v1/forward", {
        "source": {"kind": "chat", "event_id": chat["id"]}, "destination": dest,
    }, 404)
    require(refused["code"] == "forward_destination_not_found", "lost destination access wasn't rechecked")
    quiet(f, "alice", copies, 0, 4, 1)


def exercise(binary, timeout):
    with temp_state() as directory, ExitStack() as stack:
        classifier = serve_fixture(stack, Classifier("systemone", "messaging-classifier"))
        f = Fixture(stack, directory, binary, timeout, completion, {
            "METRO_PROVIDER_MASTER_KEY": base64.b64encode(secrets.token_bytes(32)).decode(),
            "METRO_PROVIDER_ALLOWED_BASE_URLS": json.dumps([classifier.base_url]),
        }, (classifier,))
        profile = f.call("bob", "POST", "/v1/providers", {
            "name": classifier.tag, "provider_id": "custom-systemone", "model": classifier.model,
            "base_url": classifier.base_url, "api_key": classifier.key, "is_default": True,
        }, 201)
        f.call("bob", "PUT", "/v1/model-routing", {"classifier_id": profile["id"]})
        sid, lobby = f.space("alice", "Messaging Lab", "bob")
        source = replies(f, sid, lobby)
        require(next(e["text"] for e in f.events("alice", lobby) if e["id"] == source["id"])
                == source["text"], "stored human Markdown changed")
        directory_and_forward(f, sid, lobby, source)
    print("PASS: reply threads, explicit reply routing, no reply, Markdown, directory, forwards and DMs")


if __name__ == "__main__":
    signal.signal(signal.SIGTERM, terminate)
    raise SystemExit(main(exercise, ("metro-server",), __doc__))
