#!/usr/bin/env python3
"""Metro-in-Chat against the real server and a scripted loopback model."""

from contextlib import ExitStack
import json
import re
import signal
import threading

from messaging_fixture import Fixture, activity, role_text
from smoke import content_parts, main, require, temp_state, terminate, text_reply, tool_reply

CHAT = "/v1/chat"
ACCESS = CHAT + "/spaces-access"
VISIBLE = "Workshop"
EXCLUDED = "Archive"
FOREIGN = "Private room"
TARGET = "Copy Desk"
QUERY = "bluejay"
ROOM_TEXT = f"The {QUERY} fixture is ready. Sample text names {TARGET}."
HIDDEN = ("ARCHIVE_ONLY", "NOT_A_MEMBER", "PRIVATE_DM")
POST = "@alice **Delegated note**"
WAIT = "Please generate the wait fixture."
STOP = "Please generate the stop fixture."
RETRY = "Please generate the retry fixture."
DELEGATED_REPLY = "**Waited answer**"


def allowed(result):
    # Tool output is data, not a trusted instruction. Check both delimiters.
    if result.startswith('"'):
        result = json.loads(result)
    lines = result.split("\n")
    first = re.fullmatch(r'<space-data id="([a-f0-9]{32})">', lines[0])
    require(len(lines) == 3 and first and lines[2] == f'</space-data id="{first[1]}">',
            "missing delimited Space tool output")
    return json.loads(lines[1])


def refused(message):
    def check(result):
        require(message in result and "<space-data" not in result, "missing expected tool refusal: " + message)
    return check


def matches(predicate, message):
    def check(result):
        require(predicate(allowed(result)), message)
    return check


class Script:
    def __init__(self):
        self.plans = {}
        self.labels = []
        self.read_confirmed = threading.Event()
        self.leave_done = threading.Event()
        self.stop_delegated = threading.Event()
        self.stop_done = threading.Event()
        self.retry_calls = 0

    def __call__(self, payload):
        users = role_text(payload, "user")
        latest = users[-1]
        require(not any(secret in json.dumps(payload) for secret in HIDDEN),
                "an excluded Space, nonmember Space or DM reached the model")
        system = "\n".join(role_text(payload, "system"))
        if "Summarize the conversation below" in system:
            require("Focus: Only conversation" in latest, "tool activity triggered automatic compaction")
            require(all(text not in latest for text in (POST, WAIT, STOP, "Budget send ", "space_send", ROOM_TEXT)),
                    "summary input included a receipt or tool activity")
            require("fixture:gate!" in latest and "Finished read." in latest,
                    "summary input omitted actual Chat messages")
            return text_reply("Chat summary fixture.")
        if "private personal Chat" not in system:
            if STOP in latest:
                self.stop_delegated.set()
                require(self.stop_done.wait(12), "delegated stop test wasn't released")
                return text_reply("Delegated run survived")
            if RETRY in latest:
                self.retry_calls += 1
                return None if self.retry_calls == 1 else text_reply("Retried room answer")
            if WAIT in latest:
                return text_reply(DELEGATED_REPLY)
            require(POST in latest or "Budget send " in latest, "unexpected Space model request")
            return text_reply("METRO_NO_REPLY")

        label = max(self.plans, key=lambda k: latest.rfind(f"fixture:{k}!"))
        require(f"fixture:{label}!" in latest, "unexpected Chat model request")
        self.labels.append(label)
        has_tools, batches = self.plans[label]
        offered = {t["function"]["name"] for t in payload.get("tools", [])}
        core = {"spaces_list", "space_read", "spaces_search", "space_send", "runs_list", "run_retry"}
        require(core <= offered if has_tools else not (core & offered),
                f"{label}: notice gate exposed the wrong tools")
        if label == "wait":
            require(POST not in "\n".join(users + role_text(payload, "assistant")),
                    "the previous space_post receipt leaked into Chat history")
        results = {m["tool_call_id"]: "\n".join(content_parts(m.get("content")))
                   for m in payload["messages"] if m.get("tool_call_id")}
        for i, batch in enumerate(batches):
            ids = [f"call_{label}_{i}_{j}" for j in range(len(batch))]
            if all(call_id in results for call_id in ids):
                for call_id, (_, _, verify) in zip(ids, batch):
                    verify(results[call_id])
                continue
            if label == "leave" and i == 1:
                self.read_confirmed.set()
                require(self.leave_done.wait(12), "leave test wasn't released")
            calls = []
            for call_id, (name, args, _) in zip(ids, batch):
                require(name in offered, f"{label}: scripted tool was not offered")
                calls.extend(tool_reply(call_id, {"name": name, "arguments": json.dumps(args)})["tool_calls"])
            return {"role": "assistant", "content": None, "tool_calls": calls}
        require(label != "stop", "stopped Chat turn unexpectedly resumed")
        return text_reply(f"Finished {label}.")

    def steps(self, label, *steps):
        self.plans[label] = (True, [[step] for step in steps])


