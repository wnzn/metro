# Runners: dedicated and registered

Metro 0.5.0 includes the desktop's
[local runner offer](released-0.5.0.md#this-computers-runner),
[guarded Space Runner settings](released-0.5.0.md#guarded-runner-changes)
and [runner update controls](updates.md#runner-and-server-updates).

A runner is a `metro-runner` process on someone's machine. It gives a Space's
agent tools over files on that machine. A Space works without one. This page
explains the two kinds of runner, who controls each kind, and how to run a
registered runner in Docker. For the variables and time limits that both
kinds share, see [runner.md](runner.md).

**Local tools aren't a runner you install.** In the desktop app's own local
workspace (**Use locally**), a Space's owner can turn on read-only local tools
instead: `workspace.list` and `workspace.read` over one private folder per
Space on that computer, with no check, MCP or commands. They're offered
only when the Space has no runner, and they never replace one. See
[apps/desktop/README.md](../apps/desktop/README.md#local-tools-on-this-computer).
**The desktop app also has a runner built in** (released in 0.6.2):
**Settings → Runners → This computer's runner → Set up** makes it
your personal registered runner for the server you're signed in to, and it
runs while Metro is open. Since 0.6.3, it also writes files and runs shell
commands. See [install.md](install.md#the-desktops-own-runner). Everything else on this
page is the separately installed runner, for another machine, for check and
MCP, or for one that runs while the app is closed.

## Two kinds of runner

| | Dedicated runner | Registered runner |
| --- | --- | --- |
| Credential | The Space's runner token, issued by the Space owner | A runner credential issued when the runner is registered. It starts with `metro_rt_v2_`. |
| Serves | Exactly one Space | Each Space it's attached to, and no other |
| Workspace | `METRO_WORKSPACE` itself | One directory per Space: `METRO_WORKSPACE/spaces/<sid>/` |
| Tools | list, read, and the operator's check and MCP | list and read. Check and MCP only in a per-Space Docker sandbox ([below](#check-and-mcp-on-a-registered-runner)). |
| Write and shell (since 0.6.3) | Yes; shell policy set by the Space owner | Yes; shell policy set by the runner's manager, with per-Space overrides |
| Managed by | The Space owner | The runner's owner (personal) or the org admins (organization) |

The runner works out which kind it is from the token's prefix alone, never
from anything the server sends.

The two credentials are separate. A dedicated token never becomes a registry
credential, and a registry credential never stands in for a Space's dedicated
token. Neither kind falls back to the other, and the server never sends one
Space's jobs to a runner that isn't attached to that Space.

### Dedicated runners

These are Space-owned tokens. The owner issues the token (it's shown once
and stored only as a hash), and can rotate or revoke the reviewed
assignment from Space settings. See
[spaces.md](spaces.md#runner-pairing). A dedicated runner stays private to its
Space: it can't be shared or attached anywhere else.

### Registered runners

A registered runner has a name and a **scope**:

- **Personal:** belongs to one user. Only that user can see it, manage it, or
  attach it, and only to Spaces they own.
- **Organization:** belongs to the server's one organization. Only org admins
  create and manage it. Any Space owner can attach it to their own Space.

Org admins are listed in the server's `METRO_ORG_ADMINS`, a JSON array of
stable user ids. With dev users, that's the username. With OIDC, it's the
`sub`. See [settings.md](settings.md#organization-admins).

Sharing works by attachment. One registered runner can be attached to many
Spaces, but **each Space uses at most one runner**. Attaching is always an
explicit choice by the Space owner, and it gives that Space's members the
runner's tools over that Space's own directory, and nothing else.

| Action | Personal runner | Organization runner |
| --- | --- | --- |
| See it in `GET /v1/runners` | its owner | everyone signed in |
| Register it, issue a new credential, delete it | its owner | org admins |
| Attach it to a Space, or detach it | its owner, for Spaces they own | the owner of that Space |
| See which runner a Space uses (name and scope only) | that Space's members | that Space's members |

Someone else's personal runner doesn't exist as far as you can tell: it gets
**404**. Trying to manage an organization runner without being an org admin
gets **403**. Members never see or manage any runner credential.

Limits: 20 personal runners per user, and 100 organization runners per
server.

### API

Use the [guarded Space assignment routes](released-0.5.0.md#guarded-runner-changes)
for new clients. The unguarded Space writes below are legacy routes.

| Request | Who | Result |
| --- | --- | --- |
| `GET /v1/runners` | anyone | `{"runners": [{"id", "name", "visibility", "can_manage"}]}`: your personal runners and every organization runner |
| `POST /v1/runners` `{"name", "visibility"}` | anyone for `"personal"`, org admins for `"organization"` | `201` with `{"runner", "token"}`, sent with `Cache-Control: no-store`. The token is shown only this once. |
| `POST /v1/runners/{id}/token` `{}` | whoever may manage it | `{"token"}`: a new credential. The old one stops working at once. |
| `DELETE /v1/runners/{id}` | whoever may manage it | `204`. Revokes the credential and detaches the runner from every Space. |
| `GET /v1/spaces/{sid}/runner` | members | `{"runner": null}`, or `{"runner": {"id", "name", "visibility", "kind"}}` where `kind` is `dedicated` or `registered` |
| `PUT /v1/spaces/{sid}/runner` `{"runner_id"}` | Space owner | Attaches the runner, replacing any other registered runner. `409` while the Space has a dedicated token. |
| `DELETE /v1/spaces/{sid}/runner` `{}` | Space owner | `204`. Detaches the registered runner, or revokes the dedicated token. |

`GET /v1/me` also returns `is_org_admin`. Removing or replacing an
assignment drops its queued and claimed work. Rotation invalidates
claimed work; unclaimed work can remain queued for the same runner.

## Workspace layout

A registered runner treats `METRO_WORKSPACE` as a base directory, not as a
tool root. Each Space gets its own root under it:

```text
/workspace/                  METRO_WORKSPACE, writable by the runner
└── spaces/
    ├── <sid-of-space-A>/    Space A's root
    └── <sid-of-space-B>/    Space B's root
```

- The runner creates `spaces/<sid>/` the first time a job arrives for that
  Space. A claim without a valid Space id is refused.
- A list, read or write job for Space A can't reach
  `spaces/<sid-of-space-B>/` or anything else outside Space A's root. File
  access uses `openat2(RESOLVE_BENEATH | RESOLVE_NO_MAGICLINKS)`, the same
  as dedicated runners. A `workspace.shell` command starts in Space A's
  root, but without the Docker sandbox it can reach whatever the
  runner's user can, including Space B's directory
  ([below](#shell-on-a-registered-runner)).
- The runner refuses `/` as the base. It creates `spaces/` (mode 711) at
  startup, and each Space directory with mode 755.
- `spaces/` and `spaces/<sid>/` must be real directories. A symlink in either
  place is refused, so you can't alias one Space's directory to another or
  point it somewhere else on the host. Inside a Space's root, symlinks that
  leave the root are refused too.
- Put each Space's checkout **in that Space's directory**. For example, the
  repo for Space `s-0123456789abcdef` goes into
  `/workspace/spaces/s-0123456789abcdef/`. The Space's id
  is shown in the desktop app, and `GET /v1/spaces` returns it as `id`.

### Check and MCP on a registered runner

A check or MCP process that runs unsandboxed has the runner's own file access.
On a registered runner that includes every sibling Space's directory, so a
registered runner **never** runs check or MCP commands unsandboxed. By default
it offers only `workspace.list`, `workspace.read`, `workspace.write` and
`workspace.shell`.

With `METRO_SANDBOX=docker`, check and MCP commands run in a locked-down
container that mounts only the claiming Space's directory, read-only, at
`/workspace`. That needs a runner that can reach a Docker daemon, so it's the
Linux runner binary on the host, not the runner image. See
[runner.md](runner.md#docker-sandbox-opt-in-fails-closed).

Dedicated runners keep their existing behaviour: with `METRO_SANDBOX=none`,
check and MCP run with the runner's privileges over its one workspace.

### Shell on a registered runner

`workspace.shell` is not limited like check and MCP. It runs on a
registered runner with or without the sandbox, and the
[shell policy](runner.md#shell-commands) decides
whether each command runs. Without `METRO_SANDBOX=docker` a command runs
as the runner's user and can read and change every Space's directory under
`METRO_WORKSPACE`, and anything else that user can reach. Before you choose
**Always allow** or **Any Space member** for a runner that serves several
Spaces, consider that a member of one Space could then reach the others.
With the sandbox, the command sees only its own Space's directory.

### Shell policy API

| Request | Who | Result |
| --- | --- | --- |
| `GET /v1/runners/{id}/shell-policy` | the runner's manager | `{"runner_id", "runner_name", "visibility", "built_in", "default": {"shell_mode", "approver"}, "spaces": [...]}`; each Space has its override (`null` inherits), `effective_mode`, `effective_approver` and `downgraded_reason` |
| `PUT /v1/runners/{id}/shell-policy` `{"shell_mode", "approver"}` | the runner's manager | The runner's default. `400` for `classify` on an organization runner. |
| `PUT /v1/runners/{id}/spaces/{sid}/shell-policy` `{"shell_mode", "approver"}` (each may be `null`) | the runner's manager | One Space's override. `404` when the runner isn't attached there. |
| `PUT /v1/spaces/{sid}/shell-policy` `{"shell_mode", "approver"}` | Space owner | A dedicated runner's policy. `409` without a dedicated token. |
| `GET /v1/spaces/{sid}/runner` | members | Adds `"shell"`: `shell_mode`, `approver`, `effective_mode`, `downgraded_reason`, `runner_name` and `source` (`dedicated`, `runner_default` or `space_override`) |
| `GET /v1/spaces/{sid}/shell-approvals` | members | Pending approvals, each with `can_decide` |
| `POST /v1/spaces/{sid}/shell-approvals/{approval_id}` `{"decision": "approve" \| "deny"}` | eligible approvers | `200` when this decision won; `409` once decided, expired or cancelled; `403` for a member who can't approve; `404` otherwise |

`shell_mode` is `off`, `approve`, `classify` or `allow`; `approver` is
`owner` or `members`. `downgraded_reason` is `null`,
`built_in_multiple_spaces` or `classify_unavailable`. `POST /v1/runners`
and `POST /v1/runners/{id}/token` accept `"built_in": true` for a personal
runner, and `GET /v1/runners` items include `built_in`; the desktop sets it
for its own runner.

### Packaging is not sandboxing

Running `metro-runner` inside a Docker container is a way to **package** it.
It keeps the runner away from the rest of your machine, and it's what this page
sets up below. It isn't the same as the **sandbox**, which is the opt-in
`METRO_SANDBOX=docker` mode, where the runner starts a separate throwaway
container for each check or MCP command. The bundled runner image has no
Docker access, so it always runs with `METRO_SANDBOX=none`, and as a
registered runner it offers no check or MCP.
Its shell commands run inside the runner container, which can reach every
Space's directory in the workspace volume.

## Run a registered runner in Docker

These steps use the published runner image and need no Metro source. The
workspaces live in a named Docker volume.

> **Published:** `ghcr.io/wnzn/metro-runner:v0.6.4` is in
> [Metro 0.6.4](https://github.com/wnzn/metro/releases/tag/v0.6.4). Pin it
> by the digest in the release's `IMAGES.txt`
> ([deploy.md](deploy.md#release-files-and-images)). Contributors can build
> the same image themselves ([development.md](development.md#images-and-binaries)).

You need Docker, `bash`, and Linux ≥ 5.6 (the kernel inside Docker Desktop's
VM also counts), because the runner confines file access with `openat2`. It
checks this at startup and refuses to run on an older kernel.

### 1. Register the runner and save its credential

In the desktop app, open **Settings → Runners**, give the runner a name,
choose **Who can attach it** (personal, or organization if you're an org
admin), and click **Add runner**. Or call `POST /v1/runners`. The credential
is shown **once**.

Save it without putting it on the screen or in your shell history. In
`bash`, run this and paste the credential at the prompt (nothing is echoed):

```sh
D="${XDG_CONFIG_HOME:-$HOME/.config}/metro-runner"
mkdir -p "$D" && chmod 700 "$D"
( umask 077
  IFS= read -rs -p 'Runner credential: ' t; echo
  printf 'METRO_RUNNER_TOKEN=%s\n' "$t" > "$D/runner.env" )
```

`runner.env` is mode 600 and lives outside any project directory. Docker reads it with
`--env-file`, so the credential never appears in a command line or in your
shell history. It does become part of the container's environment: anyone
who can run `docker inspect` on that container (any Docker administrator on
the machine) can read it. Treat Docker access as access to the credential.
The `metro-runner-container` script (at `fb7cb06`; in the local setup cut,
but not run in any recorded test; see [install.md](install.md#in-a-container-metro-runner-containersh--ps1))
avoids this: it mounts the token file read-only and keeps the credential
out of the container's configuration and environment settings. Prefer it
once it's in a download you've verified.

### 2. Pull the runner image

```sh
docker pull ghcr.io/wnzn/metro-runner:<tag>
```

The image runs as UID/GID 1000. A new named volume, as below, is writable
by that user automatically. A host directory you mount instead must already
exist and be readable and writable by UID/GID 1000 (or by the group you
grant), because the runner creates `spaces/` and each `spaces/<sid>/`
inside it.

### 3. Choose a server URL the container can reach

Inside a container, `127.0.0.1` is the container itself, not your machine. Set
`METRO_SERVER_URL` explicitly:

- **Remote or proxied server:** use its `https://` URL, for example
  `https://metro.example.com`. This is the normal choice. See
  [deploy.md](deploy.md) for the TLS reverse proxy.
- **Server in Docker on the same machine:** join its network
  and use plain HTTP to the service name only on a trusted private
  network. Find the network with
  `docker network ls`. It's `<project>_default`, for example `metro_default`.

Metro 0.5.0 warns on ordinary plain HTTP and does not require an
environment opt-in. Use HTTPS for untrusted networks.

### 4. Start the runner

With an HTTPS server:

```sh
docker volume create metro-workspaces
docker run -d --name metro-registered-runner --init --restart unless-stopped \
  --cap-drop ALL --security-opt no-new-privileges:true \
  --env-file "$D/runner.env" \
  -e METRO_SERVER_URL='https://metro.example.com' \
  -e METRO_WORKSPACE=/workspace \
  -v metro-workspaces:/workspace \
  ghcr.io/wnzn/metro-runner:<tag>
```

With a Compose server on the same machine, replace the
`METRO_SERVER_URL` line with these two:

```sh
  --network metro_default \
  -e METRO_SERVER_URL='http://server:8787' \
```

A new named volume takes the ownership of the image's `/workspace`, so it's
writable by the runner's user. Check the logs with
`docker logs metro-registered-runner`. They never contain the credential.

The repository's `registered-runner` Compose service builds the image from
source, so it's contributor tooling
([development.md](development.md#bundled-dedicated-runner-built-locally)).

### 5. Attach it to a Space

[Settings → Runners](screenshots/settings-runners.png) lists the runners you
can see. A Space's [Manage view](screenshots/settings-space-runner.png) shows
which runner that Space uses, and its folder on the runner.

As the Space's owner, open the Space's **Manage** view and click **Attach**
next to the registered runner, or call `PUT /v1/spaces/{sid}/runner` with
`{"runner_id"}`. Attaching replaces any other registered runner. A Space with a dedicated token must revoke it
first, because a Space uses at most one runner.

### 6. Put each Space's checkout in its directory

Clone into `/workspace/spaces/<sid>/` as the runner's user, so that the runner
can read the files. One way is a throwaway `git` container on the same volume:

```sh
SID=s-0123456789abcdef   # the Space's id
docker run --rm --user 1000:1000 -e HOME=/tmp \
  -v metro-workspaces:/workspace \
  alpine/git:2.47.2 clone https://example.com/your/repo.git "/workspace/spaces/$SID"
```

The target must be empty or missing. If the runner has already created the
Space's directory, it's empty and the clone still works. For a private repo,
don't hand your Git credentials to the runner. Clone on the host into a
directory you then mount with `METRO_REGISTERED_WORKSPACES`, or use a
read-only deploy key that only this helper container sees.

To look at what's there:

```sh
docker run --rm --entrypoint ls -v metro-workspaces:/workspace:ro \
  ghcr.io/wnzn/metro-runner:<tag> -la /workspace/spaces
```

### Stop, rotate, remove

- `docker rm -f metro-registered-runner` stops the runner. The volume and its
  checkouts stay.
- **Rotate token…** in **Settings → Runners** (`POST /v1/runners/{id}/token`)
  issues a new credential and invalidates the old one. Write the new one to `runner.env` with step 1, then recreate the
  container.
- Deleting the runner revokes its credential and detaches it from every
  Space. Detaching it from one Space affects only that Space. Either way,
  that Space's queued and claimed jobs fail. A command the runner has already
  started still runs until it ends or times out
  ([time limits](runner.md#time-limits)).
- `docker volume rm metro-workspaces` deletes every Space's checkout on this
  runner.

## Trust

- Every member of an attached Space can make the agent read anything in that
  Space's directory. Put only what all of them may see there.
- The runner's operator can read every Space directory on the runner. Attach a
  personal runner only to Spaces whose files you're willing to hold, and use
  an organization runner only if you trust whoever runs it.
- Detaching a runner doesn't delete that Space's directory on the runner.
  Remove it yourself if you need to.
