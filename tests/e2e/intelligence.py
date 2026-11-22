#!/usr/bin/env python3
"""Black-box Metro intelligence tests against compiled binaries and loopback vendors."""

import base64
from contextlib import ExitStack
from functools import partial
import hmac
from http.server import ThreadingHTTPServer
import json
from pathlib import Path
import signal
import threading
import time
from urllib.parse import quote

from device_connector import (
    assert_hidden, issue_token, private_file, start_connector, wait_exit, wait_online,
)
from settings import ModelHandler, check_db_redacted, serve_fixture, token
from smoke import (
    HOST, check_processes, clean_env, content_parts, create_space, main, read_events,
    request, require, server_port, start_child, temp_state, terminate, text_reply, wait_for_server,
)

AVAILABLE = "/v1/models/available"
PROVIDERS = "/v1/providers"
ROUTING = "/v1/model-routing"
SEARCH = "/v1/search"
CHAT = "/v1/chat"

ROUTED = "Please analyze the causal chain for the incident review. ROUTING_CLEAR"
DOUBTFUL = "Perhaps someone should review the incident one day. ROUTING_DOUBTFUL"
MANUAL = "@agent please analyze the causal chain. ROUTING_MANUAL"
NO_MATCH = "@agent please write a haiku about the moon. ROUTING_NO_MATCH"
HUMAN = "@bob please analyze the causal chain. ROUTING_HUMAN_ONLY"
FAILED = "Please analyze the causal chain right now. ROUTING_PROVIDER_FAILURE"
FAILED_CHAT = "Please analyze this causal chain for the review. ROUTING_CHAT_FAILURE"

QUERY = "What powered the expedition after sunset?"
LEXICAL = "The expedition after sunset recorded routine weather. SEARCH_LEXICAL"
SEMANTIC = "Portable solar cells charged the night team's batteries. SEARCH_SEMANTIC"
CHAT_HIT = "The evening crew also carried an auxiliary generator. SEARCH_OWN_CHAT"
PRIVATE = "A private transcript reveals a hidden power source. SEARCH_FOREIGN_CHAT"
STRANGER = "A stranger's room has secret battery specs. SEARCH_STRANGER_SPACE"
INACCESSIBLE = ("SEARCH_FOREIGN_CHAT", "SEARCH_STRANGER_SPACE")


def provider_path(connection_id):
    require(isinstance(connection_id, str) and bool(connection_id), "missing provider connection ID")
    return f"{PROVIDERS}/{quote(connection_id, safe='')}"