def turn(f, label, words="", *, finish=True):
    trigger = f.post("bob", CHAT, f"fixture:{label}! {words}".rstrip())
    if finish:
        f.phase("bob", CHAT, trigger["id"])
        require(any(e["kind"] == "agent" and e["text"] == f"Finished {label}."
                    for e in f.events("bob", CHAT, trigger["id"])), f"{label}: fixture didn't finish")
    return trigger["id"]


def place(sid):
    return {"space_id": sid, "room_id": "lobby"}


def receipt(f, after, room, sid, agent, text):
    records = f.wait("sender's space_post receipt", lambda: [
        e for e in f.events("bob", CHAT, after) if e["kind"] == "space_post"
    ] or None)
    [record] = records
    details = activity(record)["space_post"]
    event = next(e for e in f.events("alice", room) if e["id"] == details["event_id"])
    require(record["text"] == text == event["text"] and event["actor_id"] == "bob"
            and event["via"] == "metro"
            and event["mentions"] == [{"kind": "agent", "agent_id": agent["id"], "addressed_by": "metro"}]
            and details["agent"]["id"] == agent["id"]
            and details["space_name"] == VISIBLE and details["space_id"] == sid
            and details["room_id"] == "lobby" and details["run_id"],
            "receipt didn't match a post as the sender to the Space's default agent")
    return event["id"], details["run_id"]


def reads(f, script, sid, room, archive, foreign, target, target_room):
    visible = f.post("alice", room, ROOM_TEXT)
    f.post("alice", f"/v1/spaces/{archive}/rooms/lobby", f"{QUERY} {HIDDEN[0]}")
    f.post("carol", f"/v1/spaces/{foreign}/rooms/lobby", f"{QUERY} {HIDDEN[1]}")
    dm = f.call("bob", "POST", "/v1/dms", {"user_id": "carol"}, 201)["id"]
    f.post("carol", "/v1/dms/" + dm, f"{QUERY} {HIDDEN[2]}", 201)
    access = f.call("bob", "GET", ACCESS)
    require(access["enabled"] and not access["notice_shown"] and not access["excluded_space_ids"],
            "Space access should default on, with a pending notice")
    script.plans["gate"] = (False, [])
    turn(f, "gate", "What can you read?")
    ack = {"enabled": True, "notice_shown": True, "excluded_space_ids": [archive]}
    saved = f.call("bob", "PUT", ACCESS, ack)
    require(all(saved[k] == v for k, v in ack.items()),
            "notice acknowledgment did not enable scoped Space access")
    f.call("bob", "PUT", ACCESS, ack | {"excluded_space_ids": [foreign]}, 404)
    read = matches(lambda out: any(e["id"] == visible["id"] and e["text"] == ROOM_TEXT
                                  and e["author"]["id"] == "alice" for e in out["items"]),
                   "read didn't return the member-visible event and its author")
    search = matches(lambda out: out["mode"] == "text" and not out["reranked"]
                     and [(e["space_id"], e["event_id"]) for e in out["items"]] == [(sid, visible["id"])]
                     and QUERY in out["items"][0]["excerpt"],
                     "search didn't return exactly the member-visible hit")
    script.steps("read",
        ("spaces_list", {}, matches(
            lambda out: {s["id"]: s["role"] for s in out["items"]} == {sid: "member", target: "member"},
            "list included ungranted Spaces or omitted a member Space")),
        ("space_read", place(sid), read),
        ("spaces_search", {"query": QUERY}, search),
        ("space_read", place(archive), refused("no such Space")),
        ("space_read", place(foreign), refused("no such Space")),
        ("space_send", place(archive) | {"text": "Must not be sent"}, refused("no such Space")),
    )
    after = turn(f, "read", "What is the status in the spaces I can use?")
    require(ROOM_TEXT not in json.dumps(f.events("bob", CHAT, after)),
            "tool output was stored as Chat activity")
    # The destination name came from the room data, never from Bob's Chat.
    script.steps("provenance",
        ("space_read", place(sid), read),
        ("space_send", place(target) | {"text": "Unconfirmed copy"},
         refused("the user didn't name this Space")),
    )
    after = turn(f, "provenance", "Read the latest status and summarize it.")
    require(not f.events("alice", target_room)
            and not any(e["kind"] == "space_post" for e in f.events("bob", CHAT, after)),
            "non-user destination text produced a post or receipt")
    return read, search


