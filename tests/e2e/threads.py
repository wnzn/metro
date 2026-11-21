#!/usr/bin/env python3
"""Local Metro Threads and personal Chat smoke test; only metro-server is required."""

from contextlib import ExitStack
from functools import partial
import json
from pathlib import Path
import secrets
import signal
import time
from urllib.parse import quote
import uuid

from smoke import (
    OpenAIModel, SPACES, check_processes, clean_env, content_parts, create_space, main, read_events,
    remaining, request, require, start_server, temp_state, terminate, text_reply,
    tool_reply,
)

REACTION_ONLY = "METRO_REACTION_ONLY"


def tagged(label):
    return f"{label} {uuid.uuid4().hex}"


class ThreadModel(OpenAIModel):
    def __init__(self):
        super().__init__()
        self.create_prompt = tagged("Parent asks to create a child")
        self.send_prompt = tagged("Parent asks to follow up with the child")
        self.thread_name = tagged("Smoke child")
        self.task = tagged("Initial child task")
        self.message = tagged("Follow-up child task")
        self.first_answer = tagged("Initial child answer")
        self.next_answer = tagged("Follow-up child answer")
        self.create_ack = tagged("Parent create acknowledgment")
        self.send_ack = tagged("Parent follow-up acknowledgment")
        self.bob_prompt = tagged("Bob asks in his private Space")
        self.bob_answer = tagged("Bob's private Space answer")
        self.reaction_prompt = tagged("@agent please acknowledge only")
        self.thread_id = None
        self.create_result = None
        self.send_result = None
        self.create_call = f"call_{uuid.uuid4().hex}"
        self.send_call = f"call_{uuid.uuid4().hex}"
        self.reaction_call = f"call_{uuid.uuid4().hex}"
        self.seen = set()

    def once(self, step):
        require(step not in self.seen, f"duplicate model step: {step}")
        self.seen.add(step)

    def tool_step(self, request, tool, args, call_id, answer):
        results = [
            text for msg in request["messages"]
            if msg["role"] == "tool" and msg.get("tool_call_id") == call_id
            for text in content_parts(msg.get("content"))
        ]
        if results:
            self.once(f"{tool} result")
            if tool in {"thread_create", "thread_send"}:
                # Rig may wrap a text tool reply as a JSON string.
                require(len(results) == 1, f"{tool} must return one tool result")
                parsed = json.loads(results[0])
                if isinstance(parsed, str):
                    parsed = json.loads(parsed)
                require(isinstance(parsed, dict) and isinstance(parsed.get("thread_id"), str)
                        and isinstance(parsed.get("run_id"), str), f"invalid {tool} result")
                if tool == "thread_create":
                    self.create_result = parsed
                else:
                    self.send_result = parsed
            return text_reply(answer)
        self.once(f"{tool} call")
        matching = [
            item["function"] for item in request.get("tools", [])
            if item.get("type") == "function" and item["function"]["name"] == tool
        ]
        require(len(matching) == 1, f"model was not offered exactly one {tool} function")
        schema = matching[0]["parameters"]
        require(set(args) == set(schema.get("required", []))
                and all(schema["properties"][key].get("type") == "string" for key in args),
                f"unexpected advertised schema for {tool}")
        return tool_reply(call_id, {"name": matching[0]["name"], "arguments": json.dumps(args)})

    def respond(self, request):
        user_messages = [msg.get("content") for msg in request["messages"] if msg["role"] == "user"]
        require(user_messages, "model request did not contain a user message")
        # History contains earlier tags; only the newest user input identifies the run.
        latest = "\n".join(content_parts(user_messages[-1]))
        if self.create_prompt in latest:
            return self.tool_step(request, "thread_create",
                                  {"name": self.thread_name, "task": self.task},
                                  self.create_call, self.create_ack)
        if self.send_prompt in latest:
            require(self.thread_id is not None, "parent follow-up started before child lookup")
            return self.tool_step(request, "thread_send",
                                  {"thread_id": self.thread_id, "message": self.message},
                                  self.send_call, self.send_ack)
        if self.reaction_prompt in latest:
            return self.tool_step(request, "react_to_message", {"emoji": "👀"},
                                  self.reaction_call, REACTION_ONLY)
        for marker, answer, step in (
            (self.task, self.first_answer, "initial child run"),
            (self.message, self.next_answer, "follow-up child run"),
            (self.bob_prompt, self.bob_answer, "Bob private run"),
        ):
            if marker in latest:
                self.once(step)
                return text_reply(answer)
        raise RuntimeError("unexpected model request: newest user message matched no scripted run")