class Vendor(ThreadingHTTPServer):
    """Three real upstream protocols, with distinct keys/models and a request ledger."""

    daemon_threads = True

    def __init__(self, capability, tag):
        super().__init__((HOST, 0), ModelHandler)
        self.capability = capability
        self.tag = tag
        self.model = f"fixture/{tag}"
        self.key = token()
        self.base_url = f"http://{HOST}:{self.server_port}/v1"
        self.error = None
        self.lock = threading.Lock()
        self.calls = []
        self.reject_rerank = False

    def get_request(self):
        connection, address = super().get_request()
        connection.settimeout(2)
        return connection, address

    def handle_error(self, _request, _address):
        self.error = "loopback vendor HTTP handler failed"

    def posts(self):
        with self.lock:
            return tuple(body for method, _, body in self.calls if method == "POST")

    def respond(self, method, path, authorization, payload):
        require(hmac.compare_digest((authorization or "").encode(), f"Bearer {self.key}".encode()),
                "wrong authorization at loopback vendor")
        with self.lock:
            self.calls.append((method, path, payload))
            serial = len(self.calls)
        if method == "GET":
            require(path == "/v1/models", "unexpected model listing path")
            if self.capability == "systemone":
                return 200, {"models": [{"name": self.model, "description": "Fixture classifier",
                                         "release_date": "2026-01-01"}]}
            return 200, {"object": "list", "data": [{"id": self.model, "object": "model"}]}

        require(isinstance(payload, dict) and payload.get("model") == self.model,
                "wrong upstream model on a fixture POST")
        require(not any(marker in json.dumps(payload) for marker in INACCESSIBLE),
                "a model upstream received unauthorized transcript material")
        if self.capability == "systemone":
            require(path == "/v1/systemone", "System One used the wrong wire route")
            return self.decide(payload)
        if self.capability == "embeddings":
            require(path == "/v1/embeddings", "embeddings used the wrong wire route")
            return 200, self.embed(payload)
        require(path == "/v1/chat/completions" and not payload.get("stream"),
                "chat used the wrong wire route or requested streaming")
        user_turns = ["\n".join(content_parts(msg.get("content")))
                      for msg in payload["messages"] if msg.get("role") == "user"]
        require(user_turns and user_turns[-1], "completion had no user text")
        require(self.tag != "fallback", "an unexpected run reached the server fallback model")
        if self.tag == "routed" and "ROUTING_CHAT_FAILURE" in user_turns[-1]:
            return 503, {"error": {"message": "fixture completion unavailable"}}
        return 200, {
            "id": f"chatcmpl-{self.tag}-{serial}",
            "object": "chat.completion",
            "created": int(time.time()),
            "model": self.model,
            "choices": [{"index": 0, "message": text_reply(f"reply from {self.tag}"),
                         "finish_reason": "stop"}],
            "usage": {"prompt_tokens": 1, "completion_tokens": 1, "total_tokens": 2},
        }

    def decide(self, body):
        state, questions = body["state"], body["questions"]
        require(isinstance(state, (str, dict, list)) and isinstance(questions, dict) and questions,
                "System One did not send state and named questions")
        rerank = "relevant" in questions
        if rerank:
            require(set(questions) == {"relevant"} and questions["relevant"]["type"] == "noul"
                    and isinstance(state, dict) and set(state) == {"search_query", "candidate_message"}
                    and state["search_query"] == QUERY
                    and isinstance(state["candidate_message"], str),
                    "rerank must ask one Noul about exactly one query–candidate pair")
            if self.reject_rerank:
                return 503, {"detail": {"error_type": "api_error", "message": "fixture rerank unavailable"}}
        message = state.get("message", "") if isinstance(state, dict) else ""
        if "ROUTING_PROVIDER_FAILURE" in message:
            return 503, {"detail": {"error_type": "api_error", "message": "fixture offline"}}
        answers = {}
        for name, question in questions.items():
            kind = question["type"]
            if kind == "noul":
                require(question.get("instructions") or question.get("criteria"),
                        "Noul lacked decision criteria")
                if name == "relevant":
                    p = 0.99 if state["candidate_message"] == SEMANTIC else 0.20
                elif name == "directed":
                    p = 0.50 if "ROUTING_DOUBTFUL" in message else 0.99
                else:
                    raise RuntimeError("unrecognized Noul decision")
                answers[name] = {"type": "noul", "noul": p}
            elif kind == "choice":
                criteria = question["criteria"]
                require(isinstance(criteria, dict) and criteria, "Choice criteria were not a map")
                selected = [key for key, value in criteria.items()
                            if "causal chain" in json.dumps(value).lower()]
                require(len(selected) == 1, "Choice did not present the expected use-case description")
                require("no_match" in criteria, "Choice omitted the no-match option")
                choice = "no_match" if message == NO_MATCH else selected[0]
                answers[name] = {
                    "type": "choice", "choice": choice, "confidence": 1.0,
                    "probabilities": {key: float(key == choice) for key in criteria},
                }
            elif kind == "score":
                criteria = question["criteria"]
                require(isinstance(criteria, list) and 2 <= len(criteria) <= 10,
                        "Score criteria were not ordered levels")
                last = len(criteria) - 1
                relevance = 0.99 if "SEARCH_SEMANTIC" in json.dumps(state) else 0.20
                probabilities = {str(i): 0.0 for i in range(len(criteria))}
                probabilities["0"], probabilities[str(last)] = 1 - relevance, relevance
                mode = last if relevance > 0.5 else 0
                spread = sum(p * abs(int(i) - mode) for i, p in probabilities.items())
                uniform = sum(abs(i - last / 2) for i in range(last + 1)) / (last + 1)
                answers[name] = {
                    "type": "score", "score": last * relevance,
                    "confidence": max(0.0, 1 - spread / uniform),
                    "legend": {str(i): level for i, level in enumerate(criteria)},
                    "probabilities": probabilities,
                }
            else:
                raise RuntimeError("unexpected System One question type")
        return 200, {"model": self.model, "answers": answers,
                     "usage": {"input_tokens": 42, "output_tokens": 0}}

    def embed(self, body):
        inputs = body["input"]
        if isinstance(inputs, str):
            inputs = [inputs]
        require(isinstance(inputs, list) and inputs and all(isinstance(item, str) for item in inputs),
                "embedding input was not text or a nonempty text batch")
        require(body.get("encoding_format", "float") == "float",
                "embedding fixture expected float encoding")
        def vector(value):
            if QUERY in value or "SEARCH_LEXICAL" in value:
                return [1.0, 0.0, 0.0]
            if "SEARCH_SEMANTIC" in value:
                return [0.8, 0.6, 0.0]
            if "SEARCH_OWN_CHAT" in value:
                return [0.6, 0.8, 0.0]
            return [0.0, 0.0, 1.0]

        return {"object": "list", "model": self.model,
                "data": [{"object": "embedding", "index": i, "embedding": vector(text)}
                         for i, text in enumerate(inputs)],
                "usage": {"prompt_tokens": 2 * len(inputs), "total_tokens": 2 * len(inputs)}}


