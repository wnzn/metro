# Installing and running a Metro server

This is the operator's guide. It uses only published files. Building from
source is for contributors: [development.md](development.md).

## Release files and images

> **Not published yet.** The repository is
> <https://github.com/wnzn/metro>. Releases will appear on its
> [Releases page](https://github.com/wnzn/metro/releases), published
> automatically from the protected `main` branch
> ([CI and publishing](#ci-and-publishing)). No public release, file or image
> exists yet. The 0.2.0 names below were built and verified locally
> ([acceptance](acceptance.md#final-metro-020-local-release-prepublication-2026-11-23-project-manager-local)).
> Each release file comes with a `.sha256` file beside it. The full release
> file list, including the MSI and macOS builds, is in
> [install.md](install.md#official-releases).

| Part | Where | Release-target name |
| --- | --- | --- |
| Server image | GitHub Container Registry | `ghcr.io/wnzn/metro-server:<tag>` |
| Runner image | GitHub Container Registry | `ghcr.io/wnzn/metro-runner:<tag>` |
| Desktop, Windows x64 | [Releases](https://github.com/wnzn/metro/releases) | `Metro-0.2.0-windows-x64.zip` (+ `.sha256`) |
| Desktop, Linux x64 | Releases | `Metro-0.2.0-linux-x64.tar.gz` (+ `.sha256`) |
| Runner, Linux x64 binary | Releases | `Metro-runner-0.2.0-linux-x64.tar.gz` (+ `.sha256`) |

`<tag>` is the release tag, `v` plus the version (for example `v0.2.0`).
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

Opened over plain `http://` from a LAN or other non-loopback address, inside
a frame, or from any non-HTTP origin, the panel is a **landing page only**:
sign-in is turned off before any network call, and the page shows the SSH
command to use instead. Never paste an admin token into it.

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

Bearer tokens must never cross a network in clear text.

- **Team use:** put a TLS-terminating reverse proxy in front of
  `127.0.0.1:8787` and give people its `https://` URL.
- **Just you, or before HTTPS is set up:** each person forwards a local port
  over SSH, as in step 3, and connects the desktop app to
  `http://127.0.0.1:8080`.

The desktop app refuses plain `http://` to non-loopback hosts. The runner
does the same unless `METRO_ALLOW_INSECURE_HTTP=1`, which is only for a
container-network hop that never leaves the host. Your server's address is
specific to your deployment; Metro's only built-in address is loopback.

## After install

- People create their own Spaces. Owners who want tools give a Space one
  runner: a dedicated one with that Space's token, or a registered personal
  or organization runner that they attach. See [runners.md](runners.md) and
  [runner.md](runner.md).
- People choose their own models in **Settings → Model providers**. To let
  them save API keys on the server, or to set a fallback model, see
  [settings.md](settings.md#operator-settings).
- In the current source after 0.2.0 (not released), org admins listed in
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
It runs:

- the Rust format, lint and test suites and the end-to-end scripts;
- the terminal client's pseudo-terminal test;
- the desktop checks and release build;
- both container targets;
- the packaging and workflow tests;
- unsigned test builds for Linux, Windows and both Macs
  ([install.md](install.md#test-builds-from-github-actions)).

A tag push starts nothing, and there's no manual trigger. What has been
verified so far is in [acceptance.md](acceptance.md).

### Releases from `main` (in the source; never run with real credentials)

`packaging/release_policy.py` decides. A release runs only for a
**protected, non-deleted, non-forced push to `refs/heads/main`** of
`wnzn/metro` (repository id 1467654868). `ci.yml` itself must be the
top-level workflow at that branch, and `github.sha` must equal the pushed
commit. Every job that signs, creates the tag, pushes images or publishes
checks this again before it touches a credential. Pull requests (also into
`main`), other branches, tags and manual runs never do any of that.

The version is the one checked in. The Cargo workspace version and
`apps/desktop/nsh.json` must match. For each eligible push:

| `v<version>` tag | Published release | Result |
| --- | --- | --- |
| missing | none | **release** (new version) |
| on this commit | none | **release** (resume after a failed run) |
| on this commit | exactly the expected files | nothing to do |
| on another commit | exists | no release (this version is already out; the push only runs CI) |
| on another commit | exists, and this push changed to that version | fail |
| on another commit | none | fail (conflicting unpublished tag) |
| missing | exists | fail |
| on this commit | other files | fail (never overwritten) |

The tag is looked up on GitHub, not in a local clone. A local tag that was
never pushed can't be seen by CI, but it must not be pushed either: on a
different commit than the release push it makes that run fail as a
conflicting unpublished tag. CI creates `v<version>` itself.

Official runs are serialized. The order, in `release.yml` and then
`release-publish.yml`:

1. Build and test every platform.
2. Recheck eligibility.
3. Windows signing and verification.
4. Create the immutable `v<version>` tag on exactly that commit, with the
   workflow's own token.
5. macOS Developer ID signing and notarization.
6. The exact release set.
7. GHCR images, pushed, read back and signed.
8. The Sigstore-signed `SHA256SUMS`.
9. Publication: the files are uploaded to a staging draft, every digest
   GitHub reports is compared with the verified files, and only then does
   the release become public.

Image and file signatures are bound to `refs/heads/main` and to the exact
commit. A rerun after a failure resumes, and a rerun after success changes
nothing. A prerelease version is never marked latest.

Before the first release, a maintainer sets up:

- **The `main` branch,** the repository's only trunk and its release branch,
  protected with a ruleset: required status checks, no force push, no
  deletion. It is already protected this way. The pipeline was retargeted to
  `main` rather than renaming a branch; `wnzn/metro` never had a `master`.
- **macOS:** the five repository secrets listed in
  [packaging/README.md](../packaging/README.md#release-signing-protected-main-only):
  - the Developer ID `.p12` and its password;
  - the App Store Connect notary key ID, issuer ID and `.p8`.

  There's also an optional `MACOS_SIGNING_TEAM_ID` variable. A release push
  without them fails.
- **Windows:** the `windows-release-signing` environment, with deployment
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

Set all of this up before pushing. The `windows-release-signing` check fails
the signing job, which runs before the tag; macOS signing runs **after**
`v<version>` is pinned on that commit, so a missing secret there leaves the
tag where it is. A rerun once the setup is in place resumes from it.

Never paste a key, certificate, password or token into chat, an issue or a
log. Enter them only in GitHub's secret and variable settings.

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
  reaches the model until someone addresses the agent.
- **Secrets on screen:** user tokens, runner tokens and model keys are bearer
  secrets. Follow [spaces.md](spaces.md#keeping-secrets-out-of-screenshots-and-logs)
  before you screen-share or take screenshots.
