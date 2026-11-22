#!/usr/bin/env python3
"""Metro library → reference worker HTTP contract, with a test-only encoder."""

import base64
from contextlib import ExitStack
from functools import partial
import http.client
import json
import os
from pathlib import Path
import secrets
import signal
import subprocess
import sys
import time
from types import SimpleNamespace

from library import png
from smoke import (
    HOST, check_processes, clean_env, main, remaining, request, require,
    server_port, start_child, stop_child, temp_state, terminate, wait_for_server,
)


def worker_app():
    """Loaded only by Uvicorn in the Docker test stage, not the serving image."""
    from metro_eg2.app import create_app
    from tests.fake_encoder import FakeEncoder

    return create_app(FakeEncoder(), api_key=os.environ["REFERENCE_TEST_API_KEY"])


def run(server_binary, timeout):
    deadline = time.monotonic() + timeout
    user_key, worker_key = (secrets.token_hex(32) for _ in range(2))
    worker_port, metro_port = server_port(), server_port()
    while metro_port == worker_port:
        metro_port = server_port()
    worker_url = f"http://{HOST}:{worker_port}/v1"
    with temp_state() as directory, ExitStack() as stack:
        state = Path(directory)
        env = clean_env(directory)
        worker = subprocess.Popen(
            [sys.executable, "-B", "-m", "uvicorn", "--factory", "library_reference:worker_app",
             "--host", HOST, "--port", str(worker_port), "--no-access-log"],
            cwd=state, env=env | {
                "PYTHONPATH": os.pathsep.join((str(Path(__file__).resolve().parent), str(Path.cwd()))),
                "REFERENCE_TEST_API_KEY": worker_key,
                "HF_HUB_OFFLINE": "1",
                "TRANSFORMERS_OFFLINE": "1",
            },
            stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL,
        )
        stack.callback(stop_child, worker)
        children = [(worker, "reference worker")]
        fixture = SimpleNamespace(error=None)
        wait_for_server(worker_port, children, fixture, deadline)

        children.append(start_child(stack, "Metro server", server_binary, state, env | {
            "METRO_BIND": f"{HOST}:{metro_port}",
            "METRO_DB": str(state / "metro.sqlite"),
            "METRO_DEV_USERS": json.dumps({"alice": user_key}),
            "METRO_PROVIDER_MASTER_KEY": base64.b64encode(secrets.token_bytes(32)).decode(),
            "METRO_PROVIDER_ALLOWED_BASE_URLS": json.dumps([worker_url]),
        }))
        wait_for_server(metro_port, children, fixture, deadline)
        call = partial(request, metro_port, deadline, user_key)
        profile = call("POST", "/v1/providers", body={
            "name": "Reference test",
            "provider_id": "embeddinggemma-2",
            "model": "google/embeddinggemma-2",
            "base_url": worker_url,
            "api_key": worker_key,
        }, expect=(201,), read_json=True)
        call("PUT", "/v1/library/settings", body={"embedding_id": profile["id"]})

        def upload_and_index(name, content, mime):
            conn = http.client.HTTPConnection(HOST, metro_port, timeout=remaining(deadline))
            try:
                conn.request("POST", f"/v1/library/assets?scope=personal&name={name}", body=content,
                             headers={"Content-Type": mime, "Authorization": f"Bearer {user_key}"})
                with conn.getresponse() as response:
                    require(response.status == 201, f"reference asset upload: HTTP {response.status}")
                    asset_id = json.loads(response.read(65536))["id"]
            finally:
                conn.close()
            indexed = call("POST", "/v1/library/index", body={"asset_id": asset_id}, read_json=True)
            [item] = indexed["items"]
            require(item["asset_id"] == asset_id and item["status"] == "indexed"
                    and item["indexed_segments"] == 1 and not indexed["partial"],
                    f"reference worker did not index {name} as one complete segment")
            return asset_id

        text = b"A small synthetic document for the reference worker."
        document_id = upload_and_index("reference.txt", text, "text/plain")
        result = call("POST", "/v1/library/search",
                      body={"scope": "personal", "query": "synthetic document"}, read_json=True)
        [hit] = result["results"]
        require(result["ranking"] == "embeddings" and hit["asset_id"] == document_id
                and hit["locator"] == {"kind": "text", "start": 0, "end": len(text)}
                and hit["excerpt"] == text.decode(),
                "reference worker result did not preserve the extracted text and locator")

        image_id = upload_and_index("reference.png", png((63, 90, 137)), "image/png")
        result = call("POST", "/v1/library/search",
                      body={"scope": "personal", "query": "blue", "kind": "image"}, read_json=True)
        [hit] = result["results"]
        require(result["ranking"] == "embeddings" and hit["asset_id"] == image_id
                and hit["locator"] == {"kind": "image"} and hit.get("excerpt") is None,
                "reference worker image result did not preserve its locator without an excerpt")
        check_processes(children, fixture)
    print("PASS: Metro indexed and searched synthetic text and PNG through the reference worker HTTP API.")


if __name__ == "__main__":
    signal.signal(signal.SIGTERM, terminate)
    raise SystemExit(main(run, binaries=("metro-server",), description=__doc__))