class TestRun:
    def __init__(self, stack, directory, server_binary, connector_binary, timeout):
        self.stack = stack
        self.directory = Path(directory)
        self.connector = connector_binary
        self.deadline = time.monotonic() + timeout
        self.systemone = serve_fixture(stack, Vendor("systemone", "classifier"))
        self.embeddings = serve_fixture(stack, Vendor("embeddings", "embeddings"))
        self.default = serve_fixture(stack, Vendor("chat", "default"))
        self.routed = serve_fixture(stack, Vendor("chat", "routed"))
        self.shared = serve_fixture(stack, Vendor("chat", "shared-local"))
        self.fallback = serve_fixture(stack, Vendor("chat", "fallback"))
        self.vendors = (self.systemone, self.embeddings, self.default, self.routed, self.shared,
                        self.fallback)
        self.tokens = {user: token() for user in ("alice", "bob", "carol")}
        self.secrets = [*self.tokens.values(), *(vendor.key for vendor in self.vendors)]
        self.env = clean_env(directory)
        self.port = server_port()
        self.db = self.directory / "metro.sqlite"
        server = start_child(stack, "intelligence server", server_binary, self.directory, self.env | {
            "METRO_BIND": f"{HOST}:{self.port}",
            "METRO_DB": str(self.db),
            "METRO_DEV_USERS": json.dumps(self.tokens),
            "METRO_ORG_ADMINS": json.dumps(["alice"]),
            "METRO_PROVIDER_MASTER_KEY": base64.b64encode(bytes.fromhex(token())).decode("ascii"),
            "METRO_PROVIDER_ALLOWED_BASE_URLS": json.dumps([
                self.systemone.base_url, self.default.base_url, self.routed.base_url,
            ]),
            "METRO_MODEL_BASE_URL": self.fallback.base_url,
            "METRO_MODEL": self.fallback.model,
            "METRO_MODEL_API_KEY": self.fallback.key,
        })
        self.children = [server]
        wait_for_server(self.port, self.children, self.systemone, self.deadline)
        self.request = partial(request, self.port, self.deadline)

    def call(self, user, method, path, **kwargs):
        body = self.request(self.tokens[user], method, path, **kwargs)
        assert_hidden(body, self.secrets)
        return body

    def healthy(self):
        for vendor in self.vendors:
            check_processes(self.children, vendor)

    def connect(self, owner, provider_id, vendor, *, default=None):
        device = provider_id.startswith("device-")
        body = {"name": vendor.tag, "provider_id": provider_id, "model": vendor.model}
        if not device:
            body.update(base_url=vendor.base_url, api_key=vendor.key)
        if default is not None:
            body["is_default"] = default
        row = self.call(owner, "POST", PROVIDERS, body=body, expect=(201,), read_json=True)
        require(row["provider_id"] == provider_id and row["model"] == vendor.model,
                "provider creation returned a different adapter or model")
        if device:
            assert_hidden(row, (vendor.base_url,))
        return row["id"]

    def pair(self, owner, connection_id, kind, vendor):
        status = f"{provider_path(connection_id)}/device-token"
        credential = issue_token(self.request, self.tokens[owner], status)
        self.secrets.append(credential)
        home = self.directory / f"{vendor.tag}-connector"
        home.mkdir(mode=0o700)
        key_file, token_file = home / "input-key", home / "input-token"
        private_file(key_file, vendor.key)
        private_file(token_file, credential)
        env = self.env | {"METRO_CONNECTOR_HOME": str(home), "METRO_CONNECTOR_POLL_MS": "50"}
        setup = start_connector(self.stack, self.connector, self.directory, env, [
            "setup", "--profile", "fixture", "--server-url", f"http://{HOST}:{self.port}",
            "--kind", kind, "--base-url", vendor.base_url, "--api-key-file", str(key_file),
            "--device-token-file", str(token_file), "--model", vendor.model, "--yes",
        ])
        wait_exit(setup, self.deadline, 0, f"{kind} connector setup")
        key_file.unlink()
        token_file.unlink()
        worker = start_connector(self.stack, self.connector, self.directory, env, [
            "run", "--profile", "fixture",
        ])
        self.children.append((worker, f"{kind} connector"))
        assert_hidden(wait_online(self.request, self.tokens[owner], status, self.children,
                                  vendor, self.deadline), (*self.secrets, vendor.base_url))

    def post(self, user, conversation, text, **fields):
        event = self.call(user, "POST", f"{conversation}/messages", body={"text": text, **fields},
                          expect=(202,), read_json=True)
        require(type(event.get("id")) is int and event["text"] == text,
                "message was not written to the expected conversation")
        return event["id"]

    def wait(self, condition, label, *, seconds=8):
        end = min(self.deadline, time.monotonic() + seconds)
        while True:
            self.healthy()
            result = condition()
            if result:
                return result
            delay = end - time.monotonic()
            require(delay > 0, f"timed out waiting for {label}")
            time.sleep(min(0.05, delay))

    def expect_reply(self, user, conversation, after, vendor, prompt):
        def received():
            events = read_events(self.call, user, conversation, after)
            require(not any(event["kind"] == "error" for event in events),
                    "agent returned an error instead of the expected fixture reply")
            replies = [event for event in events
                       if event["kind"] == "agent" and event["text"] == f"reply from {vendor.tag}"]
            if not replies:
                return None
            turn_id = replies[0]["activity"]["id"]
            for event in events:
                activity = event.get("activity") or {}
                if activity.get("id") == turn_id and activity.get("phase") == "completed":
                    starts = [entry["activity"]["route"] for entry in events
                              if (entry.get("activity") or {}).get("id") == turn_id
                              and entry["activity"].get("phase") == "started"]
                    require(len(starts) == 1, "expected one annotated start for the agent turn")
                    return starts[0]
            return None

        route = self.wait(received, f"a completed reply from {vendor.tag}")
        calls = vendor.posts()
        require(calls and any(prompt in "\n".join(content_parts(message.get("content")))
                              for message in calls[-1]["messages"] if message.get("role") == "user"),
                "expected completion did not carry the outgoing message")
        require(route.get("model_id") == vendor.model, "turn annotation named a different model")
        return route

    def expect_no_run(self, user, conversation, after, counts, label):
        """Observe a bounded quiet interval after the expected classifier call, if any."""
        end = min(self.deadline, time.monotonic() + 0.45)
        while True:
            self.healthy()
            require(self.post_counts() == counts, f"{label}: unexpected vendor request")
            events = read_events(self.call, user, conversation, after)
            require(not any(event["kind"] in ("agent", "activity", "tool_call", "error")
                            for event in events),
                    f"{label}: unexpectedly started an agent run or emitted an error")
            delay = end - time.monotonic()
            if delay <= 0:
                return
            time.sleep(min(0.04, delay))

    def post_counts(self):
        return tuple(len(vendor.posts()) for vendor in self.vendors)

    def finish(self):
        self.healthy()
        check_db_redacted(self.db, (self.embeddings.base_url, self.shared.base_url,
                                    *self.secrets[len(self.tokens):]))


