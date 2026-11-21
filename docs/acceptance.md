# Acceptance

What has been demonstrated, and how to reproduce it. Anything not listed here
hasn't been demonstrated.

## Demonstrated (2026-11-22, integrator, local)

| Check | Result |
| --- | --- |
| `cargo fmt --all -- --check` and `cargo clippy --workspace --all-targets --locked -- -D warnings` | pass |
| `cargo test --workspace --locked`: 155 tests, including the Docker sandbox tests (`METRO_TEST_DOCKER_IMAGE` set) | pass |
| `python3 tests/e2e/smoke.py --timeout 60`: real server and runner binaries | pass |
| `python3 tests/e2e/threads.py --timeout 60`: real server binary | pass |
| Compose from the current `Dockerfile`, as an isolated project (details below) | pass |
| `actionlint` on `.github/workflows/ci.yml` | pass |
| Desktop: `nsh check` clean, and `nsh test` 22/22 (headless, fake server) | pass |
| Desktop, native Linux GTK capture against the real server and runner on a dedicated Xvfb display: six screenshots, listed below | pass |

**Both e2e scripts use a local scripted model.** It is an OpenAI-compatible
stand-in that returns fixed replies and tool calls. The server, Rig, the
runner and the database are real. These scripts prove the wiring, not the
behaviour of any real model.

What the two scripts cover:

- **`smoke.py`:** Space privacy and membership, runner token rotation, and two
  runners, each listing only its own workspace's marker file.
- **`threads.py`:**
  - A parent agent creates a child thread, sends it a follow-up, and the
    child's results are mirrored to the parent.
  - Access to personal Chat and to runs.
  - An agent turn answered with a reaction alone.

**Compose (isolated project, no model):**

- The server became healthy with no runner and no model.
- A member's Demo in an unpaired Space returned `409`, and an outsider got
  `404`.
- The owner paired the bundled runner.
- Bob's Demo returned `202`, and the `tool_result` listed that runner's own
  fixture workspace.

**Desktop screenshots** (1200×780, native Linux GTK; the agent's replies come
from the labelled scripted model):
[room-runner](screenshots/room-runner.png),
[chat-runs](screenshots/chat-runs.png),
[thread](screenshots/thread.png),
[space-members](screenshots/space-members.png),
[connection](screenshots/connection.png),
[connection-error](screenshots/connection-error.png).
The native Linux app needs glibc ≥ 2.39, GTK ≥ 4.14 and WebKitGTK 6.0.

## Not demonstrated

- Deployment behind a real OIDC provider (Ory Hydra, Authelia or authentik).
  Auth is covered by `crates/auth`'s tests against a local stand-in provider.
- Inference with a real or paid model.
- Publishing images to GHCR from a `v*` tag.
- A full scan of logs and the database for leaked tokens. The code is written
  never to log tokens, but no exhaustive scan was done.
- The desktop app on macOS or Windows.

## Reproduce

Run from the repo root.

```sh
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo build --locked --bin metro-server --bin metro-runner
cargo test --workspace --locked
python3 tests/e2e/smoke.py --timeout 60
python3 tests/e2e/threads.py --timeout 60
```

The Docker sandbox tests in `cargo test` are skipped without Docker. To run
them in full, first load a local image pinned by digest that contains
`/bin/sh`, `id`, `ls`, `grep` and `sleep`:

```sh
docker pull busybox:1.37
export METRO_TEST_DOCKER_IMAGE="$(docker image inspect --format '{{index .RepoDigests 0}}' busybox:1.37)"
cargo test -p metro-runner --locked --test docker_sandbox
docker ps -a --filter label=dev.metro.runner=1    # expect nothing left behind
```

Desktop, from `apps/desktop`: `nsh check` and `nsh test`. Both are headless,
and the tests use an in-memory fake server. To recreate the native
screenshots against real binaries, see
[apps/desktop/README.md](../apps/desktop/README.md#screenshots-native-linux-isolated).

To check Compose by hand, follow the [README quickstart](../README.md). In a
throwaway checkout, set `COMPOSE_PROJECT_NAME` in `.env` to keep the run
isolated.