def read_list(call, user, route, field, *, label=None):
    data = call(user, "GET", route, read_json=True, label=label)
    require(isinstance(data, dict) and isinstance(data.get(field), list),
            f"expected a {field} array in the response")
    return data[field]


def post_message(call, user, room, text, *, invoke=True, label):
    call(user, "POST", f"{room}/messages", body={"text": text, "invoke_agent": invoke},
         label=label)


def await_observation(probe, label, children, model, deadline):
    try:
        while True:
            check_processes(children, model)
            result = probe()
            if result:
                return result
            time.sleep(min(0.1, remaining(deadline)))
    except TimeoutError as exc:
        raise RuntimeError(f"timed out waiting for {label} (model calls: {model.stage})") from exc


def transcript(call, user, room, label):
    events = read_events(call, user, room)
    require(not any(event["kind"] == "error" for event in events),
            f"{label} transcript contains a room error")
    return events


def agent_answer(events, text):
    return any((event["kind"], event["actor"], event["text"]) == ("agent", "agent", text)
               for event in events)


def relayed_answer(events, child_id, answer):
    return any(event["kind"] == "thread" and event["actor"] == "agent"
               and f"({child_id})" in event["text"] and event["text"].endswith("\n" + answer)
               for event in events)


def exercise_threads(call, alice, space, children, model, deadline):
    rooms_path = f"{space}/rooms"
    parent = f"{rooms_path}/lobby"
    post_message(call, alice, parent, model.create_prompt, label="ask parent to create a thread")

    def find_thread():
        rooms = read_list(call, alice, rooms_path, "rooms", label="list Alice's Space rooms")
        matches = [room for room in rooms if room["name"] == model.thread_name]
        require(len(matches) <= 1, "parent created duplicate child rooms")
        if matches:
            room = matches[0]
            require(room.get("parent_room_id") == "lobby", "child room did not point to the lobby")
            require(isinstance(room["id"], str) and room["id"] and room["id"] != "lobby",
                    "invalid child room ID")
            return room["id"]

    child_id = await_observation(find_thread, "the linked child room", children, model, deadline)
    child = f"{rooms_path}/{quote(child_id, safe='')}"

    def first_round():
        child_events = transcript(call, alice, child, "child")
        parent_events = transcript(call, alice, parent, "parent")
        return (agent_answer(child_events, model.first_answer)
                and agent_answer(parent_events, model.create_ack)
                # The relay follows completion in the run registry, so sending
                # a follow-up after it can't race the first child run.
                and relayed_answer(parent_events, child_id, model.first_answer))

    await_observation(first_round, "parent acknowledgment and initial child answer",
                      children, model, deadline)
    require(model.create_result and model.create_result["thread_id"] == child_id,
            "thread_create tool result did not identify the child seen in the Rooms API")
    model.thread_id = child_id
    post_message(call, alice, parent, model.send_prompt, label="ask parent to send a follow-up")

    def second_round():
        child_events = transcript(call, alice, child, "child")
        parent_events = transcript(call, alice, parent, "parent")
        # The parent model never emits the child's tag. It must reach this
        # transcript via Metro's thread relay, not the parent's scripted reply.
        return (agent_answer(child_events, model.next_answer)
                and agent_answer(parent_events, model.send_ack)
                and relayed_answer(parent_events, child_id, model.next_answer))

    await_observation(second_round, "follow-up child answer in both room transcripts",
                      children, model, deadline)
    require(model.send_result and model.send_result["thread_id"] == child_id
            and model.send_result["run_id"] != model.create_result["run_id"],
            "thread_send did not start a new run in the same child")
    return parent, child, child_id