def room_path(space):
    return f"/v1/spaces/{quote(space, safe='')}/rooms/lobby"


def available(run, user, space=None, *, any_space=False):
    suffix = f"?space_id={quote(space, safe='')}" if space else "?scope=any" if any_space else ""
    body = run.call(user, "GET", AVAILABLE + suffix, read_json=True)
    assert_hidden(body, tuple(vendor.base_url for vendor in run.vendors))
    models = body["models"]
    require(isinstance(models, list), "available models did not return a list")
    rows = {item["connection_id"]: item for item in models}
    require(len(rows) == len(models), "available models duplicated a connection")
    return rows


def share(run, connection, visibility, *spaces, capability="chat", mode="owner_connection"):
    path = f"{provider_path(connection)}/sharing"
    body = {"visibility": visibility, "space_ids": list(spaces)}
    updated = run.call("alice", "PUT", path, body=body, expect=(200,), read_json=True)
    require(updated["visibility"] == visibility and updated["space_ids"] == list(spaces)
            and updated["mode"] == mode and updated["capability"] == capability,
            "sharing response did not describe the granted use")
    saved = run.call("alice", "GET", path, read_json=True)
    require(saved == updated, "sharing change did not persist")


def exercise_sharing(run, connection, shared_space, other_space):
    shared_room, other_room = room_path(shared_space), room_path(other_space)
    body = {"text": "@agent Try the borrowed endpoint.", "provider_connection_id": connection}

    def blocked(conversation, label):
        before = run.post_counts()
        run.call("bob", "POST", f"{conversation}/messages", body=body, expect=(404,), label=label,
                 read_json=True)
        require(run.post_counts() == before, f"{label}: contacted a model despite denying use")

    def borrowed(conversation, text):
        before = run.post_counts()
        event = run.post("bob", conversation, text, provider_connection_id=connection)
        route = run.expect_reply("bob", conversation, event, run.shared, text)
        require(route.get("source") == "override" and route.get("connection_id") == connection,
                "borrowed turn was not attributed to the selected connection")
        expected = tuple(n + (vendor is run.shared) for vendor, n in zip(run.vendors, before))
        require(run.post_counts() == expected, "borrowed turn did not use only the owner's endpoint")

    require(connection not in available(run, "bob", shared_space),
            "a personal connection was visible to another Space member")
    blocked(shared_room, "personal connection cannot be borrowed")
    share(run, connection, "spaces", shared_space)
    entry = available(run, "bob", shared_space)[connection]
    require(all(entry.get(key) == value for key, value in {
        "model": run.shared.model, "capability": "chat", "visibility": "spaces",
        "owned": False, "owner_id": "alice", "execution": "device",
        "runs_on": "owner_connection", "billing": "owner",
    }.items()), "borrowed model metadata did not identify the owner and billing")
    require(connection in available(run, "bob", any_space=True)
            and connection not in available(run, "bob")
            and connection not in available(run, "bob", other_space)
            and connection not in available(run, "carol", any_space=True),
            "Space share was visible outside the authorized scope")
    run.call("carol", "GET", f"{AVAILABLE}?space_id={quote(shared_space, safe='')}", expect=(404,),
             read_json=True)
    run.call("bob", "PUT", f"{provider_path(connection)}/sharing",
             body={"visibility": "organization"}, expect=(404,), read_json=True)
    run.call("bob", "POST", f"{provider_path(connection)}/models",
             body={}, expect=(404,), read_json=True)
    run.call("bob", "GET", f"{provider_path(connection)}/device-token",
             expect=(404,), read_json=True)
    borrowed(shared_room, "@agent Reply through Alice's shared Space model.")
    blocked(other_room, "Space share cannot be used in a different Space")
    blocked(CHAT, "Space share cannot be used in private Chat")

    share(run, connection, "organization")
    require(available(run, "carol")[connection]["visibility"] == "organization",
            "organization sharing was not visible outside the original Space")
    borrowed(CHAT, "@agent Reply through Alice's organization share.")
    share(run, connection, "personal")
    require(connection not in available(run, "bob", shared_space)
            and connection not in available(run, "carol"), "revocation did not remove availability")
    blocked(shared_room, "revoked share cannot be borrowed")


