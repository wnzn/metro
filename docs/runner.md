# Runner

`metro-runner` polls the server over outbound HTTP (`/v1/runner/claim`),
authenticates with `METRO_RUNNER_TOKEN`, runs each claimed job against one
workspace directory, and posts the result back. It needs no inbound port.

The token decides what the runner serves, and the server has no runner
settings. There are two kinds of token. See [runners.md](runners.md) for who
manages each kind, and for a Docker setup with the published image.

- **Dedicated:** the Space's own runner token, issued by its owner (see
  [spaces.md](spaces.md#runner-pairing)). The runner serves every room and
  thread in that one Space, and its tools work on `METRO_WORKSPACE` itself.
- **Registered** (starts with `metro_rt_v2_`): a named personal or
  organization runner. It serves each Space it's attached to, and nothing
  else. `METRO_WORKSPACE` is a writable base, and each Space gets its own
  root at `METRO_WORKSPACE/spaces/<sid>/`. It speaks runner protocol 2, and the
  server refuses a registered credential from an older runner with `426`.

**Sandboxing is opt-in.** By default (`METRO_SANDBOX=none`), a dedicated
runner runs the check and MCP commands with its own privileges on its host.
Set `METRO_SANDBOX=docker` to run them in a locked-down container instead; see
[Docker sandbox](#docker-sandbox-opt-in-fails-closed). A registered runner
never runs them on the host, because they could read other Spaces' roots.
With `METRO_SANDBOX=none` it offers list and read only, and `workspace.check`
and the MCP tools return an error. With `docker`, they run in a container
that mounts only the claiming Space's root.

Running the runner itself in a container (the `runner` image) is packaging,
not this sandbox. That image has no Docker access, so it always uses
`METRO_SANDBOX=none`.

**Requirements:** Linux ≥ 5.6. File access uses
`openat2(RESOLVE_BENEATH | RESOLVE_NO_MAGICLINKS)`. On older kernels the
runner fails at startup, and it never falls back to weaker path checks.
Containers use the host kernel, so this applies to them too.

## Tools

| Tool | Behaviour | Limit |
| --- | --- | --- |
| `workspace.list` | Lists a directory beneath the workspace | 2000 entries |
| `workspace.read` | Reads a UTF-8 text file beneath the workspace | 1 MiB (truncated beyond) |
| `workspace.check` | Runs `METRO_CHECK_ARGV`; reports an error if unset | [Time limits](#time-limits), output capped |
| MCP tools | From the stdio server in `METRO_MCP_ARGV`, if set | [Time limits](#time-limits) |

## Environment

The runner validates every setting at startup and refuses to start on any
invalid or contradictory value. Empty values count as unset.

| Variable | Default | Notes |
| --- | --- | --- |
| `METRO_RUNNER_TOKEN` | **required** | A dedicated Space token, or a registered runner credential (`metro_rt_v2_…`) |
| `METRO_WORKSPACE` | **required** | An existing directory, canonicalized at startup. Registered runners need it writable, and refuse `/`. |
| `METRO_SERVER_URL` | `http://127.0.0.1:8787` | Set it explicitly for any runner that isn't on the server's machine. Plain `http://` is allowed only for loopback hosts. |
| `METRO_ALLOW_INSECURE_HTTP` | `0` | Accepted for compatibility; not needed in 0.5.0. The runner warns on plain HTTP outside loopback and recognized Tailscale hosts. Prefer HTTPS or an SSH forward. |
| `METRO_POLL_INTERVAL_MS` | `1000` | 50 – 600000 |
| `METRO_HTTP_TIMEOUT_SECS` | `30` | See [Time limits](#time-limits) |
| `METRO_CHECK_ARGV` | unset (no check) | JSON argv |
| `METRO_CHECK_TIMEOUT_SECS` | `90` | See [Time limits](#time-limits) |
| `METRO_MCP_ARGV` | unset (no MCP) | JSON argv |
| `METRO_MCP_TIMEOUT_SECS` | `90` | See [Time limits](#time-limits) |
| `METRO_SANDBOX` | `none` | `none` or `docker` |
| `METRO_SANDBOX_IMAGE` | — | Required for `docker`; must be pinned by digest |
| `METRO_SANDBOX_MEMORY` | `1g` | Memory and swap limit |
| `METRO_SANDBOX_CPUS` | `1` | |
| `METRO_SANDBOX_PIDS` | `256` | 8 – 65536 |
| `METRO_SANDBOX_USER` | `65534:65534` | Numeric, non-root `uid:gid` |
| `METRO_SANDBOX_TMPFS_SIZE` | `256m` | Size of `/tmp` in the sandbox |
| `METRO_DOCKER_BIN` | `docker` | Docker CLI to call |

Setting any `METRO_SANDBOX_*` value while `METRO_SANDBOX` isn't `docker` is a
startup error. The runner refuses rather than run on the host by surprise.

## Operator-defined argv

`METRO_CHECK_ARGV` and `METRO_MCP_ARGV` are **JSON arrays of strings**. The
first element is the executable. The runner executes them directly (`execve`),
never through a shell, with a cleared environment that keeps only a small
allowlist (`PATH`, `HOME`, `LANG`, …). Secrets such as the runner token are not
passed. The agent can trigger these commands but can't change them.

```sh
export METRO_CHECK_ARGV='["./check.sh"]'
export METRO_MCP_ARGV='["/usr/local/bin/my-mcp-server","--stdio"]'
```

## Job semantics

The server keeps the job queue in memory and never re-queues a job once it's
claimed. A job that isn't claimed within 30 s fails, and so does one that isn't
finished within the 120 s lease (see [Time limits](#time-limits)). The runner runs each job it claims once, and retries
only the result upload, with backoff. If the runner or the server dies while a
job is running, that job fails, but any side effects of the check or MCP
command may already have happened. Treat delivery as at most once, with no
guarantee of completion, and choose commands that are safe to repeat if a
person runs the action again. Stopping a run (`POST /v1/runs/{id}/stop`)
halts the agent, but a job the runner has already claimed runs until it ends
or hits its time limit.

## Time limits

The server expires a claimed job **120 seconds** after a runner claims it
(server `run_timeout`; not configurable through the environment). A result that
arrives later is wasted, and the runner stays busy with the old job in the
meantime. The runner's own deadlines are therefore set to fit inside that
lease:

| Variable | Default | Meaning |
| --- | --- | --- |
| `METRO_CHECK_TIMEOUT_SECS` | `90` | Wall-clock limit for one `workspace.check` run. When it's reached, the process group (or container) is killed. |
| `METRO_MCP_TIMEOUT_SECS` | `90` | Wall-clock limit for one MCP job: starting the server, handshake, and the list or call. |
| `METRO_HTTP_TIMEOUT_SECS` | `30` | Limit for each HTTP request to the server, including the result upload |

All three settings are optional. At startup the runner refuses (exit code 2)
any configuration where `METRO_CHECK_TIMEOUT_SECS` or `METRO_MCP_TIMEOUT_SECS`
plus `METRO_HTTP_TIMEOUT_SECS` is more than 120 seconds. Lower values are always
accepted. To give a check more time, lower the HTTP timeout to match. For
example, `METRO_CHECK_TIMEOUT_SECS=110` with `METRO_HTTP_TIMEOUT_SECS=10` is
allowed.

These limits don't cover everything:

- Removing a timed-out sandbox container (`docker rm -f`) can take up to 30
  more seconds.
- If the first result upload fails, retries continue for up to 5 minutes and
  may arrive after the lease has expired.

## Separate remote runner

Run the runner on the machine that holds the workspace, as the published
runner image or the Linux runner binary. Either needs Linux ≥ 5.6
(`openat2`). The remote runner must reach the server over `https://`, so
expose the server through a TLS reverse proxy (see [deploy.md](deploy.md)).

> **Published in [Metro 0.6.1](https://github.com/wnzn/metro/releases/tag/v0.6.1):**
> the image `ghcr.io/wnzn/metro-runner:v0.6.1` (`linux/amd64`,
> `linux/arm64`; pin the digest from the release's `IMAGES.txt`) and the
> binary archive `Metro-runner-0.6.1-linux-x64.tar.gz` with its `.sha256`
> ([deploy.md](deploy.md#release-files-and-images)). Building them yourself
> is contributor tooling ([development.md](development.md#images-and-binaries)).

For a **registered** runner, which can serve several Spaces, follow
[runners.md](runners.md#run-a-registered-runner-in-docker). The rest of this
section is about a **dedicated** runner.

Get the Space's runner token from its owner. They issue it with
`POST /v1/spaces/{sid}/runner-token`, or under **Manage → Runner → Issue
token** in the desktop app.
Store it in a mode-600 file and send it over a private channel. Issuing a new
token rotates it, and any runner still using the old one stops getting
jobs.

### Option A: runner container, no sandbox

Put the token in a mode-600 env file, then fill in the quoted values. The
token stays out of the command line, but it's in the container's
environment, so anyone who can `docker inspect` the container can read it
([details](runners.md#1-register-the-runner-and-save-its-credential)):

```sh
TOKEN_FILE="$HOME/.config/metro-dev/space.runner-token"
( umask 077; printf 'METRO_RUNNER_TOKEN=%s\n' "$(cat "$TOKEN_FILE")" > "$TOKEN_FILE.env" )
WORKSPACE_DIR="$HOME/src/my-project"

docker run -d --name metro-runner --init --restart unless-stopped \
  --cap-drop ALL --security-opt no-new-privileges:true \
  --env-file "$TOKEN_FILE.env" \
  -e METRO_SERVER_URL='https://metro.example.com' \
  -e METRO_WORKSPACE=/workspace \
  -e METRO_CHECK_ARGV='["/bin/ls","-la"]' \
  -v "$WORKSPACE_DIR:/workspace:ro" \
  ghcr.io/wnzn/metro-runner:<tag>
```

The image's user is UID/GID 1000, so the workspace must be readable by that
user, or by a group it belongs to. Checks run inside
this container (`METRO_SANDBOX=none`). Remove `:ro` only if your check needs
to write to the workspace.

### Option B: host binary with the Docker sandbox

The Docker sandbox needs a runner that can call a Docker daemon, so run the
Linux runner from the release
(`Metro-runner-<version>-linux-x64.tar.gz`) on the host. This is also the
only way a registered runner can offer check and MCP tools. Check the
archive against its `.sha256` file (`sha256sum -c`), extract it, and start
its `run-runner`, which reads the token from the mode-600 file named by
`METRO_RUNNER_TOKEN_FILE`, never from the command line. Set `METRO_SERVER_URL` and `METRO_WORKSPACE` as above, plus
`METRO_SANDBOX=docker` and the sandbox settings below. The user that runs it
needs read access to the workspace (write access for a registered runner's
base directory) and access to the Docker daemon.

## Docker sandbox (opt-in, fails closed)

With `METRO_SANDBOX=docker`, check and MCP commands run as follows:

```text
docker run --rm --label dev.metro.runner=1 --pull=never --network=none
  --read-only --cap-drop=ALL --security-opt=no-new-privileges --ipc=none --init
  --pids-limit/--memory/--memory-swap/--cpus/--user from the settings above
  --tmpfs /tmp --mount <workspace> at /workspace, readonly --workdir /workspace
  --entrypoint <argv[0]> <image> <argv[1..]>
```

- **Digest only, never pulled.** `METRO_SANDBOX_IMAGE` must be
  `repo@sha256:<64 hex>` or `sha256:<64 hex>`. The runner rejects tags at
  startup and uses `--pull=never`, so load the image before you start it.
- **No host fallback.** If Docker or the image is unavailable, the command
  fails. It never runs on the host.
- **Read-only workspace, no network.** Only `/tmp` (tmpfs) is writable.
- **One Space per container.** On a registered runner, the mounted workspace
  is the claiming Space's `spaces/<sid>/` root, never the base or a sibling.
- The runner refuses to mount `/`, or a workspace that contains the Docker
  socket.

```sh
docker pull busybox:1.37
METRO_SANDBOX_IMAGE="$(docker image inspect --format '{{index .RepoDigests 0}}' busybox:1.37)"
echo "$METRO_SANDBOX_IMAGE"   # busybox@sha256:...

export METRO_SANDBOX=docker
export METRO_SANDBOX_IMAGE
export METRO_SERVER_URL='https://metro.example.com'
export METRO_RUNNER_TOKEN="$(cat "$HOME/.config/metro-dev/space.runner-token")"
export METRO_WORKSPACE="$HOME/src/my-project"
export METRO_CHECK_ARGV='["/bin/ls","-la"]'
./target/release/metro-runner
```

Choose a check that works on a read-only workspace. `/bin/ls` does. Build
tools like `cargo check` or `npm test` usually write caches, a `target/`
directory or temp files outside `/tmp`, so they fail in this sandbox unless
the image and argv redirect those writes to `/tmp`.

### Cleaning up orphaned containers

Sandbox containers are labelled `dev.metro.runner=1` and named
`metro-runner-<pid>-<n>-<random>`. When a job times out, or the runner stops on
SIGINT/SIGTERM, the runner force-removes the container. If the runner itself is
killed (SIGKILL, OOM kill, or a host crash), the `docker` client process dies
but **the container can keep running** until its command exits. The runner
doesn't clean these up at startup. Find and remove leftover containers with:

```sh
docker ps -a --filter label=dev.metro.runner=1
docker rm -f $(docker ps -aq --filter label=dev.metro.runner=1)
```

Only run the removal while no runner on that Docker host is running jobs.

Docker reduces risk but doesn't fully isolate a command; see
[Docker security](https://docs.docker.com/engine/security/). Access to the
Docker daemon is equivalent to root on the runner host.
