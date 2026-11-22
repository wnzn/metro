"""Entry point: ``python -I -m metro_eg2.main``.

Loads the pinned weights from a local directory only (no Hub access at runtime), then serves
the API with uvicorn. Request bodies, assets and keys are never logged; the access log is off
unless ``EG2_ACCESS_LOG=1`` (it records method, path and status only).
"""

from __future__ import annotations

import logging
import os
import stat
import sys


def _read_key(path: str) -> str:
    st = os.stat(path)
    if st.st_mode & (stat.S_IRWXG | stat.S_IRWXO):
        raise SystemExit(f"EG2_API_KEY_FILE must not be readable by group/others (chmod 600 {path})")
    with open(path, encoding="utf-8") as fh:
        key = fh.read().strip()
    if len(key) < 16:
        raise SystemExit("EG2_API_KEY_FILE must hold a key of at least 16 characters")
    return key


def _sandbox_self_test(log) -> None:
    import time

    from .limits import DEFAULT_LIMITS
    from .sandbox import Done, IsolationFailed, decode_in_sandbox

    try:
        items = list(decode_in_sandbox(b"sandbox self-test", "text/plain", DEFAULT_LIMITS, time.monotonic() + 60))
    except IsolationFailed as exc:
        raise SystemExit(f"decoder sandbox unavailable, refusing to start: {exc.message}") from None
    if not items or not isinstance(items[-1], Done):
        raise SystemExit("decoder sandbox self-test failed")
    log.info("decoder sandbox self-test passed (Landlock + seccomp + rlimits)")


def main() -> None:
    # Offline by construction: the service never downloads at runtime.
    os.environ.setdefault("HF_HUB_OFFLINE", "1")
    os.environ.setdefault("TRANSFORMERS_OFFLINE", "1")
    os.environ.setdefault("HF_HUB_DISABLE_TELEMETRY", "1")
    logging.basicConfig(level=logging.INFO, format="%(asctime)s %(levelname)s %(name)s: %(message)s")
    log = logging.getLogger("metro_eg2")

    import uvicorn

    from .app import MAX_REQUEST_TIMEOUT_S, create_app
    from .encoder import load_from_env
    from .isolation import IsolationUnavailable, set_nondumpable

    # Same-uid processes (including our own decoder workers) may not ptrace this process or read
    # its memory/environ through /proc.
    try:
        set_nondumpable()
    except IsolationUnavailable as exc:
        raise SystemExit(f"cannot harden the service process: {exc}") from None

    bind = os.environ.get("EG2_BIND", "127.0.0.1:8795")
    host, _, port = bind.rpartition(":")
    host = host.strip("[]") or "127.0.0.1"
    key_file = os.environ.get("EG2_API_KEY_FILE")
    api_key = _read_key(key_file) if key_file else None
    loopback = host in ("127.0.0.1", "::1", "localhost")
    if api_key is None and not loopback:
        if os.environ.get("EG2_TRUSTED_PRIVATE_NETWORK") != "1":
            raise SystemExit(
                f"refusing to serve on {host} without EG2_API_KEY_FILE; set a key, bind to loopback, "
                "or set EG2_TRUSTED_PRIVATE_NETWORK=1 if this port is reachable only from a trusted private network"
            )
        log.warning("serving on %s WITHOUT an API key (EG2_TRUSTED_PRIVATE_NETWORK=1)", host)

    timeout = float(os.environ.get("EG2_REQUEST_TIMEOUT_S", "150"))
    if not (0 < timeout <= MAX_REQUEST_TIMEOUT_S):
        raise SystemExit(f"EG2_REQUEST_TIMEOUT_S must be in (0, {MAX_REQUEST_TIMEOUT_S:g}] (Metro's client waits 180 s)")
    concurrency = int(os.environ.get("EG2_MAX_CONCURRENCY", "1"))

    # Fail closed at startup if the decoder sandbox cannot be established on this kernel/runtime.
    _sandbox_self_test(log)

    encoder = load_from_env()
    log.info("loaded %s revision %s dtype %s modalities %s", encoder.model_id, encoder.revision, encoder.dtype, ",".join(sorted(encoder.modalities)))
    app = create_app(
        encoder,
        api_key=api_key,
        request_timeout_s=timeout,
        max_concurrency=concurrency,
        revision=encoder.revision,
    )
    del api_key
    uvicorn.run(
        app,
        host=host,
        port=int(port),
        workers=1,
        access_log=os.environ.get("EG2_ACCESS_LOG") == "1",
        log_level="info",
        server_header=False,
        proxy_headers=False,
    )


if __name__ == "__main__":
    sys.exit(main())