def exercise_subscription_preset(run):
    # There is deliberately no vendor sign-in or subscriber credential.
    row = run.call("alice", "POST", PROVIDERS, body={
        "name": "Unpaired subscription preset", "provider_id": "device-openai-oauth",
        "model": "fixture-subscription", "is_default": False,
    }, expect=(201,), read_json=True)
    share(run, row["id"], "organization", mode="preset")
    entry = available(run, "bob")[row["id"]]
    require(all(entry.get(key) == value for key, value in {
        "owned": False, "owner_id": "alice", "runs_on": "your_own_sign_in",
        "billing": "you", "ready": False,
    }.items()), "subscription share did not require the borrower's own connection")
    before = run.post_counts()
    event = run.post("bob", CHAT, "@agent Try the unpaired subscription preset.",
                     provider_connection_id=row["id"])
    run.wait(lambda: any(item["kind"] == "error"
                         for item in read_events(run.call, "bob", CHAT, event)),
             "a visible failure without the borrower's own sign-in")
    require(run.post_counts() == before,
            "unpaired subscription preset sent inference instead of requiring the borrower's sign-in")
    share(run, row["id"], "personal", mode="preset")


def assert_questions(vendor, after, prompt, expected):
    payloads = vendor.posts()[after:]
    require(len(payloads) == len(expected)
            and all(body["state"]["message"] == prompt for body in payloads),
            "System One did not evaluate the intended outgoing turn")
    actual = tuple({name: question["type"] for name, question in body["questions"].items()}
                   for body in payloads)
    require(actual == tuple({name: kind} for name, kind in expected),
            "routing did not send the expected sequence of single-question System One requests")


