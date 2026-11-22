"""HTTP API. Metro's adapter contract for media embedding; not a Google or OpenAI standard
(``/v1/models`` and ``/v1/embeddings`` follow the OpenAI shape for text only)."""

from __future__ import annotations

import asyncio
import base64
import binascii
import hashlib
import hmac
import logging
import threading
import time
from collections.abc import Callable, Iterator
from concurrent.futures import ThreadPoolExecutor
from typing import Any, Literal

from fastapi import FastAPI, Request
from fastapi.exceptions import RequestValidationError
from fastapi.responses import JSONResponse
from pydantic import BaseModel, ConfigDict, Field

from .decode import AssetError, Segment, validate_name
from .encoder import MODEL_ID, MRL_DIMS, NATIVE_DIM, Encoder, ModalityDisabled
from .limits import DEFAULT_LIMITS, Limits
from .sandbox import Done, decode_in_sandbox
from .wire import Cancelled, DecodeTimeout, IsolationFailed

log = logging.getLogger("metro_eg2")

# decoder(raw, mime_type, limits, deadline, cancel_event) -> Segment..., Done
Decoder = Callable[..., Iterator[Segment | Done]]


class ApiError(Exception):
    def __init__(self, status: int, message: str, code: str = "invalid_request_error"):
        self.status, self.message, self.code = status, message, code


# ----------------------------------------------------------------------------- schemas


class Asset(BaseModel):
    model_config = ConfigDict(extra="forbid")
    name: str
    mime_type: str
    data_base64: str


class MediaRequest(BaseModel):
    model_config = ConfigDict(extra="forbid")
    model: str
    query: str | None = None
    asset: Asset | None = None
    dimensions: int | None = None


class OpenAIEmbeddingsRequest(BaseModel):
    model_config = ConfigDict(extra="forbid")
    model: str
    input: str | list[str]
    dimensions: int | None = None
    encoding_format: Literal["float"] | None = None
    user: str | None = Field(default=None, exclude=True)  # accepted and ignored (never logged)
    input_type: Literal["query", "document"] | None = None  # Metro extension; default "document"


# ----------------------------------------------------------------------------- body limit


class BodyLimit:
    """Reject bodies over ``max_bytes`` before they are buffered, whatever Content-Length says."""

    def __init__(self, app, max_bytes: int):
        self.app, self.max_bytes = app, max_bytes

    async def __call__(self, scope, receive, send):
        if scope["type"] != "http":
            return await self.app(scope, receive, send)
        for k, v in scope.get("headers", []):
            if k == b"content-length":
                try:
                    if int(v) > self.max_bytes:
                        return await _too_large(send)
                except ValueError:
                    return await _too_large(send)
        seen = 0

        async def limited():
            nonlocal seen
            msg = await receive()
            if msg["type"] == "http.request":
                seen += len(msg.get("body", b""))
                if seen > self.max_bytes:
                    raise _BodyTooLarge()
            return msg

        try:
            await self.app(scope, limited, send)
        except _BodyTooLarge:
            await _too_large(send)


class _BodyTooLarge(Exception):
    pass


async def _too_large(send):
    body = b'{"error":{"message":"request body too large","type":"invalid_request_error","code":"payload_too_large"}}'
    await send({"type": "http.response.start", "status": 413, "headers": [(b"content-type", b"application/json")]})
    await send({"type": "http.response.body", "body": body})


# ----------------------------------------------------------------------------- app


MAX_REQUEST_TIMEOUT_S = 170.0  # Metro's media client gives up at 180 s; always answer first.


