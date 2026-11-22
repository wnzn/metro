#!/usr/bin/env python3
"""Loopback vendors for the Model routing and sharing screenshots.

Starts the e2e fixtures of tests/e2e/intelligence.py (`Vendor`): one System
One endpoint (POST /v1/systemone, GET /v1/models), one OpenAI-compatible
embeddings endpoint and two OpenAI-compatible chat endpoints, each with its
own random key and model, all on 127.0.0.1. These are test doubles, not
vendors. Their base URLs, models and keys go to OUT (created mode 600) and
nowhere else; the script then serves until it is terminated.

    typed_fixtures.py OUT
"""

from contextlib import ExitStack
import json
import os
from pathlib import Path
import signal
import sys
import threading

E2E = Path(__file__).resolve().parents[4] / "tests" / "e2e"
sys.path.insert(0, str(E2E))

from intelligence import Vendor  # noqa: E402
from settings import serve_fixture  # noqa: E402


def main(out):
    stop = threading.Event()
    signal.signal(signal.SIGTERM, lambda *_: stop.set())
    signal.signal(signal.SIGINT, lambda *_: stop.set())
    with ExitStack() as stack:
        vendors = {
            "systemone": serve_fixture(stack, Vendor("systemone", "classifier")),
            "embeddings": serve_fixture(stack, Vendor("embeddings", "embeddings")),
            "team": serve_fixture(stack, Vendor("chat", "default")),
            "local": serve_fixture(stack, Vendor("chat", "shared-local")),
        }
        info = {name: {"base_url": v.base_url, "model": v.model, "key": v.key} for name, v in vendors.items()}
        fd = os.open(out, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
        with os.fdopen(fd, "w") as f:
            json.dump(info, f)
        stop.wait()
        errors = [name for name, v in vendors.items() if v.error]
        if errors:
            print(f"fixture handler errors: {', '.join(errors)}", file=sys.stderr)
            return 1
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1]))