def configure_routing(run, user, **options):
    saved = run.call(user, "PUT", ROUTING, body=options, expect=(200,), read_json=True)
    require(all(saved.get(key) == value for key, value in options.items())
            and run.call(user, "GET", ROUTING, read_json=True) == saved,
            "routing preferences were not persisted")


def exercise_routing(run, space, classifier, default, routed):
    room = room_path(space)
    current = run.call("alice", "GET", ROUTING, read_json=True)
    require(current["enabled"] is False and current["infer_addressing"] is False,
            "classifier and routing were not opt-in")
    before = run.post_counts()
    event = run.post("alice", room, "Please analyze the causal chain. ROUTING_OPT_OUT")
    run.expect_no_run("alice", room, event, before, "unaddressed while opted out")
    configure_routing(
        run, "alice", enabled=True, infer_addressing=True, classifier_id=classifier,
        default_chat_id=default, min_directedness=0.85, min_route_probability=0.65,
        cases=[
            {"key": "causal_review", "label": "Incident review",
             "description": "Analyze the causal chain of an incident.", "connection_id": routed},
            {"key": "quick_fact", "label": "Quick fact",
             "description": "Answer a simple factual question.", "connection_id": default},
        ],
    )
    directed = (("directed", "noul"),)
    use_case = (("use_case", "choice"),)
    implicit = directed + use_case

    def change_counts(counts, vendor):
        index = run.vendors.index(vendor)
        return counts[:index] + (counts[index] + 1,) + counts[index + 1:]

    before = run.post_counts()
    event = run.post("alice", room, HUMAN)
    run.expect_no_run("alice", room, event, before, "human-only mention")

    before = run.post_counts()
    event = run.post("alice", room, DOUBTFUL)
    run.wait(lambda: len(run.systemone.posts()) > before[0], "directedness for uncertain turn")
    assert_questions(run.systemone, before[0], DOUBTFUL, directed)
    run.expect_no_run("alice", room, event, change_counts(before, run.systemone), "uncertain turn")

    for text, questions, addressed_by in (
        (ROUTED, implicit, "classifier"), (MANUAL, use_case, "explicit"),
    ):
        before = run.post_counts()
        event = run.post("alice", room, text)
        route = run.expect_reply("alice", room, event, run.routed, text)
        require(route.get("source") == "case" and route.get("addressed_by") == addressed_by
                and route.get("case_key") == "causal_review" and route.get("connection_id") == routed,
                "turn annotation did not identify the inferred address and intended case")
        assert_questions(run.systemone, before[0], text, questions)
        require(run.post_counts()[1:] == change_counts(before, run.routed)[1:],
                "a successful routed turn also contacted another answering model")

    before = run.post_counts()
    event = run.post("alice", room, NO_MATCH)
    route = run.expect_reply("alice", room, event, run.default, NO_MATCH)
    assert_questions(run.systemone, before[0], NO_MATCH, use_case)
    require(route.get("source") == "routing_default" and route.get("connection_id") == default
            and run.post_counts()[1:] == change_counts(before, run.default)[1:],
            "a no-match decision did not use only the configured default")

    before = run.post_counts()
    event = run.post("alice", room, FAILED_CHAT)
    run.wait(lambda: any(entry["kind"] == "error"
                         for entry in read_events(run.call, "alice", room, event)),
             "visible failure from the selected answering model")
    after = run.post_counts()
    assert_questions(run.systemone, before[0], FAILED_CHAT, implicit)
    require(len(run.routed.posts()) > before[run.vendors.index(run.routed)],
            "the intended answering model did not receive the failing turn")
    require(all(a == b for vendor, a, b in zip(run.vendors, before, after)
                if vendor not in (run.systemone, run.routed)),
            "a failed answering model billed a different model")

    before = run.post_counts()
    event = run.post("alice", room, FAILED)
    run.wait(lambda: len(run.systemone.posts()) > before[0], "failing directedness call")
    # Provider retries are permissible; no answering model may run.
    end = min(run.deadline, time.monotonic() + 0.45)
    while True:
        run.healthy()
        require(all(body["state"]["message"] == FAILED and set(body["questions"]) == {"directed"}
                    for body in run.systemone.posts()[before[0]:]),
                "directedness failure reached the use-case classifier")
        require(run.post_counts()[1:] == before[1:],
                "directedness failure billed an unexpected model")
        require(not any(entry["kind"] in ("agent", "activity", "tool_call")
                        for entry in read_events(run.call, "alice", room, event)),
                "directedness failure unexpectedly started an agent run")
        delay = end - time.monotonic()
        if delay <= 0:
            break
        time.sleep(min(0.04, delay))


