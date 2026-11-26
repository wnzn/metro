# Installing and running a Metro server

This is the operator's guide. It uses only published files. Building from
source is for contributors: [development.md](development.md).

## Release files and images

> **Latest release: [Metro 0.6.0](https://github.com/wnzn/metro/releases/tag/v0.6.0)**,
> published 2026-11-26 from commit `430c835` of
> <https://github.com/wnzn/metro>. Releases are published automatically
> when a version bump is merged into the protected `main` branch
> ([CI and publishing](#ci-and-publishing)). Each archive and MSI comes
> with a `.sha256` file beside it. The full release file list
> is in [install.md](install.md#official-releases); what was checked is in
> [acceptance](acceptance.md#published-metro-060-2026-11-26-github-release-v060).
> The image digests are in the release's
> [`IMAGES.txt`](https://github.com/wnzn/metro/releases/download/v0.6.0/IMAGES.txt).

| Part | Where | Name in 0.6.0 |
| --- | --- | --- |
| Server image | GitHub Container Registry | `ghcr.io/wnzn/metro-server:v0.6.0` (`linux/amd64`, `linux/arm64`) |
| Runner image | GitHub Container Registry | `ghcr.io/wnzn/metro-runner:v0.6.0` (`linux/amd64`, `linux/arm64`) |
| Media embedding image (optional) | GitHub Container Registry | `ghcr.io/wnzn/metro-embeddinggemma2:v0.6.0` (`linux/amd64`, no weights) |
| Desktop, Windows x64 | [Releases](https://github.com/wnzn/metro/releases/tag/v0.6.0) | `Metro-0.6.0-windows-x64.zip` and `Metro-0.6.0-windows-x64.unsigned-preview.msi` (+ `.sha256`), **unsigned** |
| Desktop, Linux x64 | Releases | `Metro-0.6.0-linux-x64.tar.gz` (+ `.sha256`) |
| Runner, Linux x64 binary | Releases | `Metro-runner-0.6.0-linux-x64.tar.gz` (+ `.sha256`) |

`<tag>` is the release tag, `v` plus the version (for example `v0.6.0`).
The Windows files ship without an Authenticode signature until a
code-signing certificate is set up
([Windows releases](#windows-releases-signed-or-unsigned)); expect
SmartScreen warnings, and check them with the Sigstore-signed `SHA256SUMS`.
CI creates it on the released commit; it's never moved.
For production, pin images by digest (`…@sha256:…`), not by tag.

## Install the server

You need Docker on a Linux host. The server image runs as UID 10001 and
stores its SQLite database in `/data`.

### 1. Bootstrap the first admin

The first admin is a local account named in the server's environment.
Choose a username (1–64 characters from `A–Z a–z 0–9 . _ @ -`; `admin` below)
and generate the token into a private file. Nothing is printed, and an
existing file is never overwritten:

```sh
C="$HOME/.config/metro"
mkdir -p "$C" && chmod 700 "$C"
( umask 077
  [ -e "$C/admin.token" ] || openssl rand -hex 32 > "$C/admin.token"
  [ -e "$C/server.env" ] && { echo "$C/server.env exists; not overwriting" >&2; exit 0; }
  printf "METRO_DEV_USERS={\"admin\":\"%s\"}\nMETRO_ORG_ADMINS=[\"admin\"]\n" \
    "$(cat "$C/admin.token")" > "$C/server.env" )
```

`METRO_ORG_ADMINS` lists admins by stable id: for local accounts, the
username. Being an admin gives no access to anyone's Spaces. The env file
uses Docker's `--env-file` format, so its values aren't quoted.

### 2. Start it

```sh
docker volume create metro-data
docker run -d --name metro-server --restart unless-stopped \
  --cap-drop ALL --security-opt no-new-privileges:true \
  --env-file "$HOME/.config/metro/server.env" \
  -p 127.0.0.1:8787:8787 \
  -v metro-data:/data \
  ghcr.io/wnzn/metro-server:<tag>
curl -fsS http://127.0.0.1:8787/healthz
```

It publishes `127.0.0.1:8787` only. Back up the `metro-data` volume: it holds
every Space, its members, rooms, threads and personal Chats, local accounts,
token hashes, registered runners, model connections and their encrypted
keys, and the identity namespace the database is pinned to. Back up the
provider master key separately
([settings.md](settings.md#operator-settings)). Run history is kept in memory
only.

The same settings work with Docker Compose: an `image:` service with the
same environment file, port, volume and hardening. The repository's own
`compose.yaml` builds from source, so it's for contributors.

### 3. Open the admin panel

See also the [Page limits API](pages.md#live-limits-in-the-organization-admin-panel)
and [transport and proxy rules](released-0.5.0.md#http-tailscale-and-proxies).

The panel is served by the server itself at `/admin`, on the same origin as
the API, and `/` redirects there. It's a static page that holds no data, and
does nothing until an admin signs in with their token. The token stays in
that browser tab's memory and is forgotten on sign-out, reload or close.

Open it over a protected connection only:

- on the server host: `http://127.0.0.1:8787/admin`;
- from your computer, through an SSH port forward, then
  `http://127.0.0.1:8080/admin`:

  ```sh
  ssh -N -L 8080:127.0.0.1:8787 you@your-metro-host
  ```

- or through your HTTPS reverse proxy (below).

The panel requires confirmation before sign-in over ordinary plain HTTP.
Loopback and recognized Tailscale hosts don't prompt, but the panel
can't detect browser proxies. Verify that a tailnet route is direct
before entering a token. Framed pages and non-HTTP(S) origins cannot sign in.

### 4. Create people's accounts

In the panel, **Add a user** creates a local account. Metro gives it a
stable id (`mu:` plus 32 hex digits) and a token (`metro_ut_…`), which is
shown **once**. Hand it over privately. Metro keeps only a hash.

- **Rename** changes only the display name. The id never changes.
- **Reissue token** replaces the token; the old one stops working at once.
- **Revoke token** turns the account's access off at once.
- Accounts are never deleted, so an id is never reused.
- Created accounts are ordinary people. They can't be made admins, and
  they get **no Space memberships**. Nobody is added to any Space
  automatically: people create their own Spaces, and a Space's owner adds
  members by id.

The same actions are available as `GET|POST /v1/admin/users`,
`PATCH /v1/admin/users/{id}` and `POST|DELETE /v1/admin/users/{id}/token`.

**Verified (staged):** in a headless browser against a real server, the
panel added an account, rotated its token (the old token was refused at
once) and revoked it (the new token was refused). The account could read
`/v1/me` but not the admin list
([acceptance](acceptance.md#staged-admin-panel-local-accounts-runner-package-and-release-targets-2026-11-22-project-manager-local-and-dev-service)).

The bootstrap admin from `METRO_DEV_USERS` isn't managed in the panel. To
change their token, write a new one into `server.env` and recreate the
container.

### Local accounts or OIDC

A server uses exactly one sign-in mode, and the database is pinned to the
first one it sees:

- **Local accounts**, as above: `METRO_DEV_USERS` for the bootstrap admin(s),
  plus the accounts they create in the panel.
- **OIDC**: your identity provider issues sign-ins (JWT access tokens), and
  you revoke them there. There are no local accounts, the admin user routes
  answer `404`, and `METRO_ORG_ADMINS` lists OIDC `sub` values. See
  [auth.md](auth.md).

## Reaching the server from other machines

Use these protected routes with published clients. Bearer
tokens sent over ordinary plain HTTP can be read on the network.

- **Team use:** put a TLS-terminating reverse proxy in front of
  `127.0.0.1:8787` and give people its `https://` URL.
- **Just you, or before HTTPS is set up:** each person forwards a local port
  over SSH, as in step 3, and connects the desktop app to
  `http://127.0.0.1:8080`.

The desktop asks before using ordinary plain HTTP; the runner warns.
See [transport and proxy rules](released-0.5.0.md#http-tailscale-and-proxies)
for loopback and Tailscale exceptions. Your server's address is specific
to your deployment; Metro's only built-in address is loopback.

## After install

- People create their own Spaces. Owners who want tools give a Space one
  runner: a dedicated one with that Space's token, or a registered personal
  or organization runner that they attach. See [runners.md](runners.md) and
  [runner.md](runner.md).
- People choose their own models in **Settings → Model providers**. To let
  them save API keys on the server, or to set a fallback model, see
  [settings.md](settings.md#operator-settings).
- Since 0.3.2, org admins listed in
  `METRO_ORG_ADMINS` can read the server's own monitoring events with
  `GET /v1/monitoring/events`, and each Space's owner reads that Space's
  with `?space_id=SID`. Each person reads their own personal Chat events
  with `?scope=me`; org admins can't read those. Events hold only fixed
  codes, never message text, prompts or credentials. See
  [spaces.md](spaces.md#monitoring-events).

To upgrade from the single-room build, see
[spaces.md](spaces.md#upgrading-from-the-single-room-build). The server
refuses an old database that holds history, so back it up and start on a
fresh volume.

## CI and publishing

`.github/workflows/ci.yml` runs on pull requests and on pushes to `main`.
`main` is the only long-lived branch; everything else is a feature branch.
It runs:

- the Rust format, lint and test suites and the end-to-end scripts;
- the terminal client's pseudo-terminal test;
- the desktop checks and tests (the test builds below package the app);
- both container targets;
- the packaging and workflow tests;
- unsigned test builds for Linux and Windows
  ([install.md](install.md#test-builds-from-github-actions)).

There are no macOS builds for now, in test builds or releases. GitHub's
macOS runners queued for 15 minutes or more and the jobs were cancelled, so
the Mac jobs are switched off. The macOS packaging code stays in the source;
the jobs can be restored from git history (see the comment at the top of
`release.yml`).

Each pull request into `main` and each push to `main` runs only the jobs its
changes need. The `workflows` job (workflow lint, action pins) always runs
and sorts the changed files with `packaging/ci_changes.py`. A pull request
is compared with its merge base, a push with the previous tip of `main`.
Each job then runs only if a changed path needs it:

| Job | Runs when the change touches |
| --- | --- |
| `rust` | `crates`, `Cargo.lock`, `.cargo`, `rust-toolchain`, `tests`, `examples`, `apps/desktop/assets` |
| `desktop` | `apps/desktop`, except `apps/desktop/dev` and Markdown |
| `containers` | `crates`, `Cargo.lock`, `rust-toolchain`, the Dockerfile and `.dockerignore`, the Compose files, `services`, `tests/e2e`, the container runner scripts |
| `packaging` | `packaging`, `.github`, the Compose files, `apps/desktop/assets` and `apps/desktop/src/ui` |
| Test builds | `crates`, `Cargo.lock`, `.cargo`, `rust-toolchain`, the Dockerfile and `.dockerignore`, `services`, `packaging`, `.github`, `apps/desktop` (except `dev` and Markdown) |
| All of them | `ci.yml`, `.gitattributes`, the version files (`Cargo.toml`, `apps/desktop/nsh.json`), and any path not listed here |

A change made only of documentation runs none of them: Markdown and images
under `docs/`, top-level Markdown such as `README.md`, and Markdown under
`ops/` and `apps/desktop/`. Nor does a change only to `apps/desktop/dev`
(screenshot scripts), `ops/` (the runner stack there has its own workflow,
`check-runner-stack.yml`), `.gitignore` or `.env.example`.

- A skipped job counts as passed for the required checks.
- Everything runs when CI can't tell what changed: an unreadable or empty
  diff, a symlink or submodule, a new branch, a forced push, a push whose
  previous commit is missing, or a classifier that didn't answer.
- A release run always runs every job a release needs, whatever changed,
  and the release goes ahead only if each of them ran and passed.
- Pull requests into any other branch run everything.
- A new push to a pull request cancels that pull request's older run.
  Pushes are never cancelled.

The Rust jobs keep a build cache. Only pushes to `main` write it. Pull
requests and other pushes only read it. Release builds never use it, so
they always compile from scratch.

A tag push starts nothing, and there's no manual trigger. What has been
verified so far is in [acceptance.md](acceptance.md).

### Releases from `main` on a version bump

This process published Metro 0.6.0 from `430c835` in CI run
[45737693549](https://github.com/wnzn/metro/actions/runs/45737693549)
([acceptance](acceptance.md#published-metro-060-2026-11-26-github-release-v060)).

`packaging/release_policy.py` decides. A release runs only for a
**protected, non-deleted, non-forced push to `refs/heads/main`** of
`wnzn/metro` (repository id 1467654868) **that changes the version**.
`ci.yml` itself must be the top-level workflow at that branch, and
`github.sha` must equal the pushed commit. Every job that signs, creates the
tag, pushes images or publishes checks this again before it touches a
credential. Pull requests, other branches, tags and manual runs never do any
of that.

The version is the one checked in. The Cargo workspace version and
`apps/desktop/nsh.json` must match. A push releases only when that version
differs from the version at the push's previous commit
(`github.event.before`). Every other push to `main` runs CI and test builds
only. That includes a push whose previous commit is missing or can't be
read. Such a push never looks at tags or releases, so it never fails on
them.

To release:

1. In a pull request, set the new version in both the Cargo workspace
   (`Cargo.toml`, `[workspace.package] version`) and `apps/desktop/nsh.json`.
2. Merge it into `main`. That push to `main` builds, signs and publishes the
   release. While Windows ships unsigned
   ([below](#windows-releases-signed-or-unsigned)), "signs" means the
   Sigstore-signed `SHA256SUMS` and the signed images, not the Windows files.
3. If the release fails, fix the cause (for example a missing signing
   setting) and **re-run that same workflow run** in GitHub Actions. It
   resumes from the tag it already created. If the fix needs a code change,
   merge it and bump to a new version: a push without a bump never
   releases.

For a push that changes the version:

| `v<version>` tag | Published release | Result |
| --- | --- | --- |
| missing | none | **release** (new version) |
| on this commit | none | **release** (a re-run resumes after a failure) |
| on this commit | exactly the expected files | nothing to do |
| on another commit | exists | fail (changed to a version that is already out) |
| on another commit | none | fail (conflicting unpublished tag) |
| missing | exists | fail |
| on this commit | other files | fail (never overwritten) |

Every push to `main` gets its own CI run, and pushes never cancel or replace
each other. Only the publication step (`publish`) of a release run is
serialized, and a running release is never interrupted. So only a second
release run can wait, and only behind a first one that is still publishing.
GitHub keeps one waiting job in that queue: if a third release run reaches
it while the second still waits, the second's `publish` is cancelled.
Re-run that run; it still releases. The order, in `release.yml` and then
`release-publish.yml`:

1. Build and test every platform.
2. Recheck the version bump and eligibility.
3. The Windows gate. With Windows signed: Authenticode signing and
   verification (`windows-sign.yml`). With Windows unsigned (now): no
   signing step; the unsigned Windows build was already installed and
   self-tested natively in step 1, and its ZIP, MSI and scripts go into the
   release as they are.
4. Create the immutable `v<version>` tag on exactly that commit, with the
   workflow's own token.
5. The exact release set: Windows and Linux only, no macOS for now.
6. GHCR images, pushed, read back and signed.
7. The Sigstore-signed `SHA256SUMS`.
8. Publication: the files are uploaded to a staging draft, every digest
   GitHub reports is compared with the verified files, and only then does
   the release become public.

Image and file signatures are bound to `refs/heads/main` and to the exact
commit. A rerun after a failure resumes, and a rerun after success changes
nothing. A prerelease version is never marked latest.

A release needs this setup, which was in place for 0.6.0:

- **The `main` branch,** the repository's default branch, protected against
  deletion and force pushes. Changes, including version bumps, arrive
  through pull requests.
- **macOS:** nothing for now. Releases don't build or sign a Mac, and no
  workflow reads the Apple signing secrets. If they're already set, they're
  unused.
- **Windows:** nothing while releases ship Windows unsigned
  ([below](#windows-releases-signed-or-unsigned)). Before switching to
  signed: the `windows-release-signing` environment, with deployment
  allowed only from the `main` branch and never from tags (the signing job
  checks this). Reviewers are recommended. It also needs repository
  variables:
  - `METRO_WINDOWS_SIGNING_BACKEND`;
  - `METRO_WINDOWS_PUBLISHER_CN`, set exactly as the issued certificate
    shows it, plus the optional `METRO_WINDOWS_PUBLISHER_ORG`.

  There are two backends:
  - `azure`: Azure Artifact Signing with a **Public Trust** profile, over
    GitHub OIDC, with no stored secret. Microsoft's identity validation and
    eligibility depend on the publisher's country and entity type, so check
    it before choosing it (for example, from the Philippines); don't assume
    it's available.
  - `kms`: a certificate from a public CA whose key stays in an HSM or KSP,
    on a restricted self-hosted runner.

  Details are in
  [packaging/windows-signing/README.md](../packaging/windows-signing/README.md#repository-setup-for-official-releases).
- **Images, the tag and Sigstore** need no secret. They use the workflow's
  own GitHub token and OIDC identity.

Never paste a key, certificate, password or token into chat, an issue or a
log. Enter them only in GitHub's secret and variable settings.

### Windows releases: signed or unsigned

One checked-in file decides how a release ships Windows:
`packaging/windows-release-signing.txt`. It holds exactly one value line,
`signed` or `unsigned`; anything else fails the release. Nothing else
switches it: no variable, input or fallback.

**It says `unsigned` now**, because Metro has no Windows code-signing
certificate yet. Then a release:

- never calls `windows-sign.yml`, so it reads no signing variable, never
  requests the `windows-release-signing` environment and requests no OIDC
  token for Windows;
- still checks the Windows seed as strictly as a signed release would
  (`metro-tui.exe` required), and goes ahead only if the unsigned ZIP and
  MSI pass the same native checks as a test build on a fresh `windows-2025`
  runner in the same run (MSI install and removal, the MSI lane's upgrade,
  repair and downgrade tests, and the verifier self-test). Those native
  checks haven't passed on GitHub yet
  ([install.md](install.md#desktop-on-windows));
- ships `Metro-<version>-windows-x64.zip` (its `RELEASE.json` says
  `unsigned-preview` and names the commit),
  `Metro-<version>-windows-x64.unsigned-preview.msi`, their `.sha256` files
  and the two unsigned `.ps1` scripts, all listed in the Sigstore-signed
  `SHA256SUMS`;
- says in its release notes that the Windows files aren't code-signed, that
  SmartScreen and "Unknown publisher" warnings are expected, and how to
  check them with `SHA256SUMS` and Sigstore.

**To sign again,** set up a backend and the environment as described above
(a CA certificate in an HSM or KSP works through the `kms` backend), then
change the value line to `signed` in a pull request. Signed releases fail
closed exactly as before: without `METRO_WINDOWS_SIGNING_BACKEND` set to
`azure` or `kms`, or without the environment, `windows-sign.yml` stops the
release before the tag, and nothing falls back to unsigned. Bump the version
in the same or a later pull request, because only a version bump releases.

## Trust boundaries

- **Identity provider:** decides who exists. Anyone it issues a valid token to
  can sign in, use personal Chat, and create Spaces. The database is pinned to
  that provider's ids, and the server refuses to switch to another.
- **Space owner:** decides who is a member and which runner, if any, the
  Space uses. Members can't add people, and nobody can add themselves.
- **Org admins** (`METRO_ORG_ADMINS`): create, re-credential and delete
  organization runners, and, with local accounts, create accounts and
  reissue or revoke their tokens. They don't gain access to any Space by
  being admins, and a new account is never added to a Space for them.
- **Members:** everyone in a Space can read all of its rooms and threads, and
  address its agent. The agent can use the runner's tools and spawn threads
  that use them too, so every member can effectively read any file in the
  runner's workspace. Add only people you'd give that access.
- **Server:** holds the SQLite data, runner and device token hashes, the
  encrypted personal API keys, the provider master key and the fallback model
  key. It is the most sensitive component. Its operator can read every Space,
  and can decrypt stored API keys. Device connections keep their vendor
  credentials off the server, but their requests and replies still pass
  through it unencrypted.
- **Dedicated runner:** belongs to one Space. With `METRO_SANDBOX=none`,
  commands run with the runner's own privileges. That is a trust decision,
  not a sandbox. `docker` narrows it (see [runner.md](runner.md)). Mount only
  what that Space's members may see.
- **Registered runner:** serves each Space it's attached to, each in its own
  `spaces/<sid>/` root. Members of one Space can't reach another Space's
  root through it, and it never runs check or MCP commands on the host. Its
  operator can read every Space root on that machine, so a Space owner who
  attaches someone's runner is trusting that person with the Space's
  files.
- **Model provider:** room, thread and Chat history, with author and
  recipient labels, plus tool output (up to 64 KiB per tool call) go to the
  model connection of whoever started the run. That's their device connector,
  their stored API profile, or `METRO_MODEL_BASE_URL`. So a Space's
  transcript can reach any member's chosen provider. Messages between people still become history for
  later agent turns in the same room. Demo never calls a model, and nothing
  reaches the model until someone addresses an agent. A Space agent's
  owner-written instructions go with each of its runs, so they reach every
  member's chosen provider too; every member can read them. A member who
  turned on classification sends only their own new message (no history,
  ids or instructions) to their System One connection, with the agents'
  names and descriptions.
- **Secrets on screen:** user tokens, runner tokens and model keys are bearer
  secrets. Follow [spaces.md](spaces.md#keeping-secrets-out-of-screenshots-and-logs)
  before you screen-share or take screenshots.