def exercise_runs(call, alice, bob, alice_id, bob_id, child_id, children, model, deadline):
    def runs(user):
        return read_list(call, user, "/v1/runs", "runs")

    def before_membership():
        alice_runs, bob_runs = runs(alice), runs(bob)
        require(all(run["space_id"] == alice_id for run in alice_runs),
                "Alice's Runs API exposed another Space's runs")
        require(all(run["space_id"] == bob_id for run in bob_runs),
                "Bob's Runs API exposed Alice's runs before he joined")
        completed_child_runs = {run["id"] for run in alice_runs
                                if run["room_id"] == child_id and run["status"] == "completed"}
        return ({model.create_result["run_id"], model.send_result["run_id"]} <= completed_child_runs
                and any(run["room_id"] == "lobby" for run in alice_runs)
                and any(run["room_id"] == "lobby" for run in bob_runs))

    await_observation(before_membership, "Space-scoped parent, child, and Bob run records",
                      children, model, deadline)
    call(alice, "POST", f"{SPACES}/{quote(alice_id, safe='')}/members",
         body={"user_id": "bob"}, label="add Bob to Alice's Space")
    require({(run["space_id"], run["room_id"]) for run in runs(bob)}
            >= {(alice_id, "lobby"), (alice_id, child_id), (bob_id, "lobby")},
            "Bob's Runs API did not include the rooms of the Space he joined")


def exercise_chat(call, alice, bob, space_rooms):
    alice_text, bob_text = tagged("Alice private Chat"), tagged("Bob private Chat")
    post_message(call, alice, "/v1/chat", alice_text, invoke=False, label="post Alice's Chat")
    post_message(call, bob, "/v1/chat", bob_text, invoke=False, label="post Bob's Chat")
    message_ids = {}
    for user, actor, own, other in (
        (alice, "alice", alice_text, bob_text),
        (bob, "bob", bob_text, alice_text),
    ):
        events = read_events(call, user, "/v1/chat")
        own_events = [event for event in events
                      if (event["kind"], event["actor"], event["text"]) == ("message", actor, own)]
        require(len(own_events) == 1, f"{actor} could not read their own private Chat post")
        message_ids[actor] = own_events[0]["id"]
        require(not any(other in event["text"] for event in events),
                f"{actor} could read the other user's private Chat")
    # Bob is now a member of Alice's Space. Neither Chat should be broadcast
    # to a Space room, even when he can read both Space transcripts.
    for room in space_rooms:
        events = read_events(call, bob, room)
        require(not any(alice_text in event["text"] or bob_text in event["text"] for event in events),
                "personal Chat appeared in a Space transcript")
    return message_ids["alice"]


def reaction_path(room, target):
    require(type(target) is int and target > 0, "invalid reaction target event ID")
    return f"{room}/events/{target}/reactions"


def toggle_reaction(call, user, path, label):
    for method, changed in (("PUT", True), ("PUT", False), ("DELETE", True), ("DELETE", False)):
        data = call(user, method, path, body={"emoji": "👍"}, read_json=True,
                    label=f"{method} {label} reaction")
        require(isinstance(data, dict) and data.get("changed") is changed,
                f"{method} {label} reaction did not return changed={changed}")


def forbid_reaction(call, user, path, label):
    for method in ("PUT", "DELETE"):
        call(user, method, path, body={"emoji": "👍"}, expect=(404,),
             label=f"{method} inaccessible {label} reaction")


def quiet_period(check, deadline):
    end = time.monotonic() + 0.35
    while True:
        check()
        left = end - time.monotonic()
        if left <= 0:
            return
        time.sleep(min(0.05, left, remaining(deadline)))


