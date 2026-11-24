# Building Metro from source (contributors)

This page is for people who work on Metro itself. **You don't need anything
here to install or use Metro.** Ordinary users and operators should follow
the [README](../README.md), [deploy.md](deploy.md), [runners.md](runners.md)
and [settings.md](settings.md), which use prebuilt files only.

Everything below builds from a checkout of
<https://github.com/wnzn/metro>.

## Toolchain

- Rust (the version pinned by `Dockerfile`'s `rust-base` stage) for
  `crates/`.
- Docker with Compose v2 for the local stack and images.
- [Native Shell](https://nsh.dev) 0.6.x for `apps/desktop` (see
  [the desktop requirements](#requirements)).
- `python3` and `openssl` for the e2e scripts and the token helpers.

## Local stack from source

`compose.yaml` builds the server and runner images from this checkout. This
is the contributor path; operators use the published images instead
([deploy.md](deploy.md)).

```sh
D="${XDG_CONFIG_HOME:-$HOME/.config}/metro-dev"
mkdir -p "$D" && chmod 700 "$D"
( umask 077
  for u in alice bob; do [ -e "$D/$u.token" ] || openssl rand -hex 32 > "$D/$u.token"; done
  if [ -e .env ]; then
    echo ".env already exists; check that METRO_DEV_USERS matches $D" >&2
  else
    printf "METRO_DEV_USERS='{\"alice\":\"%s\",\"bob\":\"%s\"}'\nMETRO_ORG_ADMINS='[\"alice\"]'\n" \
      "$(cat "$D/alice.token")" "$(cat "$D/bob.token")" > .env
  fi )
docker compose up --build -d
curl -fsS http://127.0.0.1:8787/healthz
```

Tokens go to mode-600 files and are never printed. `.env` is git-ignored.

### Two people, one Space, from a shell

```sh
BASE=http://127.0.0.1:8787
ALICE_TOKEN="$(cat "$D/alice.token")"; BOB_TOKEN="$(cat "$D/bob.token")"
api() { tok="$1" method="$2" path="$3" body="${4:-}"
  curl -fsS -X "$method" -H "Authorization: Bearer $tok" \
    ${body:+-H 'Content-Type: application/json' -d "$body"} "$BASE$path"; }
json() { python3 -I -c 'import json,sys; print(json.load(sys.stdin)[sys.argv[1]])' "$1"; }

SPACE="$(api "$ALICE_TOKEN" POST /v1/spaces '{"name":"Alice and Bob"}' | json id)"
BOB_ID="$(api "$BOB_TOKEN" GET /v1/me | json id)"
api "$ALICE_TOKEN" POST "/v1/spaces/$SPACE/members" "{\"user_id\":\"$BOB_ID\"}"
```

### Bundled dedicated runner, built locally

```sh
TOKEN_FILE="$D/$SPACE.runner-token"
[ -e "$TOKEN_FILE" ] && echo "$TOKEN_FILE exists; issuing again would rotate the token" >&2 \
  || ( umask 077; api "$ALICE_TOKEN" POST "/v1/spaces/$SPACE/runner-token" '{}' | json token > "$TOKEN_FILE" )
METRO_RUNNER_TOKEN="$(cat "$TOKEN_FILE")" docker compose --profile runner up --build -d runner
```

It serves `examples/workspace` with `METRO_SANDBOX=none`. The registered
runner service is `docker compose --profile reusable-runner up --build -d
registered-runner`, with the credential in `METRO_REGISTERED_RUNNER_TOKEN`.

To stop: `docker compose --profile runner down` (add `-v` only to delete
the database).

## Images and binaries

```sh
docker build --target server -t metro-server:local .
docker build --target runner -t metro-runner:local .   # --build-arg RUNNER_UID=… RUNNER_GID=…
cargo build --locked --release --bin metro-server --bin metro-runner --bin metro-connector
```

A locally built runner image can be used anywhere the docs say
`<runner-image>`. The `RUNNER_UID`/`RUNNER_GID` build arguments are the only
way to change the image's built-in user; the published image uses
1000:1000.

## The desktop app

The app lives in `apps/desktop` and is written in TSX on
[Native Shell](https://nsh.dev). Users install the prebuilt app instead
([apps/desktop/README.md](../apps/desktop/README.md)).

### Requirements

- **NSH 0.6.x.** Install the official release with
  `curl -fsSL https://nsh.dev/install.sh | sh`, which installs to `~/.nsh` and
  links `~/.local/bin/nsh`, or extract
  `nsh-linux-x86_64.tar.gz` from <https://github.com/longbridge/nsh/releases>.
  `nsh.json` pins `>=0.6, <0.7`.
- **Linux window (`nsh run` / `--target linux`):** glibc ≥ 2.39, GTK ≥ 4.14,
  and **WebKitGTK 6.0** (`libwebkitgtk-6.0-4`). The NSH Linux host links
  WebKitGTK even though Metro shows no web views. GStreamer is optional.
- **Headless (`nsh test`, `nsh check`, `nsh build`):** only `nsh`. These run
  anywhere, CI included.
- macOS 14+ and Windows 10 1809+ follow NSH's platform support. Metro hasn't
  been run on either. A Windows x64 package has been built and checked on
  Linux; see [apps/desktop/README.md](../apps/desktop/README.md#windows-portable-package).

### Commands

Run these from `apps/desktop`.

```sh
nsh check                 # TypeScript 7 type check (tsc downloaded once by nsh)
nsh test                  # headless tests, hermetic: network stubbed, no server needed
nsh run App.tsx           # open the app in a native window (needs the Linux requirements above)
nsh package App.tsx --target linux   # .nsh/out/Metro/ and .nsh/out/Metro.tar.gz (NSH's prebuilt release runner)
```

To work on the UI without the Rust server, use the dev double. It is the
same in-memory fake the tests use. It has no agent, runner or persistence,
so never use it as evidence:

```sh
node dev/mock-server.mjs       # Node >= 22.18; users alice, bob, carol (fake tokens in test/fakeServer.ts)
```

### Screenshots (native Linux, isolated)

Run these from `apps/desktop`.

`dev/screenshots/` runs the real `metro-server` and `metro-runner` on loopback
with a temp database, workspace and tokens. It drives the app in NSH's GTK
host on a private Xvfb display inside a container. The screenshot steps:

1. Bob @mentions Alice from his app.
2. Alice's app captures the connect screen before any secret is entered.
3. Alice reacts, posts, runs Demo (one collapsed Runner activity group), and
   opens Manage.
4. With a model: Alice asks `@agent what is in the workspace?`. One real Rig
   turn calls `workspace.list` on the runner. `agent-thinking.png` shows the
   turn live and collapsed ("Metro is thinking…", 2 steps) while the model
   holds its answer. `agent-activity-expanded.png` shows the finished turn
   ("Metro’s activity") opened to its call and listing.
5. Alice opens Settings. Runners shows a personal and an organization runner,
   both created through the API. Model providers saves a keyless connection
   to the allowlisted local fixture, lists its models and picks one. Device
   onboarding creates a device connection and shows its token masked, with
   the connector shown as offline. Then Appearance, and the Website Space,
   which the organization runner serves (a second real `metro-runner` with a
   `metro_rt_v2_` token).

Tokens and the provider master key (`METRO_PROVIDER_KEY_FILE`) live only in
mode-600 files inside the temp directory, which is removed on exit. The
script fails if any of them turns up under the output directory or the app.

```sh
docker build -t metro-desktop-shots:trixie dev/screenshots
dev/screenshots/capture.sh ../../docs/screenshots      # NSH_DIST=… if nsh isn't on PATH; METRO_BIN=… for other binaries
```

The agent shots (`agent-thinking.png`, `agent-activity-expanded.png`,
`chat-runs.png`, `thread.png`) need a model. By default
the script starts `scripted_model.py`, a deterministic local OpenAI-compatible
stand-in for the model only. The server, Rig, the tool call, the runner and
its listing are all real, and the stand-in's replies say "Scripted test
model". To use a real model instead, set `MODEL_ENV_FILE` to a mode-600 file
of `METRO_MODEL*` lines. To skip the agent shots, set `SCRIPTED_MODEL=0`.
The scripted model also answers a keyless `GET /v1/models`. It is the only
allowlisted provider endpoint (`METRO_PROVIDER_ALLOWED_BASE_URLS`), so
`settings-providers.png` and `settings-device-onboarding.png` are captured
only with it.

### Model routing and sharing screenshots

`dev/screenshots/capture-routing.sh` runs the real `metro-server` with four
loopback fixture vendors from `tests/e2e/intelligence.py` (System One,
embeddings and two OpenAI-compatible chat endpoints; `typed_fixtures.py`).
Nothing reaches a real vendor. Alice and Bob's connections, the shares and
Alice's routing are set up through the API with throwaway keys. Then the
app, from a disposable copy in the temp directory (so the step file `nsh
test` writes, which holds the typed token, never lands in the checkout),
captures `model-routing.png`, `model-routing-dataflow.png`,
`model-sharing.png` and `route-attribution.png`. The last one is a real
turn: Alice's unaddressed message, picked up for the Space's agent by the
System One fixture under her opt-in for that Space (`classify_unaddressed`
in `PUT /v1/spaces/{sid}/prefs`; her `infer_addressing` covers only her
personal Chat), and routed to a use case that Bob's shared connection
answers.
The script fails if a fixture rejected a request, or if any token or key
turns up under the output or the app.

```sh
dev/screenshots/capture-routing.sh ../../docs/screenshots   # same image and NSH_DIST / METRO_BIN as above
```

## metro-connector (developer CLI)

`crates/connector` builds `metro-connector`, the outbound-only process that
serves `device-*` connections. In this build, device connections still need
it. Making the desktop app run on-device integrations itself, so that
ordinary users never install or start a separate terminal process, is in
progress; see [settings.md](settings.md#device-connections). Until a release
says otherwise, treat everything in this section as contributor tooling.

```sh
cargo install --locked --path crates/connector     # or: cargo build --locked --release --bin metro-connector
```

It only makes outbound connections, to the Metro server and to the provider,
and never follows redirects. Both URLs must be `https://`, or `http://` to
this machine only (`localhost`, `127.0.0.0/8`, `::1`).

### Setup and run

`metro-connector setup` asks for everything it needs, with hidden input for
secrets. Use one **profile** per device connection created in Metro. Secrets
are never accepted as command-line arguments: pass the device token and any
API key as owner-only files, or as `-` for stdin.

```sh
# Save the device token without echoing it
( umask 077; IFS= read -rs -p 'Device token: ' t; echo
  printf '%s\n' "$t" > "$HOME/metro-device-token.txt" )

# Your own OpenAI-compatible endpoint, with your own key
metro-connector setup --profile work --server-url https://metro.example.com \
  --kind device-openai-compatible --base-url https://api.example.com/v1 \
  --device-token-file ~/metro-device-token.txt --api-key-file ~/provider-key.txt \
  --model my-model --yes

# A local server on this machine that needs no key
metro-connector setup --profile local --server-url https://metro.example.com \
  --kind device-openai-compatible --base-url http://127.0.0.1:11434/v1 --no-api-key \
  --device-token-file - --model llama3 --yes < ~/metro-device-token.txt

metro-connector run --profile work     # Ctrl-C to stop; --once for one poll cycle
```

Setup copies the token and key into the profile, so delete the originals
afterwards. Metro can use only model ids approved on the machine (`--model`,
repeatable, or `metro-connector allow`); `run` refuses to start without one.

| Command | What it does |
| --- | --- |
| `metro-connector models --profile work` | Lists the provider's models, marking the approved ones |
| `metro-connector allow --profile work MODEL...` | Replaces the approved list |
| `metro-connector login --profile work` | Signs in again, or replaces the API key (`--api-key-file`, `--no-api-key`) |
| `metro-connector logout --profile work` | Deletes the provider key or sign-in from this computer, keeps the device token |
| `metro-connector forget --profile work [--yes]` | Deletes the whole profile, device token included. Revoke the connection in Metro too. |
| `metro-connector status --profile work` | Kind, server, endpoint, whether a key and token are saved, approved models. Never shows secrets. |
| `metro-connector list` | Lists profiles |

- **Files.** `$METRO_CONNECTOR_HOME` (default `~/.config/metro-connector`, or
  `%APPDATA%\metro-connector` on Windows) holds `profiles/<name>/` with
  `profile.json`, `device-token`, `provider-key` and `vendor/`. Directories
  are mode 700 and files 600. No OS keystore is used.
- **Environment.** `METRO_CONNECTOR_HOME`, `METRO_CONNECTOR_PROFILE`,
  `METRO_CONNECTOR_POLL_MS` (default 2000, clamped 50–30000). Failed polls
  back off up to 30 seconds.
- **Time limits.** `request_timeout_secs` in `profile.json`: 240 by default,
  5–270. The server gives a device job 300 seconds.
- **Credentials stay with their endpoint.** Re-running `setup` with another
  base URL or kind asks for new credentials and never reuses the old ones.
- **Exit codes.** `0` ok, `1` error, `2` usage, `3` device token revoked or
  replaced (create a new token in Metro and run `setup` again).
- **Kind-specific sign-in.** `setup --kind device-openai-oauth` starts the
  Sign in with ChatGPT browser flow at the end unless `--no-login`;
  `setup --kind device-claude-code` asks for a Claude Console API key, or
  reads `METRO_CLAUDE_CODE_KEY_FILE`, and finds `claude` on `PATH` or at
  `METRO_CLAUDE_CODE_BIN`. What each kind does and doesn't do is in
  [settings.md](settings.md#device-connections).

## Media embedding service (`compose.media.yaml`)

This optional override builds the reference `google/embeddinggemma-2`
service and the server from this checkout. No media-service image is
published, so it's contributor and self-build tooling, not part of the
install path. What Documents & media does with it is in
[library.md](library.md).

The reference service (`services/embeddinggemma2/`) runs
`google/embeddinggemma-2` with Sentence Transformers. It's an **override
file**, not part of the default stack, and the Metro images contain neither
torch nor the weights. What the override sets up:

- **Isolation.** The service sits on an internal-only `media` network,
  using the bridge's `isolated` IPv4 gateway mode, which needs a recent
  Docker Engine. It publishes no host port. Only the Metro server joins
  that network as well.
- **Hardening.** The container is read-only, with all capabilities dropped,
  `no-new-privileges` and at most 256 processes. Its memory is capped by
  `METRO_EG2_MEMORY_LIMIT`, which defaults to `8g`. That's a ceiling you
  can change, not a measured requirement of the real weights. The one-shot
  import container is capped at 256 MiB. Landlock started under Docker's
  default profile in this stack, so no privileges or extra capabilities
  are needed.
- **Offline.** `HF_HUB_OFFLINE=1` and `TRANSFORMERS_OFFLINE=1` are set, so
  the service never downloads anything.
- **Weights.** They come from a project-scoped named volume,
  `media-model-cache`, which is mounted read-only for serving.
- **Key.** A bearer key is required in this stack. It's mounted as a Compose
  secret, `metro_eg2_api_key`.

**1. Get the pinned snapshot once, on a trusted machine.** It's about 1.5
GB, public and ungated (Apache-2.0), so you don't need a Hugging Face
token:

```sh
hf download google/embeddinggemma-2 \
  --revision f9972ac22e696a105c482202646979e33e4b5a1e \
  --local-dir /absolute/path/to/embeddinggemma-2
```

**2. Import it into the named cache.** The import is one-shot, runs with no
network, and gets no key. It refuses to overwrite a cache that's already
prepared. The snapshot directory must be readable by the image's non-root
user.

```sh
METRO_EG2_MODEL_SOURCE=/absolute/path/to/embeddinggemma-2 \
  docker compose -f compose.yaml -f compose.media.yaml \
  run --rm --build embeddinggemma2-prepare
```

Use the same Compose project name (`-p`, if you set one) for both this step
and the next.

**3. Make the service key.** It's a separate, strong key, and never the
provider master key. The service runs as UID 10001, and Compose doesn't
remap file ownership for secrets. So the file must be readable by that UID
and by nobody else:

```sh
( umask 077; openssl rand -hex 32 > /absolute/path/to/eg2.key )
sudo chown 10001:10001 /absolute/path/to/eg2.key   # stays mode 600
```

You'll paste the same key into the connection in step 5. Read it from the
file when you need it, and don't put it in shell history or logs.

**4. Allow the endpoint, then start.** Add `http://embeddinggemma2:8795/v1`
to your existing `METRO_PROVIDER_ALLOWED_BASE_URLS` array in `.env`. If you
have none yet, use `'["http://embeddinggemma2:8795/v1"]'`. The override
doesn't change the allowlist for you.

```sh
METRO_EG2_API_KEY_HOST_FILE=/absolute/path/to/eg2.key \
  docker compose -f compose.yaml -f compose.media.yaml \
  up -d --build server embeddinggemma2
```

**5. Create the connection.** In **Settings**, choose **EmbeddingGemma 2 on
an approved endpoint**. Use the base URL `http://embeddinggemma2:8795/v1`
and the model `google/embeddinggemma-2`, and paste the same key. Then choose
it as your media model under **Settings › Model providers › Documents &
media**.

- The Metro **server** resolves and calls this address, not your browser
  or connector.
- If the server uses an HTTP proxy, add `embeddinggemma2` to its proxy
  bypass.

**Other ways to run it:**

- **A remote HTTPS endpoint you trust, running the same service.** Use the
  base stack, allow that endpoint's exact base URL, and give its URL and key
  in the connection.
- **On your device** (contributor tooling in this build; see
  [settings.md](settings.md#device-connections)). Run `metro-connector` with `--kind
  device-embeddinggemma-2`, a `--base-url` reachable from that machine (not
  the Compose hostname), `--model google/embeddinggemma-2` and
  `--api-key-file`. Use HTTPS to a remote Metro server.
- Don't expose the Compose sidecar on the host to make either of these work.

**Service settings:**

- `EG2_DEVICE` is `cpu` by default.
- `EG2_DTYPE` is `float32` by default, or `bfloat16`. `float16` is refused.
- `EG2_VERIFY_SHA256` is on by default. At startup, it checks the weights
  against the pinned checksums.
- `EG2_MODALITIES` turns off encoders you don't need. A file of a disabled
  modality gets `422`.
- `EG2_MAX_CONCURRENCY` sets the number of admission slots, from 1 (the
  default) to 8. One slot covers decoding and inference for one request.
  With no free slot, the service answers `503` with `Retry-After: 5`
  straight away. A slot is freed only after its worker is reaped, and after
  any encoder call already running has finished, because encoder calls
  can't be interrupted.
- `EG2_REQUEST_TIMEOUT_S` is the per-request deadline: 150 s by default, and
  at most 170 s. When it passes, the service answers `504`.
- `EG2_API_KEY_FILE` names a mode-600 file holding a key of at least 16
  characters. The key is **required whenever the service listens on a
  non-loopback address**, and without it the service won't start. The one
  exception is `EG2_TRUSTED_PRIVATE_NETWORK=1`. Use that only when the port
  can be reached solely from a trusted private network. The Compose
  override always sets a key.
- The other settings are `EG2_THREADS`, `EG2_DECODE_TMPDIR` (the parent
  directory of the per-worker temp directories) and `EG2_ACCESS_LOG`. When
  `EG2_ACCESS_LOG=1`, the log holds method, path and status only.
- Dimensions are 768 by default. 512, 256 and 128 are allowed as well.
- The service is written not to log request bodies, files or keys, and its
  validation errors name fields without echoing values. No log scan has
  been recorded.
- Pinned: the revision `f9972ac22e696a105c482202646979e33e4b5a1e`, and the
  sha256 of `model.safetensors`. The libraries are locked in `uv.lock`:
  torch 2.15.0+cpu, transformers 5.24.0, sentence-transformers 6.3.0, PyAV
  19.0.2, pypdfium2 5.15.2 and Pillow 13.0.0. The model loads with
  `trust_remote_code=False`.

**The service's protocol.** This is Metro's adapter contract, not a Google
or OpenAI standard:

| Route | What it does |
| --- | --- |
| `GET /healthz` | `{"status":"ok","model":"google/embeddinggemma-2"}`, without auth |
| `GET /v1/models` | The model id, plus a `metro` object with its dimensions, MRL dimensions, enabled modalities and revision |
| `POST /v1/embeddings` | **Text only**, OpenAI-shaped: `input` holds up to 64 strings, with an optional `input_type` of `query` or `document`. Message search can use it. It never stands in for media. |
| `POST /v1/media/embeddings` | Exactly one of `{model, query}` or `{model, asset: {name, mime_type, data_base64}}`, with an optional `dimensions` |

`POST /v1/media/embeddings` returns `{model, data: [{index, embedding,
locator, modality, excerpt?}], partial, notes?}`:

- `excerpt` is at most 240 characters, and appears only on text that was
  really extracted. A rendered page, an image, audio or video never has
  one.
- `notes` explain things like a video's audio track not being embedded.
- The prefixes come from the pinned Sentence Transformers config, and are
  applied exactly once.
- Unknown fields get `422`.

| Status | When |
| --- | --- |
| `401` | A missing or wrong key |
| `404` | The wrong model |
| `413` | Over 8 MiB decoded, or a body over 12 MiB |
| `415` | A MIME type that isn't allowed, or one that doesn't match the file's magic bytes |
| `422` | Invalid or undecodable input |
| `503` | No admission slot is free (`Retry-After: 5`), or the decoder sandbox can't be applied (`sandbox_unavailable`) |
| `504` | The request deadline passed |

**Optional real smoke, not run yet.** This step downloads the real weights
(about 1.53 GB, checked against their sha256). It then runs offline on CPU
at `float32`, with GPUs hidden, using synthetic PNGs only. Two text queries
must each rank the matching shape first. It reports cosines, ranks, peak
RSS, timings and versions. No result has been recorded, so this page makes
no claim about it.

```sh
cd services/embeddinggemma2
scripts/fetch_weights.sh ~/.cache/metro-eg2/weights
TMPDIR=~/.cache/metro-eg2/tmp UV_PROJECT_ENVIRONMENT=~/.cache/metro-eg2/venv \
  uv run --frozen python -I scripts/real_smoke.py \
  --model-dir ~/.cache/metro-eg2/weights --threads 4
```

Add `--all` to the last command to include audio and video. Passing the
smoke would show that the pretrained model runs, and that it gets two
synthetic shapes right. It wouldn't measure retrieval quality on real files.

**Planning numbers, not measurements:**

- Allow about 3 GiB of disk for the weights in the cache, plus about 2.3 GB
  for the runtime image (the measured build was 2.27 GB).
- On CPU, reserve about 8 GiB of free RAM for short text and image work,
  and 12–16 GiB with every encoder loaded.
- Each admission slot can also hold a decoder worker of up to 3 GiB. Size
  the memory to match, and raise `EG2_MAX_CONCURRENCY` only if you have it.

The RAM figures are estimates from the checkpoint size. The 2.2 GiB peak
RSS seen with the tiny random model says nothing about the real one.

## Tests and acceptance

`cargo test --workspace`, the scripts in `tests/e2e/`, and `nsh test` in
`apps/desktop`. How each recorded run was done, and its results, are in
[acceptance.md](acceptance.md). Record every new verification run there.

A self-hosted Linux runner for the repository's own CI (unrelated to Metro's
Space runners) is described in
[ops/github-runner/README.md](../ops/github-runner/README.md).
