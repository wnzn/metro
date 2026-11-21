# Self-hosting, CI/CD and trust boundaries

## Self-hosting

1. Create `.env` with OIDC settings (not `METRO_DEV_USERS`). See
   [auth.md](auth.md) and `.env.example`. The OIDC issuer must be reachable
   over HTTPS when the server starts. The server has no runner settings.
2. Start the server with `docker compose up -d`
   ([Compose docs](https://docs.docker.com/compose/)). It publishes
   `127.0.0.1:8787` only and stores SQLite at `/data/metro.sqlite3` in the
   `server-data` volume. Back that volume up. It holds every Space, its
   members, rooms, threads and personal Chats, the hashes of the runner
   tokens, and the identity namespace the database is pinned to. Run history
   is kept in memory only.
3. To reach the server from other machines, put a TLS-terminating reverse
   proxy in front of `127.0.0.1:8787`. The desktop app refuses plain `http://`
   to non-loopback hosts, and the runner does the same unless
   `METRO_ALLOW_INSECURE_HTTP=1`. Compose sets that only for the bundled
   runner's internal `http://server:8787` hop.
4. Users create their own Spaces. Owners who want tools pair a runner: either
   the bundled one (`docker compose --profile runner up -d runner` with that
   Space's token) or a remote one (see [runner.md](runner.md)).

To upgrade from the single-room build, see
[spaces.md](spaces.md#upgrading-from-the-single-room-build). The server
refuses an old database that holds history, so back it up and start on a
fresh volume.

## CI and images

`.github/workflows/ci.yml` runs on pull requests, on pushes to `main`, and on
`v*` tags. It runs `cargo fmt --check`, `clippy -D warnings`,
`cargo test --workspace`, the example workspace check, the end-to-end smoke
test (`tests/e2e/smoke.py`), `nsh check` and `nsh build --release` for the
desktop app, and it builds both container targets.

Pushing a `v*` tag also publishes to the
[GitHub Container Registry](https://docs.github.com/en/packages/working-with-a-github-packages-registry/working-with-the-container-registry):

```text
ghcr.io/<owner>/<repo>/metro-server:<tag>
ghcr.io/<owner>/<repo>/metro-runner:<tag>
```

The path is lower case. For production, pin images by digest. Publishing
hasn't been exercised yet; the first tagged release will be its first real
run. Background:
[publishing Docker images](https://docs.github.com/en/actions/publishing-packages/publishing-docker-images).

## Trust boundaries

- **Identity provider:** decides who exists. Anyone it issues a valid token to
  can sign in, use personal Chat, and create Spaces. The database is pinned to
  that provider's ids, and the server refuses to switch to another.
- **Space owner:** decides who is a member and holds the Space's runner token.
  Members can't add people, and nobody can add themselves.
- **Members:** everyone in a Space can read all of its rooms and threads, and
  address its agent. The agent can use the runner's tools and spawn threads
  that use them too, so every member can effectively read any file in the
  runner's workspace. Add only people you'd give that access.
- **Server:** holds the SQLite data, runner token hashes and the model API
  key. It is the most sensitive component. Its operator can read every Space.
- **Runner:** belongs to one Space. With `METRO_SANDBOX=none`, commands run
  with the runner's own privileges. That is a trust decision, not a sandbox.
  `docker` narrows it (see [runner.md](runner.md)). Mount only what that
  Space's members may see.
- **Model provider:** room, thread and Chat history, with author and
  recipient labels, plus tool output (up to 64 KiB per tool call) go to
  `METRO_MODEL_BASE_URL`. Messages between people still become history for
  later agent turns in the same room. Demo never calls a model, and nothing
  reaches the model until someone addresses the agent.
- **Secrets on screen:** user tokens, runner tokens and model keys are bearer
  secrets. Follow [spaces.md](spaces.md#keeping-secrets-out-of-screenshots-and-logs)
  before you screen-share or take screenshots.
