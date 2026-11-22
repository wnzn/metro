# EmbeddingGemma 2 media embedding reference service

This is an optional, self-hosted service that embeds text queries and uploaded documents, images,
audio and video with [`google/embeddinggemma-2`](https://huggingface.co/google/embeddinggemma-2)
into one shared 768-dimension vector space. It's a small Python model worker. Metro's core stays
in Rust, and this service isn't part of the default `compose.yaml` stack.

> **Metro adapter, not a standard.** `POST /v1/media/embeddings` is Metro's own contract. Google
> doesn't define it, and it isn't the OpenAI API. Other servers that host this model (for example
> vLLM's `/v1/embeddings` with `messages`) use a different wire, and nobody has tested them as
> drop-in replacements. `GET /v1/models` and the text-only `POST /v1/embeddings` follow the
> OpenAI shape.

The model takes no native PDF input, so this service extracts PDF page text or renders the page
first (see below). Video is embedded from visual frames only: **the audio track of a video is not
encoded.**

## API

All POST bodies are JSON. Unknown fields are rejected. When `EG2_API_KEY_FILE` is set, every
route except `/healthz` needs `Authorization: Bearer <key>`.

| Route | Purpose |
| --- | --- |
| `GET /healthz` | `{"status":"ok","model":"google/embeddinggemma-2"}` |
| `GET /v1/models` | `{"object":"list","data":[{"id":"google/embeddinggemma-2",...,"metro":{"dimensions":768,"mrl_dimensions":[...],"modalities":[...],"revision":...}}]}` |
| `POST /v1/embeddings` | Text only, OpenAI-shaped: `{model, input: string \| string[], dimensions?, input_type?: "query" \| "document"}`. `input_type` is a Metro extension and defaults to `document`. `usage` is always 0, because tokens aren't counted. |
| `POST /v1/media/embeddings` | Exactly one of `{model, query: string}` or `{model, asset: {name, mime_type, data_base64}}`, plus an optional `dimensions`. |

Response of `/v1/media/embeddings`:

```json
{
  "model": "google/embeddinggemma-2",
  "data": [
    {"index": 0, "embedding": [0.01, ...], "modality": "text",
     "locator": {"kind": "page", "page": 1, "start": 0, "end": 1180},
     "excerpt": "First 240 characters of the extracted text..."}
  ],
  "partial": false,
  "notes": ["..."]
}
```

- `embedding` is L2-normalised, 768 dimensions by default. `dimensions` can be 512, 256 or 128,
  using Matryoshka truncation followed by re-normalisation. Queries and documents must use the
  same dimension.
- `locator.kind` is one of `query`, `text`, `page`, `image` or `time`.
  - `start`/`end` are code-point offsets into the extracted text: the whole TXT/MD/DOCX text, or
    that one PDF page's text.
  - `page` is 1-based.
  - `start_ms`/`end_ms` are times from the start of the decoded stream.
- `excerpt` appears only on segments whose text was really extracted. Images, rendered pages,
  audio and video never carry one.
- `modality` (`text`, `image`, `audio` or `video`) and `notes` are additive fields. `notes`
  explains partial results, rendered pages and ignored video audio.
- `partial: true` means the asset was longer than the limits, so only a leading part of it was
  embedded.

## What each format becomes

The pinned `config_sentence_transformers.json` sets the prefixes. Each one is applied exactly
once:

- Query text gets `SearchQuery`, which is `task: search result | query: `.
- Document chunks get `Document`, which is `title: none | text: `.
- Images, audio and video get no prefix.

| MIME type | What is embedded | Locator |
| --- | --- | --- |
| `text/plain`, `text/markdown` | UTF-8 text in chunks of up to 1,200 characters, cut at a paragraph, line or space boundary. | `text` with `start`/`end` |
| `application/vnd.openxmlformats-officedocument.wordprocessingml.document` | Body paragraph text from `word/document.xml` only. Relationships, external templates, headers, embedded objects and fields aren't read. | `text` with `start`/`end` |
| `application/pdf` | A page with at least 32 non-space characters of extractable text becomes text chunks. A page without them, such as a scanned page, is rendered (forms and annotations off, at most 1,536 px per side and 2.4 MP) and embedded as an **image**. No OCR is done, and `notes` says how many pages were rendered. | `page` with `page` (plus `start`/`end` for text) |
| `image/png`, `image/jpeg` | The decoded RGB pixels (first frame only), downscaled to at most 1,536 px per side. | `image` |
| `audio/wav`, `audio/mpeg` | Mono 16 kHz audio (each channel resampled, then averaged), cut into 8 s windows. The pinned processor caps one input at 280 tokens × 40 ms, about 11.2 s. A trailing remainder under 0.5 s is dropped and flagged as partial. | `time` |
| `video/mp4`, `video/webm` | Visual frames sampled at 1 fps (the model's default), grouped into fixed 16 s clips of at most 16 frames (the processor allows 32). Frames are downscaled to at most 768 px per side. **The audio track isn't embedded.** Allowed codecs: H.264, HEVC, VP8, VP9, AV1, MPEG-4. | `time` |

Nothing is inferred from the file name. `asset.name` must be a bare basename. It's validated and
then ignored: never opened, fetched or embedded.

## Limits and safety

`metro_eg2/limits.py` holds the authoritative values.

- **Size and segments.** Uploads are at most 8 MiB after base64 decoding, and the raw request
  body is capped at 12 MiB before it's buffered. One asset yields at most 48 segments, and more
  than that sets `partial`.
- **Format checks.** The declared `mime_type` must be on the allowlist above and match the
  file's magic bytes. Otherwise the service returns 415.
- **Decoder isolation (Linux, fail closed).** The decoders (FFmpeg via PyAV, pdfium, Pillow,
  numpy) never run in the service process. Each asset gets a fresh `python -I` worker
  (`metro_eg2/sandbox.py`, `decoder_worker.py`, `isolation.py`):
  - **Environment.** The environment is built from an allowlist only (`PATH`, locale,
    `HOME`=`TMPDIR`=its own workdir, thread caps). No parent variables reach it: no key-file
    path, no HF settings, no proxies. Only stdin and stdout are open, stderr goes to
    `/dev/null`, and it runs in its own session.
  - **Rlimits.** `RLIMIT_AS` 3 GiB, `RLIMIT_CPU` 90 s, `RLIMIT_FSIZE` 0, `RLIMIT_NOFILE` 64,
    and no core dumps.
  - **Then, before importing any decoder library and while still single-threaded:**
    `PR_SET_DUMPABLE=0` and `no_new_privs`.
  - **Landlock (ABI 3 or later required).** Read and execute are allowed only beneath the
    Python runtime, the `metro_eg2` package and system library directories. Read and write are
    allowed only in its private `0700` temp directory, plus `/dev/null` and `/dev/urandom`.
    Everything else is denied by the kernel: `/proc` (other processes' environ and memory),
    home directories, mounted secrets and sibling jobs' temp directories. With ABI 4 or later,
    TCP bind and connect are denied. With ABI 6 or later, signals and abstract UNIX sockets
    outside the sandbox are denied.
  - **seccomp-BPF, synced to all threads.** The wrong architecture or the x32 ABI kills the
    worker. These return `EPERM`:
    - sockets of every family;
    - `ptrace`, `process_vm_*`, `kcmp` and pidfd calls;
    - `execve`/`execveat`, `fork`/`vfork`, and `clone` without `CLONE_THREAD` (`clone3`
      returns `ENOSYS`);
    - signals to any pid other than its own;
    - mount, namespace, keyring, bpf, perf, io_uring, module and kexec calls;
    - chmod, chown, utime and xattr changes.
  - **Failure.** If any step fails, the worker reports it and exits without reading the asset.
    The request returns `503 sandbox_unavailable`, and at startup the service refuses to start.
  - **Untrusted output.** The worker's output is treated as untrusted (`metro_eg2/wire.py`): no
    pickle, only length-prefixed JSON with raw arrays. Every type, dtype, shape, size, locator
    key, string length and status is validated before anything reaches the model.
- **Admission and deadlines.** One pool of `EG2_MAX_CONCURRENCY` slots (default 1) covers the
  whole path for every POST route: base64 decoding, the decoder worker and each encoder call.
  - With no free slot, the request gets `503` with `Retry-After: 5` immediately.
  - Each admitted request has a server-side deadline, `EG2_REQUEST_TIMEOUT_S`. The default is
    150 s, and it's capped at 170 s, below the Metro library's 180 s indexing budget
    (`INDEX_TIMEOUT`). The lower-level media request timeout in `metro-model-protocol`
    (`MEDIA_TIMEOUT`) is 240 s. The deadline is enforced by this service only, so OS, network
    or client delays can still affect when a response arrives, or whether it does. At the
    deadline the request gets `504`, and if the client disconnects it is cancelled (logged as
    `499`).
  - In both cases the worker's process group is SIGKILLed and reaped, and no further segments
    are encoded.
  - The slot is released only after that and after any encoder call already in progress
    returns. Encoder calls can't be interrupted, but each is bounded by the segment caps.
  - Each slot can hold one worker of up to 3 GiB, plus the model (size the memory to match).
- **Inputs stay in memory.** Assets only ever exist in memory. The service never opens a URL,
  stream, file path or external reference:
  - MP4 data references are disabled (`enable_drefs=0`).
  - pdfium runs without a form environment, so PDF JavaScript and XFA never run, and embedded
    files aren't opened.
  - DOCX XML is parsed with `defusedxml`, which forbids DTDs, entities and external references.
    The zip entry count, uncompressed size and compression ratio are all checked.
- **Bounded decoding.** Every decoder is bounded:
  - image side and pixel count
  - PDF pages, page size and render pixels
  - audio channels and source sample rate
  - video width, height, frame count and duration
- **Typed text input.** User text always reaches Sentence Transformers as a typed `{"text": ...}`
  input. A bare string that looks like a URL or an existing path would otherwise be treated as
  media to fetch or open (`sentence_transformers.base.modality.infer_modality`).
- **No logging of user data.** Request bodies, assets and keys are never logged. The access log
  is off by default, and `EG2_ACCESS_LOG=1` turns it on for method, path and status only.
  Validation errors name fields and never echo values.
- **Precision.** Weights run in `float32` (the CPU default) or `bfloat16`. `float16` is refused,
  because this model overflows in it.
- **Remote code.** The model uses native Transformers classes (`EmbeddingGemma2Model`), with
  `trust_remote_code=False`.

## Configuration

| Variable | Default | Meaning |
| --- | --- | --- |
| `EG2_BIND` | `127.0.0.1:8795` (Docker image: `0.0.0.0:8795`) | Listen address. Publish it only on loopback or a private network. |
| `EG2_MODEL_DIR` | `/models/embeddinggemma-2` | Local snapshot directory. It's read offline, and nothing is downloaded at runtime. |
| `EG2_VERIFY_SHA256` | `1` | Check `model.safetensors` against the pinned sha256 at startup. |
| `EG2_DTYPE` | `float32` | `float32` or `bfloat16` |
| `EG2_DEVICE` | `cpu` | Torch device |
| `EG2_MODALITIES` | `text,image,audio,video` | Encoders to load. For example, `text,image` skips the 300M audio encoder. A disabled modality returns 422. |
| `EG2_THREADS` | torch default | CPU threads |
| `EG2_API_KEY_FILE` | unset | Mode-600 file holding a key of at least 16 characters. When set, a bearer token is required. **Required whenever `EG2_BIND` isn't loopback**, and the service refuses to start without it. |
| `EG2_TRUSTED_PRIVATE_NETWORK` | unset | `1` allows a non-loopback bind without a key. Use it only when the port is reachable solely from a trusted private network. |
| `EG2_REQUEST_TIMEOUT_S` | `150` | Per-request deadline, at most 170. |
| `EG2_MAX_CONCURRENCY` | `1` | Admission slots (1 to 8) for decode plus inference. |
| `EG2_DECODE_TMPDIR` | system temp | Parent directory for the per-worker `0700` temp directories. |
| `EG2_ACCESS_LOG` | `0` | `1` logs method, path and status only. |

Pinned versions:

- Model: `google/embeddinggemma-2` at revision `f9972ac22e696a105c482202646979e33e4b5a1e`, with
  `model.safetensors` sha256 `197a3296…3d79`.
- Libraries (locked in `uv.lock`): torch 2.15.0+cpu, torchvision 0.29.2+cpu, transformers 5.24.0,
  sentence-transformers 6.3.0, PyAV 19.0.2 (FFmpeg 9.0.2), pypdfium2 5.15.2, Pillow 13.0.0.

## Host and runtime requirements

The service refuses to start, and media requests fail closed, unless all of these hold:

- **Kernel.** Linux 6.2 or later, with `landlock` in the active LSM list
  (`cat /sys/kernel/security/lsm`; on some distributions you have to add it to the `lsm=` boot
  parameter). The architecture must be x86_64 or aarch64.
- **Container runtime.** Its seccomp profile must allow `landlock_create_ruleset`,
  `landlock_add_rule`, `landlock_restrict_self` and `seccomp`, and the process must be allowed
  to install its own filter.
  - Verified: Docker 29.9.1 with its built-in default profile, run with `--read-only`,
    `--cap-drop ALL`, `--security-opt no-new-privileges`, a non-root uid, `--pids-limit` and
    `--memory`.
  - No privileges, extra capabilities, Docker socket or nested containers are needed, and none
    should be given.
  - Not verified: Podman, Kubernetes `RuntimeDefault`, gVisor or other runtimes. Check them with
    the test image (below) before use.
- **Libraries.** No extra system libraries. FFmpeg, pdfium and libjpeg/libpng come bundled in
  the locked wheels.

## Setup and run

The model weights are about 1.53 GB, and downloading them is an intentional, one-time step:

```bash
services/embeddinggemma2/scripts/fetch_weights.sh ~/models/embeddinggemma-2   # verifies sha256
```

Docker (CPU, no weights in the image):

```bash
cd services/embeddinggemma2
docker build --target test .            # weight-free test suite inside the image
docker build --target runtime -t metro-eg2 .
install -m 600 /dev/null ~/.config/metro-eg2.key && openssl rand -hex 32 > ~/.config/metro-eg2.key
docker run --rm --read-only --tmpfs /tmp:rw,nosuid,nodev,noexec --cap-drop ALL \
  --security-opt no-new-privileges --pids-limit 256 --memory 8g \
  -p 127.0.0.1:8795:8795 \
  -v ~/models/embeddinggemma-2:/models/embeddinggemma-2:ro \
  -v ~/.config/metro-eg2.key:/run/secrets/eg2.key:ro -e EG2_API_KEY_FILE=/run/secrets/eg2.key \
  metro-eg2
```

The key file has to be readable by the image's user (uid 10001), for example after
`chown 10001`, or through a Docker/Compose secret. The image is about 2.3 GB, mostly CPU torch.

Plan for about 3 GiB of disk for the weights and **8 to 16 GiB of free RAM** at float32. These
are estimates, not measurements, until the real smoke below records them.

Without Docker:

```bash
cd services/embeddinggemma2 && uv sync --frozen
EG2_MODEL_DIR=~/models/embeddinggemma-2 .venv/bin/python -I -c \
  "import sys; sys.path.insert(0, '.'); from metro_eg2.main import main; main()"
```

## Tests and checks

| Command | Needs weights | What it proves |
| --- | --- | --- |
| `docker run --rm --read-only --tmpfs /tmp --cap-drop ALL --security-opt no-new-privileges --network none <test-image> /opt/venv/bin/python -I -m pytest -q -p no:cacheprovider tests` | no | The same suite under the runtime's hardening. Use it to check a new host or runtime. |
| `uv run --frozen pytest -q` | no | Decoders on synthetic media, real locators and excerpts, and limits. Also the API contract, admission and deadlines, and wire validation. The isolation tests run the real `isolation.apply()` in a clean-env worker and try only benign actions against test-created sentinels. These must be denied: a sibling file, `/proc/<parent>/environ`, a loopback TCP listener, a signal to the parent, ptrace and `process_vm_readv` on the parent, fork and exec. These must still work: threads, its own workdir and importing numpy. They also check fail-closed behaviour, kill-and-reap on timeout and cancel, and temp-directory cleanup. It uses `tests/fake_encoder.py`, a **test-only** hash encoder with no semantics that the service never imports. |
| `python -I scripts/plumbing_check.py <pinned-config-dir> <scratch>` | no (pinned config and tokenizer only) | Runs the real `SentenceTransformerEncoder` and the real processor over every modality on a **random-init, 2-layer** copy of the architecture: shapes, normalisation, MRL, 1 fps frame passing, and exact parity of the prefix path with ST's `prompt_name` path. It makes **no quality claim**. |
| `python -I scripts/real_smoke.py --model-dir <snapshot> [--all]` | **yes** (opt-in) | Starts the real service on loopback (CPU, offline, GPUs hidden) and checks that the text queries "a red circle on white" and "a blue square on white" each rank the matching synthetic PNG first. `--all` also dispatches WAV, MP4, WebM and PDF. It prints cosines, ranks, versions, peak RSS and timings. |

**Known limits.**

- The seccomp filter is a deny-list layered on Landlock's allow-list. System calls added to
  future kernels are allowed unless they're listed.
- The worker runs as the same uid as the service. Separation comes from the kernel controls
  above, not from a different user.
- The model process itself isn't sandboxed. It only receives validated arrays and text.
- An encoder call in progress can't be cancelled.
- Landlock and seccomp aren't available on macOS or Windows, so the service decodes nothing
  there.

Record each run, with its output, in Metro's `docs/acceptance.md`. Until the real smoke has
been recorded there, retrieval quality on this service is unverified.