def exercise_reactions(call, alice, bob, parent, bob_room, chat_target, children, model, deadline):
    # Bob can read Alice's lobby now, but an event ID from it is not valid
    # when addressed through Bob's own Space, even though both rooms are lobby.
    parent_events = read_events(call, bob, parent)
    target = next((event["id"] for event in parent_events
                   if event["kind"] == "message" and event["text"] == model.create_prompt), None)
    toggle_reaction(call, bob, reaction_path(parent, target), "Space")
    forbid_reaction(call, bob, reaction_path(bob_room, target), "other Space")
    chat = "/v1/chat"
    toggle_reaction(call, alice, reaction_path(chat, chat_target), "personal Chat")
    forbid_reaction(call, bob, reaction_path(chat, chat_target), "other personal Chat")

    def before_agent():
        check_processes(children, model)
        require(model.requests == model.stage == 7,
                "a human reaction or unaddressed Chat message woke the model")
        for user in (alice, bob):
            require(not any(event["kind"] in {"agent", "error"}
                            for event in read_events(call, user, chat)),
                    "unaddressed Chat message or human reaction started agent work")

    quiet_period(before_agent, deadline)
    post_message(call, alice, chat, model.reaction_prompt, invoke=False,
                 label="ask the Chat agent to acknowledge with a reaction only")
    turns = [event for event in read_events(call, alice, chat)
             if event["kind"] == "message" and event["text"] == model.reaction_prompt]
    require(len(turns) == 1, "the addressed Chat message did not appear in the transcript")
    agent_target = turns[0]["id"]
    require(type(agent_target) is int and agent_target > 0, "invalid agent reaction target ID")

    def agent_reacted():
        events = transcript_after_reaction_request()
        for event in events:
            if event["kind"] != "reaction":
                continue
            try:
                data = json.loads(event["text"])
            except ValueError:
                raise RuntimeError("agent reaction event payload was not JSON") from None
            require(isinstance(data, dict)
                    and data.get("target_event_id") == agent_target
                    and data.get("emoji") == "👀" and data.get("present") is True
                    and data.get("reactor_kind") == "agent" and data.get("reactor_id") == "agent"
                    and event["id"] > agent_target, "incorrect agent reaction event payload")
            return model.stage == 9

    def transcript_after_reaction_request():
        check_processes(children, model)
        events = read_events(call, alice, chat, agent_target)
        require(not any(event["kind"] in {"agent", "error"} or REACTION_ONLY in event["text"]
                        for event in events),
                "reaction-only agent turn wrote an error or redundant text to Chat")
        return events

    await_observation(agent_reacted, "agent's cursor-visible Chat reaction",
                      children, model, deadline)
    quiet_period(transcript_after_reaction_request, deadline)
    require(not any(event["kind"] == "reaction" for event in read_events(call, bob, chat)),
            "Alice's personal Chat reaction appeared in Bob's Chat")


def smoke_threads(server, timeout):
    alice, bob = (secrets.token_hex(24) for _ in range(2))
    with temp_state() as directory, ExitStack() as stack:
        model = ThreadModel()
        deadline = time.monotonic() + timeout
        port, children = start_server(stack, server, Path(directory), clean_env(directory),
                                      {"alice": alice, "bob": bob}, model, deadline)
        call = partial(request, port, deadline)
        alice_id = create_space(call, alice, "Alice Thread Space")
        bob_id = create_space(call, bob, "Bob Thread Space")
        require(alice_id != bob_id, "both Spaces were assigned the same ID")
        alice_space, bob_space = (f"{SPACES}/{quote(sid, safe='')}" for sid in (alice_id, bob_id))
        parent, child, child_id = exercise_threads(call, alice, alice_space, children, model, deadline)
        bob_room = f"{bob_space}/rooms/lobby"
        post_message(call, bob, bob_room, model.bob_prompt, label="invoke agent in Bob's Space")
        await_observation(
            lambda: agent_answer(transcript(call, bob, bob_room, "Bob private Space"), model.bob_answer),
            "Bob's private Space agent reply", children, model, deadline,
        )
        exercise_runs(call, alice, bob, alice_id, bob_id, child_id, children, model, deadline)
        chat_target = exercise_chat(call, alice, bob, (parent, child, bob_room))
        exercise_reactions(call, alice, bob, parent, bob_room, chat_target, children, model, deadline)
        check_processes(children, model)
        require(model.requests == model.stage == 9, "expected nine scripted model requests")
    print("PASS: the parent's child thread replied twice, relayed its follow-up, and remained "
          "Space-scoped; personal Chat stayed private; reactions were isolated and idempotent.")


if __name__ == "__main__":
    signal.signal(signal.SIGTERM, terminate)
    raise SystemExit(main(smoke_threads, ("metro-server",), __doc__))
