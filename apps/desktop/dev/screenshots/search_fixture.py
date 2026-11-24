#!/usr/bin/env python3
"""A deterministic loopback stand-in for the typed endpoints of the Search
screenshot (capture-discovery.sh). A test double, not a vendor: keyless,
127.0.0.1 only.

- GET  /v1/models      -> {"data": [{"id": "fixture-embed"}], "models": [{"name": "fixture-system-one"}]}
- POST /v1/embeddings  -> hashed bag-of-words vectors (64 dims, L2-normalised)
- POST /v1/systemone   -> one `noul` per question: the word overlap between
                          state.search_query and state.candidate_message

It is NOT a semantic model, and its scores say nothing about search quality.
They only let the real metro-server run its real embedding, indexing and
System One rerank code paths on ordinary sentences. (typed_fixtures.py, the
e2e vendors, rank by SEARCH_* marker words, which would show in the shot.)

    search_fixture.py PORT_FILE
Writes the port it listens on to PORT_FILE.
"""

import hashlib
import json
import math
import re
import sys
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

DIM = 64
MAX_BODY = 4 * 1024 * 1024


def words(text):
    out = []
    for w in re.findall(r"[a-z0-9]+", str(text).lower()):
        if len(w) > 3 and w.endswith("s"):
            w = w[:-1]
        if len(w) > 2:
            out.append(w)
    return out


def embed(text):
    v = [0.0] * DIM
    v[0] = 0.05  # never the zero vector
    for w in words(text):
        h = int(hashlib.sha256(w.encode()).hexdigest(), 16)
        v[1 + h % (DIM - 1)] += 1.0
    n = math.sqrt(sum(x * x for x in v))
    return [x / n for x in v]


def overlap(query, candidate):
    q, c = set(words(query)), set(words(candidate))
    if not q:
        return 0.05
    return round(min(0.97, 0.05 + 0.9 * len(q & c) / len(q)), 4)


class Handler(BaseHTTPRequestHandler):
    def log_message(self, fmt, *args):
        sys.stderr.write("fixture " + (fmt % args) + "\n")

    def send(self, status, body):
        data = json.dumps(body).encode()
        self.send_response(status)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(data)))
        self.end_headers()
        self.wfile.write(data)

    def do_GET(self):
        if self.path.rstrip("/") == "/v1/models":
            return self.send(200, {"data": [{"id": "fixture-embed"}], "models": [{"name": "fixture-system-one"}]})
        self.send(404, {"error": "not found"})

    def do_POST(self):
        length = int(self.headers.get("Content-Length") or 0)
        if length > MAX_BODY:
            return self.send(413, {"error": "too large"})
        try:
            body = json.loads(self.rfile.read(length) or b"{}")
        except ValueError:
            return self.send(400, {"error": "bad json"})
        path = self.path.rstrip("/")
        if path == "/v1/embeddings":
            inputs = body.get("input")
            if isinstance(inputs, str):
                inputs = [inputs]
            data = [{"object": "embedding", "index": i, "embedding": embed(t)} for i, t in enumerate(inputs or [])]
            return self.send(200, {"object": "list", "model": body.get("model"), "data": data})
        if path == "/v1/systemone":
            state = body.get("state") or {}
            q = state.get("search_query", "") if isinstance(state, dict) else ""
            c = state.get("candidate_message", "") if isinstance(state, dict) else json.dumps(state)
            answers = {}
            for name, question in (body.get("questions") or {}).items():
                if question.get("type") != "noul":
                    return self.send(400, {"error": "fixture answers noul only"})
                answers[name] = {"type": "noul", "noul": overlap(q, c)}
            return self.send(200, {"model": body.get("model"), "answers": answers, "usage": {"input_tokens": 1, "output_tokens": 1}})
        self.send(404, {"error": "not found"})


def main():
    server = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
    with open(sys.argv[1], "w") as f:
        f.write(str(server.server_address[1]))
    server.serve_forever()


if __name__ == "__main__":
    main()