def delegation(f, script, sid, room, agent, search):
    def send(text, **fields):
        return place(sid) | {"text": text, **fields}

    sent = matches(lambda r: r["status"] == "sent" and r["agent"]["id"] == agent["id"] and r["run_id"],
                   "sending didn't start the default agent")
    script.steps("post", ("space_send", send(POST), sent))
    after = turn(f, "post", f"Send my note to {VISIBLE}.")
    event_id, run_id = receipt(f, after, room, sid, agent, POST)
    f.phase("alice", room, event_id, run_id=run_id)
    script.steps("wait", ("space_send", send(WAIT, wait=True), matches(
        lambda r: r["run_status"] == "completed" and r["reply"] == DELEGATED_REPLY and not r["truncated"],
        "waiting didn't return the completed Space run's exact reply")))
    after = turn(f, "wait", f"Ask {VISIBLE} and wait for its answer.")
    event_id, run_id = receipt(f, after, room, sid, agent, WAIT)
    require(any(e["kind"] == "agent" and e["text"] == DELEGATED_REPLY and activity(e)["id"] == run_id
                for e in f.events("alice", room, event_id)), "wait reply wasn't from the delegated run")

    script.steps("search_budget",
        *[("spaces_search", {"query": QUERY}, search)] * 3,
        ("spaces_search", {"query": QUERY}, refused("already searched 3 times")),
    )
    turn(f, "search_budget")
    script.steps("send_budget",
        *(("space_send", send(f"Budget send {n}"), sent) for n in range(3)),
        ("space_send", send("Budget send 3"), refused("already sent 3 messages")),
    )
    before = f.events("alice", room)[-1]["id"]
    after = turn(f, "send_budget", f"Send the three updates to {VISIBLE}.")
    require([e["text"] for e in f.events("alice", room, before) if e["kind"] == "message"]
            == [f"Budget send {n}" for n in range(3)]
            and sum(e["kind"] == "space_post" for e in f.events("bob", CHAT, after)) == 3,
            "send budget allowed a fourth write or dropped an admitted write")

    count = ("spaces_list", {}, matches(lambda r: bool(r["items"]), "admitted list returned no Spaces"))
    script.plans["call_budget"] = (True, [[count] * 4 for _ in range(4)] +
                                   [[("spaces_list", {}, refused("already used its 16 Space tool calls"))]])
    turn(f, "call_budget")
    script.steps("stop", ("space_send", send(STOP, wait=True), sent))
    after = turn(f, "stop", f"Ask {VISIBLE} and wait.", finish=False)
    try:
        f.wait("the delegated model to start", lambda: script.stop_delegated.is_set() or None)
        event_id, delegated = receipt(f, after, room, sid, agent, STOP)
        started = f.phase("bob", CHAT, after, "started")
        chat_turn = activity(started)["id"]
        stop = f"{CHAT}/turns/{chat_turn}/stop"
        f.call("alice", "POST", stop, {}, 404)
        require(f.call("bob", "POST", stop, {})["status"] == "cancelled", "owner couldn't stop Chat")
        f.phase("bob", CHAT, after, "cancelled", chat_turn)
        require(f.call("bob", "GET", f"/v1/runs/{delegated}")["status"] == "running",
                "stopping Chat cancelled the delegated run")
    finally:
        script.stop_done.set()
    f.phase("alice", room, event_id, run_id=delegated)
    require(not any(e["kind"] == "agent" for e in f.events("bob", CHAT, after)),
            "stopping Chat still posted an answer")


