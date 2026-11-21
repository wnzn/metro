"""Fake stdio MCP server for metro-runner tests.

Usage: python3 -I fake_mcp.py [normal|hang|badversion|notools|crash]
Speaks newline-delimited JSON-RPC 2.0 on stdin/stdout.
"""
import json
import os
import sys
import time

MODE = sys.argv[1] if len(sys.argv) > 1 else "normal"

TOOLS = [
    {"name": "echo", "description": "Echo text", "inputSchema": {"type": "object", "properties": {"text": {"type": "string"}}}},
    {"name": "fail", "description": "Always errors", "inputSchema": {"type": "object"}},
    {"name": "env", "description": "Report env", "inputSchema": {"type": "object"}},
    {"name": "page2", "description": "On second page", "inputSchema": {"type": "object"}},
]


def send(obj):
    sys.stdout.write(json.dumps(obj) + "\n")
    sys.stdout.flush()


def recv():
    line = sys.stdin.readline()
    if not line:
        sys.exit(0)
    return json.loads(line)


def main():
    print("fake-mcp starting", file=sys.stderr, flush=True)
    while True:
        msg = recv()
        method = msg.get("method")
        mid = msg.get("id")
        if method == "initialize":
            if MODE == "hang":
                time.sleep(3600)
            if MODE == "crash":
                print("fatal: boom", file=sys.stderr, flush=True)
                sys.exit(3)
            version = "1999-01-01" if MODE == "badversion" else msg["params"]["protocolVersion"]
            caps = {} if MODE == "notools" else {"tools": {}}
            # Unrelated notification before the response must be ignored.
            send({"jsonrpc": "2.0", "method": "notifications/message", "params": {"level": "info", "data": "hi"}})
            send({"jsonrpc": "2.0", "id": mid, "result": {
                "protocolVersion": version, "capabilities": caps,
                "serverInfo": {"name": "fake", "version": "0"}}})
        elif method == "notifications/initialized":
            pass
        elif method == "tools/list":
            cursor = (msg.get("params") or {}).get("cursor")
            if cursor is None:
                send({"jsonrpc": "2.0", "id": mid, "result": {"tools": TOOLS[:3], "nextCursor": "p2"}})
            else:
                send({"jsonrpc": "2.0", "id": mid, "result": {"tools": TOOLS[3:]}})
        elif method == "tools/call":
            p = msg["params"]
            name = p["name"]
            args = p.get("arguments") or {}
            if name == "echo":
                # Server-initiated ping must be answered before we respond.
                send({"jsonrpc": "2.0", "id": "srv-1", "method": "ping"})
                reply = recv()
                assert reply.get("id") == "srv-1" and reply.get("result") == {}, reply
                send({"jsonrpc": "2.0", "id": mid, "result": {"content": [{"type": "text", "text": "echo:" + args.get("text", "")}]}})
            elif name == "fail":
                send({"jsonrpc": "2.0", "id": mid, "result": {"content": [{"type": "text", "text": "it broke"}], "isError": True}})
            elif name == "env":
                keys = sorted(k for k in os.environ if k.startswith("METRO") or "TOKEN" in k)
                send({"jsonrpc": "2.0", "id": mid, "result": {"content": [{"type": "text", "text": "cwd=" + os.getcwd() + " keys=" + ",".join(keys)}]}})
            else:
                send({"jsonrpc": "2.0", "id": mid, "error": {"code": -32602, "message": "Unknown tool: " + name}})
        elif mid is not None:
            send({"jsonrpc": "2.0", "id": mid, "error": {"code": -32601, "message": "nope"}})


if __name__ == "__main__":
    main()
