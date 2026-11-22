# syntax=docker/dockerfile:1
# Metro media embedding reference service (google/embeddinggemma-2), CPU build.
# No model weights in the image: mount a pinned snapshot at /models/embeddinggemma-2.

ARG PYTHON_IMAGE=python:3.13-slim-bookworm@sha256:a5a7017efc5969e00ae2b91c8eb0631049a569652aa4ffc4501769049ae45c72
ARG UV_IMAGE=astral/uv:0.13.5@sha256:32a7bb39b496babbd997b0a39a4d66c126bdaf5661b302bf6f06abda8deca6ff

FROM ${UV_IMAGE} AS uv

FROM ${PYTHON_IMAGE} AS deps
COPY --from=uv /uv /usr/local/bin/uv
ENV UV_PROJECT_ENVIRONMENT=/opt/venv UV_LINK_MODE=copy UV_PYTHON_DOWNLOADS=never UV_COMPILE_BYTECODE=1
WORKDIR /src
COPY pyproject.toml uv.lock ./
RUN uv sync --frozen --no-dev --no-install-project --python /usr/local/bin/python3

# `docker build --target test .` runs the weight-free test suite (fake encoder, real decoders).
FROM deps AS test
RUN uv sync --frozen --no-install-project --python /usr/local/bin/python3
COPY metro_eg2 ./metro_eg2
COPY tests ./tests
RUN useradd --uid 10001 --create-home eg2 && chown -R eg2 /src
USER 10001
RUN /opt/venv/bin/python -I -m pytest -q -p no:cacheprovider tests

FROM ${PYTHON_IMAGE} AS runtime
RUN useradd --uid 10001 --no-create-home --home-dir /nonexistent --shell /usr/sbin/nologin eg2
COPY --from=deps /opt/venv /opt/venv
# Into site-packages, because `python -I` (isolated mode) ignores the cwd and PYTHONPATH.
COPY metro_eg2 /opt/venv/lib/python3.13/site-packages/metro_eg2
WORKDIR /tmp
ENV PATH=/opt/venv/bin:$PATH \
    PYTHONDONTWRITEBYTECODE=1 \
    PYTHONUNBUFFERED=1 \
    HF_HUB_OFFLINE=1 \
    TRANSFORMERS_OFFLINE=1 \
    HF_HUB_DISABLE_TELEMETRY=1 \
    HF_HOME=/tmp/hf \
    EG2_BIND=0.0.0.0:8795 \
    EG2_MODEL_DIR=/models/embeddinggemma-2 \
    EG2_DTYPE=float32 \
    EG2_DEVICE=cpu
USER 10001
EXPOSE 8795
HEALTHCHECK --interval=30s --timeout=5s --start-period=180s --retries=3 \
  CMD ["python", "-I", "-c", "import urllib.request,sys;sys.exit(0 if urllib.request.urlopen('http://127.0.0.1:8795/healthz',timeout=4).status==200 else 1)"]
CMD ["python", "-I", "-m", "metro_eg2.main"]