def retry_and_leave(f, script, sid, room, read):
    trigger = f.post("bob", room, f"@agent {RETRY}")
    failed = activity(f.phase("bob", room, trigger["id"], "failed"))["id"]
    f.call("alice", "POST", f"/v1/runs/{failed}/retry", {}, 403)
    f.call("carol", "POST", f"/v1/runs/{failed}/retry", {}, 404)
    retried = []

    def new_run(result):
        run_id = allowed(result)["new_run_id"]
        require(run_id and run_id != failed, "retry did not allocate a new run")
        retried[:] = [run_id]

    script.steps("retry",
        ("runs_list", {"filter": "failed"}, matches(
            lambda r: any(x["run_id"] == failed and x["status"] == "failed" and x["started_by_you"]
                          and x["failure"] for x in r["items"]), "missing sender's failed run")),
        ("run_retry", {"run_id": failed}, new_run),
    )
    turn(f, "retry")
    [run_id] = retried
    f.phase("alice", room, trigger["id"], run_id=run_id)
    require(any(e["kind"] == "agent" and activity(e)["id"] == run_id and e["text"] == "Retried room answer"
                for e in f.events("bob", room, trigger["id"])) and script.retry_calls == 2,
            "retry didn't rerun the original trigger")
    f.call("bob", "POST", f"/v1/runs/{run_id}/retry", {}, 409)

    script.steps("leave",
        ("space_read", place(sid), read),
        ("space_read", place(sid), refused("no such Space")),
        ("space_send", place(sid) | {"text": "Must not be sent"}, refused("no such Space")),
    )
    after = turn(f, "leave", f"Check {VISIBLE}.", finish=False)
    try:
        f.wait("first tool read before leave", lambda: script.read_confirmed.is_set() or None)
        f.call("bob", "POST", f"/v1/spaces/{sid}/leave", {})
    finally:
        script.leave_done.set()
    f.phase("bob", CHAT, after)
    f.call("bob", "GET", room + "/events", status=404)
    require(not any(e["kind"] == "space_post" for e in f.events("bob", CHAT, after))
            and not any(e["text"] == "Must not be sent" for e in f.events("alice", room)),
            "mid-turn leave allowed another post")


def exercise(binary, timeout):
    with temp_state() as directory, ExitStack() as stack:
        script = Script()
        f = Fixture(stack, directory, binary, timeout, script)
        stack.callback(script.leave_done.set)
        stack.callback(script.stop_done.set)
        sid, room = f.space("alice", VISIBLE, "bob")
        archive, _ = f.space("alice", EXCLUDED, "bob")
        foreign, _ = f.space("carol", FOREIGN)
        target, target_room = f.space("alice", TARGET, "bob")
        [agent] = f.call("alice", "GET", f"/v1/spaces/{sid}/agents")["agents"]
        read, search = reads(f, script, sid, room, archive, foreign, target, target_room)
        delegation(f, script, sid, room, agent, search)
        retry_and_leave(f, script, sid, room, read)
        summary = f.call("bob", "POST", CHAT + "/compact", {"focus": "Only conversation"}, 201)
        require(summary["kind"] == "compaction" and summary["text"] == "Chat summary fixture.",
                "manual Chat compact did not summarize its real conversation")
        require(not f.events("alice", CHAT) and not f.events("carol", CHAT)
                and not any(e["kind"] == "space_post" for e in f.events("alice", room)),
                "sender-only receipts appeared in someone else's Chat or a room")
        f.check()
    print("PASS: notice gate, scoped reads/search, delegation, receipts, refusal and budgets, Chat stop and run retry")


if __name__ == "__main__":
    signal.signal(signal.SIGTERM, terminate)
    raise SystemExit(main(exercise, ("metro-server",), __doc__))