def search_request(run, scope, space=None, *, expect=(200,)):
    body = {"query": QUERY, "scope": scope, "limit": 10}
    if space is not None:
        body["space_id"] = space
    return run.call("bob", "POST", SEARCH, body=body, expect=expect, read_json=True)


def search_results(response, ranking):
    require(response.get("ranking") == ranking and response.get("partial") is False
            and isinstance(response.get("results"), list),
            f"search did not return complete {ranking} results")
    require(not any(marker in json.dumps(response) for marker in INACCESSIBLE),
            "search returned unauthorized transcript material")
    return response["results"]


def exercise_search(run, shared_space, stranger_space):
    room = room_path(shared_space)
    lexical = run.post("alice", room, LEXICAL)
    semantic = run.post("alice", room, SEMANTIC)
    chat = run.post("bob", CHAT, CHAT_HIT)
    run.post("alice", CHAT, PRIVATE)
    run.post("carol", room_path(stranger_space), STRANGER)

    before = run.post_counts()
    search_request(run, "space", stranger_space, expect=(404,))
    require(run.post_counts() == before, "inaccessible Space search contacted an upstream")
    classifier_before, embedding_before = len(run.systemone.posts()), len(run.embeddings.posts())

    # First prove the pure embedding preference, then turn the same rerank back on.
    run.systemone.reject_rerank = True
    try:
        raw = search_request(run, "space", shared_space)
    finally:
        run.systemone.reject_rerank = False
    retrieval = search_results(raw, "embeddings")
    expected = [(shared_space, "lobby", lexical), (shared_space, "lobby", semantic)]

    def keys(items):
        return [(item["space_id"], item["room_id"], item["event_id"]) for item in items]

    require(keys(retrieval)[:2] == expected and retrieval[0]["score"] > retrieval[1]["score"],
            "embedding shortlist did not prefer the higher-cosine candidate")
    require(raw.get("notice"), "rerank failure was not disclosed")
    require(len(run.systemone.posts()) > classifier_before,
            "the failed rerank did not actually call System One")
    classifier_before = len(run.systemone.posts())

    ranked = search_results(search_request(run, "space", shared_space), "classifier")
    require(keys(ranked)[:2] == expected[::-1] and ranked[0]["score"] > ranked[1]["score"],
            "System One did not reverse the embedding preference")
    ranked_chat = search_results(search_request(run, "chat"), "classifier")
    require(keys(ranked_chat) == [(None, None, chat)],
            "Chat search did not restrict results to Bob's personal Chat")
    ranked_all = search_results(search_request(run, "all"), "classifier")
    require(set(keys(ranked_all)) == {*expected, (None, None, chat)},
            "all search did not return exactly the authorized Chat and Space candidates")

    embedded = json.dumps(run.embeddings.posts()[embedding_before:])
    scored = run.systemone.posts()[classifier_before:]
    require(all(text in embedded for text in (QUERY, LEXICAL, SEMANTIC, CHAT_HIT)),
            "embedding upstream did not see the query and all authorized candidates")
    require({body["state"]["candidate_message"] for body in scored} == {LEXICAL, SEMANTIC, CHAT_HIT}
            and all(body["state"]["search_query"] == QUERY for body in scored),
            "System One did not score each authorized query–candidate pair independently")
    rubrics = {json.dumps(body["questions"], sort_keys=True) for body in scored}
    require(len(rubrics) == 1 and all(
        {name: question["type"] for name, question in body["questions"].items()}
        == {"relevant": "noul"} for body in scored
    ), "pointwise rerank did not use an identical Noul rubric for each candidate")
    require(all(len(vendor.posts()) == n for vendor, n in zip(run.vendors[2:], before[2:])),
            "semantic search silently used a chat model")


