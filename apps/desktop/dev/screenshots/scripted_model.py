#!/usr/bin/env python3
"""A deterministic OpenAI-compatible chat model for screenshot runs.

It is a test double for the MODEL only: metro-server, Rig, the tool call,
metro-runner and the workspace listing stay real. Screenshots that show its
replies must say so.

Behaviour per request (non-streaming /v1/chat/completions), keyed on the
newest user message and the tool results after it:
- "acknowledge" and a reaction tool: react 👀, then reply METRO_REACTION_ONLY
  (the server's "reaction, no text" marker);
- "what is in the workspace" and a listing tool: list the root, hold, summarize;
- "delegate" and thread_create: start a child thread "workspace-audit" whose
  task is to list the workspace, then confirm;
- a workspace listing tool and no result yet: list the workspace root;
- a listing result: wait HOLD seconds (so the run shows as running), then
  summarize the file names;
- otherwise: answer at once.

    python3 scripted_model.py PORT_FILE [HOLD_SECONDS]
Writes the port it listens on (127.0.0.1) to PORT_FILE.
"""

import json
import re
import sys
import time
import uuid
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

HOLD = float(sys.argv[2]) if len(sys.argv) > 2 else 20.0
MAX_BODY = 4 * 1024 * 1024


def text_of(content):
    if isinstance(content, str):
        return content
    if isinstance(content, list):
        return " ".join(text_of(c.get("text", "")) if isinstance(c, dict) else str(c) for c in content)
    return ""


def offered(tools, pattern):
    for tool in tools or []:
        fn = tool.get("function", {}) if tool.get("type") == "function" else {}
        if re.fullmatch(pattern, fn.get("name", ""), re.IGNORECASE):
            return fn
    return None


def arguments_for(fn):
    schema = fn.get("parameters", {}) or {}
    props = schema.get("properties", {}) or {}
    args = {}
    for name in schema.get("required", []) or []:
        prop = props.get(name, {})
        if name in ("path", "relative_path", "dir", "directory", "root"):
            args[name] = "."
        elif "default" in prop:
            args[name] = prop["default"]
        elif prop.get("type") == "boolean":
            args[name] = False
    for name in ("path", "relative_path", "dir", "directory"):
        if name in props and name not in args:
            args[name] = "."
            break
    return args


def call(fn, args):
    return {
        "role": "assistant",
        "content": None,
        "tool_calls": [{"id": f"call_{uuid.uuid4().hex}", "type": "function",
                        "function": {"name": fn["name"], "arguments": json.dumps(args)}}],
    }, "tool_calls"


def say(text):
    return {"role": "assistant", "content": text}, "stop"


def summarize_listing(listing):
    try:  # workspace.list answers JSON, possibly after a status line
        data = json.loads(listing[listing.index("{"):])
        names = [e["name"] + ("/" if e.get("type") == "dir" else "") for e in data.get("entries", [])][:8]
    except (ValueError, KeyError, TypeError, AttributeError):
        names = re.findall(r"[\w.-]+/?", listing)[:8]
    return "The workspace has " + (", ".join(names) if names else "no files") + \
        ". (Scripted test model; the listing came from the runner.)"


def complete(req):
    messages = req.get("messages", [])
    tools = req.get("tools")
    last_user = max((i for i, m in enumerate(messages) if m.get("role") == "user"), default=-1)
    latest = text_of(messages[last_user].get("content")).lower() if last_user >= 0 else ""
    results = [text_of(m.get("content")) for m in messages[last_user + 1:] if m.get("role") == "tool"]

    react = offered(tools, r"react[_.-]?to[_.-]?message|react")
    delegate = offered(tools, r"thread[_.-]create")
    listing = offered(tools, r"workspace[_.-]list")

    # The newest prompt may batch several unanswered messages: act on the last request in it.
    intents = ("acknowledge", "delegate", "what is in the workspace")
    wants = max(intents, key=lambda k: latest.rfind(k)) if any(k in latest for k in intents) else ""
    if wants == "what is in the workspace" and listing:
        # One turn that lists, holds (still live), then answers from the listing.
        if results:
            time.sleep(HOLD)
            message, finish = say(summarize_listing(results[-1].strip()))
        else:
            message, finish = call(listing, arguments_for(listing))
    elif wants == "acknowledge" and react:
        message, finish = say("METRO_REACTION_ONLY") if results else call(react, {"emoji": "👀"})
    elif wants == "delegate" and delegate:
        message, finish = (say("Started a thread for the workspace audit; its result will be posted here. (Scripted test model.)")
                           if results else
                           call(delegate, {"name": "workspace-audit", "task": "List the workspace and report the files."}))
    elif listing and not results:
        message, finish = call(listing, arguments_for(listing))
    elif results:
        time.sleep(HOLD)
        message, finish = say(summarize_listing(results[-1].strip()))
    else:
        message, finish = say("Noted. (Scripted test model.)")
    return {
        "id": f"chatcmpl-{uuid.uuid4().hex[:12]}",
        "object": "chat.completion",
        "created": int(time.time()),
        "model": req.get("model", "scripted"),
        "choices": [{"index": 0, "message": message, "finish_reason": finish}],
        "usage": {"prompt_tokens": 1, "completion_tokens": 1, "total_tokens": 2},
    }


class Handler(BaseHTTPRequestHandler):
    def log_message(self, *_):
        pass

    def do_GET(self):
        # Keyless model listing, so Settings › Model providers shows a real
        # (allowlisted, local) endpoint's list. It lists only this double.
        if self.path.rstrip("/").endswith("/models"):
            body, status = {"object": "list", "data": [
                {"id": "metro-scripted-test-model", "object": "model", "owned_by": "local-fixture"},
                {"id": "metro-scripted-test-model-small", "object": "model", "owned_by": "local-fixture"},
            ]}, 200
        else:
            body, status = {"error": {"message": f"unexpected path {self.path}"}}, 404
        data = json.dumps(body).encode()
        self.send_response(status)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(data)))
        self.end_headers()
        self.wfile.write(data)

    def do_POST(self):
        try:
            if not self.path.rstrip("/").endswith("/chat/completions"):
                raise ValueError(f"unexpected path {self.path}")
            size = int(self.headers.get("Content-Length", "0"))
            if not 0 < size <= MAX_BODY:
                raise ValueError("bad body size")
            body, status = complete(json.loads(self.rfile.read(size))), 200
        except Exception as exc:  # report, never crash the double
            body, status = {"error": {"message": str(exc)}}, 400
        data = json.dumps(body).encode()
        self.send_response(status)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(data)))
        self.end_headers()
        self.wfile.write(data)


if __name__ == "__main__":
    server = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
    with open(sys.argv[1], "w") as f:
        f.write(str(server.server_port))
    server.serve_forever()