def create_app(
    encoder: Encoder,
    *,
    limits: Limits = DEFAULT_LIMITS,
    api_key: str | None = None,
    decoder: Decoder = decode_in_sandbox,
    request_timeout_s: float = 150.0,
    max_concurrency: int = 1,
    revision: str | None = None,
):
    """Build the ASGI app.

    Admission: one bounded pool of ``max_concurrency`` slots covers the *whole* request path
    (base64 decode, isolated decoder process, every encoder call). A request that finds no free
    slot gets 503 immediately. A slot is released only when its worker thread has finished, i.e.
    after the decoder process has been killed and reaped and any in-flight encoder call has
    returned, never while native work is still running.

    Deadline: each admitted request gets ``request_timeout_s`` (<= 170 s). At the deadline, or
    when the client disconnects, the request is cancelled: the decoder process group is killed,
    no further segments are encoded, and the client gets 504 (if still connected). An encoder
    call already running cannot be interrupted; it finishes (bounded by the input caps) and only
    then is the slot freed.
    """
    if not (0 < request_timeout_s <= MAX_REQUEST_TIMEOUT_S):
        raise ValueError(f"request_timeout_s must be in (0, {MAX_REQUEST_TIMEOUT_S}]")
    if not (1 <= max_concurrency <= 8):
        raise ValueError("max_concurrency must be 1..8")
    app = FastAPI(title="Metro EmbeddingGemma 2 reference service", docs_url=None, redoc_url=None, openapi_url=None)
    slots = threading.BoundedSemaphore(max_concurrency)
    executor = ThreadPoolExecutor(max_workers=max_concurrency, thread_name_prefix="eg2-job")
    encode_lock = threading.Lock()
    key_digest = hashlib.sha256(api_key.encode()).digest() if api_key else None
    app.state.slots = slots  # for tests / introspection

    def check_auth(request: Request) -> None:
        if key_digest is None:
            return
        header = request.headers.get("authorization", "")
        token = header[7:] if header[:7].lower() == "bearer " else ""
        if not hmac.compare_digest(hashlib.sha256(token.encode()).digest(), key_digest):
            raise ApiError(401, "missing or invalid API key", "invalid_api_key")

    def check_model(model: str) -> None:
        if model != MODEL_ID:
            raise ApiError(404, f"model {model!r} is not served here; use {MODEL_ID!r}", "model_not_found")

    def check_dim(dim: int | None) -> int:
        if dim is None:
            return NATIVE_DIM
        if dim not in MRL_DIMS:
            raise ApiError(422, f"dimensions must be one of {list(MRL_DIMS)}")
        return dim

    def encode(kind: str, payload: Any, dim: int, job: _Job) -> list[float]:
        job.check()
        if not encode_lock.acquire(timeout=max(0.0, job.deadline - time.monotonic())):
            raise DecodeTimeout()
        try:
            job.check()
            vec = encoder.encode(kind, payload, dim)
        except ModalityDisabled as exc:
            raise ApiError(422, f"the {exc.modality} encoder is disabled on this server (EG2_MODALITIES)") from None
        finally:
            encode_lock.release()
        return [float(x) for x in vec]

    async def run(request: Request, work: Callable[[_Job], Any]) -> Any:
        if not slots.acquire(blocking=False):
            raise ApiError(503, "all embedding slots are busy; retry later", "server_busy")
        job = _Job(time.monotonic() + request_timeout_s)

        def wrapped():
            try:
                return work(job)
            finally:
                slots.release()

        try:
            fut = asyncio.get_running_loop().run_in_executor(executor, wrapped)
        except BaseException:
            slots.release()
            raise
        fut.add_done_callback(lambda f: f.cancelled() or f.exception())  # consume late errors
        while True:
            done, _ = await asyncio.wait({fut}, timeout=0.2)
            if done:
                break
            if time.monotonic() >= job.deadline:
                job.cancel.set()
                raise ApiError(504, "processing exceeded the time limit", "timeout")
            if await request.is_disconnected():
                job.cancel.set()
                raise ApiError(499, "client disconnected", "client_closed")
        try:
            return fut.result()
        except DecodeTimeout:
            raise ApiError(504, "processing exceeded the time limit", "timeout") from None
        except Cancelled:
            raise ApiError(499, "client disconnected", "client_closed") from None

    @app.exception_handler(ApiError)
    async def _api_error(_request: Request, exc: ApiError):
        headers = {"Retry-After": "5"} if exc.status == 503 else None
        return JSONResponse(
            {"error": {"message": exc.message, "type": "invalid_request_error", "code": exc.code}},
            status_code=exc.status,
            headers=headers,
        )

    @app.exception_handler(RequestValidationError)
    async def _validation(_request: Request, exc: RequestValidationError):
        # Report field locations only; never echo submitted values (they may be base64 user data).
        fields = sorted({".".join(str(p) for p in e.get("loc", ()) if p != "body") for e in exc.errors()})
        return JSONResponse(
            {"error": {"message": f"invalid request body: {', '.join(fields) or 'malformed JSON'}", "type": "invalid_request_error", "code": "invalid_body"}},
            status_code=422,
        )

    @app.get("/healthz")
    async def healthz():
        return {"status": "ok", "model": MODEL_ID}

    @app.get("/v1/models")
    async def models(request: Request):
        check_auth(request)
        entry: dict[str, Any] = {"id": MODEL_ID, "object": "model", "owned_by": "google"}
        meta: dict[str, Any] = {"dimensions": NATIVE_DIM, "mrl_dimensions": list(MRL_DIMS), "modalities": sorted(encoder.modalities)}
        if revision:
            meta["revision"] = revision
        entry["metro"] = meta
        return {"object": "list", "data": [entry]}

    @app.post("/v1/embeddings")
    async def openai_embeddings(request: Request, body: OpenAIEmbeddingsRequest):
        check_auth(request)
        check_model(body.model)
        dim = check_dim(body.dimensions)
        inputs = [body.input] if isinstance(body.input, str) else body.input
        if not inputs or len(inputs) > limits.max_openai_inputs:
            raise ApiError(422, f"input must hold 1..{limits.max_openai_inputs} strings")
        if any(not s or len(s) > limits.max_query_chars for s in inputs):
            raise ApiError(422, f"each input must be 1..{limits.max_query_chars} characters")
        kind = "query" if body.input_type == "query" else "document"

        def work(job: _Job):
            return [{"object": "embedding", "index": i, "embedding": encode(kind, s, dim, job)} for i, s in enumerate(inputs)]

        data = await run(request, work)
        return {"object": "list", "data": data, "model": MODEL_ID, "usage": {"prompt_tokens": 0, "total_tokens": 0}}

    @app.post("/v1/media/embeddings")
    async def media_embeddings(request: Request, body: MediaRequest):
        check_auth(request)
        check_model(body.model)
        dim = check_dim(body.dimensions)
        if (body.query is None) == (body.asset is None):
            raise ApiError(422, "send exactly one of query or asset")

        if body.query is not None:
            q = body.query
            if not q.strip() or len(q) > limits.max_query_chars:
                raise ApiError(422, f"query must be 1..{limits.max_query_chars} characters")
            vec = await run(request, lambda job: encode("query", q, dim, job))
            return {"model": MODEL_ID, "data": [{"index": 0, "embedding": vec, "locator": {"kind": "query"}, "modality": "text"}], "partial": False}

        asset = body.asset
        assert asset is not None
        try:
            validate_name(asset.name, limits)
        except AssetError as exc:
            raise ApiError(exc.status, exc.message) from None
        max_b64 = (limits.max_decoded_bytes + 2) // 3 * 4
        if len(asset.data_base64) > max_b64 + 4:
            raise ApiError(413, f"asset exceeds {limits.max_decoded_bytes} decoded bytes", "payload_too_large")
        b64, mime = asset.data_base64, asset.mime_type

        def work(job: _Job):
            try:
                raw = base64.b64decode(b64, validate=True)
            except (binascii.Error, ValueError):
                raise ApiError(422, "asset.data_base64 is not valid base64") from None
            if len(raw) > limits.max_decoded_bytes:
                raise ApiError(413, f"asset exceeds {limits.max_decoded_bytes} decoded bytes", "payload_too_large")
            data: list[dict[str, Any]] = []
            partial, notes = False, []
            gen = decoder(raw, mime, limits, job.deadline, job.cancel)
            del raw
            try:
                for item in gen:
                    job.check()
                    if isinstance(item, Done):
                        partial, notes = item.partial, item.notes
                        break
                    kind = "document" if item.modality == "text" else item.modality
                    entry: dict[str, Any] = {
                        "index": len(data),
                        "embedding": encode(kind, item.payload, dim, job),
                        "locator": item.locator,
                        "modality": item.modality,
                    }
                    if item.excerpt:
                        entry["excerpt"] = item.excerpt
                    data.append(entry)
            except AssetError as exc:
                code = {413: "payload_too_large", 415: "unsupported_media_type"}.get(exc.status, "invalid_asset")
                raise ApiError(exc.status, exc.message, code) from None
            except IsolationFailed:
                log.error("decoder isolation unavailable; refusing to decode (fail closed)")
                raise ApiError(503, "the decoder sandbox is unavailable on this host; media decoding is disabled", "sandbox_unavailable") from None
            finally:
                gen.close()  # kills and reaps the decoder process before the slot is released
            if not data:
                raise ApiError(422, "the asset produced no embeddable segments", "invalid_asset")
            return data, partial, notes

        data, partial, notes = await run(request, work)
        out: dict[str, Any] = {"model": MODEL_ID, "data": data, "partial": partial}
        if notes:
            out["notes"] = notes
        return out

    return BodyLimit(app, limits.max_body_bytes)


class _Job:
    def __init__(self, deadline: float):
        self.deadline = deadline
        self.cancel = threading.Event()

    def check(self) -> None:
        if self.cancel.is_set():
            raise Cancelled()
        if time.monotonic() >= self.deadline:
            raise DecodeTimeout()
