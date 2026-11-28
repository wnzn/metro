#!/usr/bin/env python3
"""workspace.write and workspace.shell end to end: a scripted model, the real
server and the real runner. The shell command waits for the Space owner's
approval, runs in the runner's workspace, and a stopped run kills its command."""

from contextlib import ExitStack, closing
from functools import partial
import json
from pathlib import Path
import secrets
import signal
import sqlite3
import time
from urllib.parse import quote

from smoke import (
    HOST, SPACES, OpenAIModel, check_processes, clean_env, content_parts, create_space,
    issue_token, main, read_events, remaining, request, require, start_child, start_server,
    temp_state, terminate, text_reply, tool_reply,
)


class ShellModel(OpenAIModel):
    """Each person's message names the one tool call to make; the next turn
    quotes its result."""

    def respond(self, request):
        messages = request["messages"]
        last = messages[-1]
        if last["role"] == "tool":
            text = " ".join(content_parts(last.get("content")))
            return text_reply(f"tool said: {text}")
        names = {t["function"]["name"] for t in request.get("tools", [])}
        for name in ("workspace_write", "workspace_shell"):
            require(name in names, f"{name} was not offered to the upgraded agent")
        prompt = " ".join(content_parts(last.get("content")))
        if "write the note" in prompt:
            call = {"name": "workspace_write",
                    "arguments": json.dumps({"path": "notes/hello.txt", "content": "hello from metro\n"})}
        elif "sleep" in prompt:
            call = {"name": "workspace_shell",
                    "arguments": json.dumps({"command": "touch started; sleep 30; touch finished"})}
        else:
            call = {"name": "workspace_shell",
                    "arguments": json.dumps({"command": "cat notes/hello.txt; echo \"$TERM\"; pwd"})}
        return tool_reply(f"call_{self.stage}", call)


def wait_events(call, user, room, pred, children, model, deadline, after=0):
    while True:
        check_processes(children, model)
        events = read_events(call, user, room, after)
        hit = [e for e in events if pred(e)]
        if hit:
            return hit[-1], events
        time.sleep(min(0.05, remaining(deadline)))


def approval_of(event):
    return json.loads(event["text"])


def run_shell(server, runner, timeout):
    alice, bob = (secrets.token_hex(24) for _ in range(2))
    with temp_state() as directory, ExitStack() as stack:
        state = Path(directory)
        workspace = state / "workspace"
        workspace.mkdir()
        fixture = (Path(__file__).resolve().parents[2] / "crates" / "server" / "tests"
                   / "fixtures" / "agent_tools_v062.sql")
        with closing(sqlite3.connect(state / "metro.sqlite")) as db:
            db.executescript(fixture.read_text(encoding="utf-8"))
        model = ShellModel()
        env = clean_env(directory)
        deadline = time.monotonic() + timeout
        port, children = start_server(stack, server, state, env, {"alice": alice, "bob": bob},
                                      model, deadline)
        call = partial(request, port, deadline)
        sid = create_space(call, alice, "Shell")
        agents = call(alice, "GET", f"{SPACES}/{quote(sid, safe='')}/agents", read_json=True)
        for tool in ("workspace.write", "workspace.shell"):
            require(tool in agents["default_tools"] and tool in agents["agents"][0]["tools"],
                    f"{tool} is not on by default")

        # Exercise the existing agent, not the freshly created one.
        space = f"{SPACES}/s-0000000000000001"
        room = f"{space}/rooms/lobby"
        call(alice, "POST", f"{space}/members", body={"user_id": "bob"})
        token = issue_token(call, alice, f"{space}/runner-token", "issue the runner token")
        children.append(start_child(stack, "runner", runner, state, env | {
            "METRO_RUNNER_TOKEN": token,
            "METRO_SERVER_URL": f"http://{HOST}:{port}",
            "METRO_WORKSPACE": str(workspace),
        }))

        def ask(text):
            call(bob, "POST", f"{room}/messages", body={"text": f"@agent {text}", "invoke_agent": True})

        def reply_after(after):
            event, _ = wait_events(call, alice, room, lambda e: e["kind"] == "agent",
                                   children, model, deadline, after)
            return event

        def idle():
            while True:
                runs = call(alice, "GET", "/v1/runs", read_json=True)["runs"]
                if all(r["status"] != "running" for r in runs):
                    return
                time.sleep(min(0.05, remaining(deadline)))

        # Write needs no approval.
        ask("write the note")
        reply = reply_after(0)
        require("wrote 17 bytes to notes/hello.txt" in reply["text"], "write did not report")
        require((workspace / "notes" / "hello.txt").read_text() == "hello from metro\n",
                "the runner did not write the file")
        idle()

        # Shell waits for the owner; Bob (a member) can't approve.
        mark = reply["id"]
        ask("show the note")
        event, _ = wait_events(call, alice, room, lambda e: e["kind"] == "shell_approval",
                               children, model, deadline, mark)
        approval = approval_of(event)
        decide = f"{space}/shell-approvals/{approval['approval_id']}"
        call(bob, "POST", decide, body={"decision": "approve"}, expect=(403,),
             label="a member who isn't the approver")
        call(alice, "POST", decide, body={"decision": "approve"}, label="the owner approves")
        call(alice, "POST", decide, body={"decision": "deny"}, expect=(409,),
             label="a second decision")
        reply = reply_after(event["id"])
        for needle in ("exit: 0", "hello from metro", "dumb", str(workspace.resolve())):
            require(needle in reply["text"], f"shell output is missing {needle!r}")
        idle()

        # A stopped run kills its running command through the heartbeat.
        call(alice, "PUT", f"{space}/shell-policy", body={"shell_mode": "allow", "approver": "owner"})
        ask("sleep for a while")
        while not (workspace / "started").exists():
            check_processes(children, model)
            time.sleep(min(0.05, remaining(deadline)))
        runs = call(alice, "GET", "/v1/runs", read_json=True)["runs"]
        running = [r["id"] for r in runs if r["status"] == "running"]
        require(len(running) == 1, "expected one running run")
        call(alice, "POST", f"/v1/runs/{running[0]}/stop", body={})
        time.sleep(min(5, remaining(deadline)))
        require(not (workspace / "finished").exists(), "the stopped command kept running")
        check_processes(children, model)
    print("PASS: upgraded agent received write and shell; write needed no approval; "
          "shell waited for the owner and ran in the workspace "
          "with TERM=dumb; stopping the run killed the command.")


if __name__ == "__main__":
    signal.signal(signal.SIGTERM, terminate)
    raise SystemExit(main(run_shell, description=__doc__))