def intelligence(server, connector, timeout):
    with temp_state() as directory, ExitStack() as stack:
        run = TestRun(stack, directory, server, connector, timeout)
        space = create_space(run.call, "alice", "Shared intelligence fixture")
        run.call("alice", "POST", f"/v1/spaces/{quote(space, safe='')}/members",
                 body={"user_id": "bob"}, expect=(201,))
        stranger = create_space(run.call, "carol", "Inaccessible fixture Space")

        classifier = run.connect("alice", "custom-systemone", run.systemone, default=True)
        default = run.connect("alice", "custom", run.default, default=True)
        routed = run.connect("alice", "custom", run.routed, default=False)
        chat_kind = "device-openai-compatible"
        shared = run.connect("alice", chat_kind, run.shared, default=False)
        bob_classifier = run.connect("bob", "custom-systemone", run.systemone, default=True)
        embedding_kind = "device-embeddings"
        embedding = run.connect("alice", embedding_kind, run.embeddings, default=True)
        run.pair("alice", embedding, embedding_kind, run.embeddings)
        run.pair("alice", shared, chat_kind, run.shared)

        for user, connection, vendor in (("alice", classifier, run.systemone),
                                         ("alice", embedding, run.embeddings)):
            listing = run.call(user, "POST", f"{provider_path(connection)}/models",
                               body={}, read_json=True)
            require(listing == {"models": [vendor.model]}
                    and any(method == "GET" and path == "/v1/models"
                            for method, path, _ in vendor.calls),
                    "capability-specific discovery did not reach the expected vendor")
        alice_models, bob_models = available(run, "alice"), available(run, "bob")
        require({row["capability"]: cid for cid, row in alice_models.items() if row["is_default"]}
                == {"chat": default, "system_one": classifier, "embeddings": embedding}
                and {row["capability"]: cid for cid, row in bob_models.items() if row["is_default"]}
                == {"system_one": bob_classifier},
                "discovery did not expose independent capability defaults")
        require(embedding not in bob_models and alice_models[embedding]["execution"] == "device"
                and alice_models[classifier]["execution"] == "server",
                "discovery exposed a personal device or used the wrong execution kind")
        run.call("bob", "PUT", ROUTING, body={"embedding_id": bob_classifier}, expect=(400,),
                 read_json=True, label="search cannot use a classifier as embeddings")
        share(run, embedding, "organization", capability="embeddings")
        borrowed_embedding = available(run, "bob")[embedding]
        require(all(borrowed_embedding.get(key) == value for key, value in {
            "capability": "embeddings", "execution": "device", "owner_id": "alice",
            "owned": False, "runs_on": "owner_connection", "billing": "owner",
        }.items()), "borrowed embeddings were not attributed to Alice's device")
        run.call("bob", "POST", f"{provider_path(embedding)}/device-token", body={}, expect=(404,),
                 read_json=True, label="borrowing a typed device does not grant its credential")
        configure_routing(run, "bob", embedding_id=embedding, classifier_id=bob_classifier)
        exercise_search(run, space, stranger)
        share(run, embedding, "personal", capability="embeddings")
        before = run.post_counts()
        search_request(run, "chat", expect=(502,))
        require(run.post_counts() == before, "revoked embeddings still sent search data")
        other = create_space(run.call, "bob", "Outside sharing fixture")
        exercise_sharing(run, shared, space, other)
        exercise_subscription_preset(run)
        exercise_routing(run, space, classifier, default, routed)
        run.finish()
    print("PASS: capability discovery, borrowed endpoint, directedness and routing, device embeddings, semantic rerank and privacy.")


if __name__ == "__main__":
    signal.signal(signal.SIGTERM, terminate)
    raise SystemExit(main(intelligence, binaries=("metro-server", "metro-connector"),
                          description=__doc__))
