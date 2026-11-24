# Self-hosted GitHub Actions runner (`metro-ci-linux-x64`)

One Linux x64 runner for the public repository `wnzn/metro`, run with Docker
Compose. It's for Metro's own CI only. It has nothing to do with Metro's
personal or Space runners (`metro-runner`), and it never signs anything:
the Windows signer and every release job stay on their own workflows.
GitHub-hosted Windows jobs aren't affected. (There are no macOS jobs for now.)

> **Status: registered; the manual smoke run passed.**
>
> - **Source:** the runner at `fdbbec1`, `bd8e6ae` and `023b442`; the egress
>   proxy at `5d06302`; the manual workflow at `b9934f1`.
> - **Registered and used once:** a manual `self-hosted-ci.yml` smoke run
>   succeeded on it
>   ([run 45202782869](https://github.com/wnzn/metro/actions/runs/45202782869),
>   runner id 2, which is offline now).
> - **Not yet run:** the automatic `ci.yml` routing below.
> - **More evidence:** [acceptance.md](../../docs/acceptance.md).

## What it is, and what it isn't

- **One runner, one label.** It registers to `wnzn/metro` only, with the
  single label `metro-ci-linux-x64` and none of GitHub's default labels
  (`self-hosted`, `Linux`, `X64`). A workflow reaches it only with
  `runs-on: metro-ci-linux-x64`.
- **What runs on it.** It checks, builds and tests the desktop app, and
  publishes nothing. Two workflows use this label:
  - **`ci.yml`'s `desktop` job,** automatically. That covers pull requests
    from branches of `wnzn/metro` itself into `main` by trusted authors (not
    forks, not Dependabot) and pushes to the protected `main` branch that
    don't release. It runs `nsh check` and `nsh test`; pull requests that
    change only documentation skip it.
  - **`.github/workflows/self-hosted-ci.yml`,** a manual run on the literal
    `main` branch ([below](#run-the-workflow)).
- **What stays on GitHub's runners:** everything else, including:
  - official release runs: a push to `main` that bumps the version keeps its
    `desktop` job on GitHub's runners too;
  - pull requests from forks;
  - the Rust jobs and anything that needs Docker;
  - the native Windows jobs (there are no macOS jobs for now).
- **The label only routes jobs; it doesn't authorize them.** A workflow
  change that targets it still needs review like any other.
- **No Docker access, no host files.** There's no Docker socket, no host
  directory mount (not your home folder, not the repository) and no
  published port. Nothing on your LAN can connect to it.
- **Hardened containers.** The runner runs as UID 1001 on a read-only root
  filesystem, with no capabilities and `no-new-privileges`. It's limited to
  4 CPUs, 6 GiB of memory and 2048 processes.
- **Outbound to GitHub only.** The runner sits on `metro-ci-jobs`, an
  internal bridge with no gateway. Its only way out is the `egress`
  proxy, which allows HTTPS only to `github.com`, `api.github.com`,
  `codeload.github.com` and `*.actions.githubusercontent.com`. A job can't
  install packages or reach anything else, so everything a job needs is in
  the image:
  - Ubuntu 24.04, GTK 4, WebKitGTK 6 and Xvfb;
  - the pinned NSH;
  - the TypeScript 7 compiler that NSH pins for `nsh check`
    (`@typescript/typescript-linux-x64@7.0.2`, at `/opt/typescript`, with its
    `LICENSE` and `NOTICE.txt`). `NSH_TSC=/opt/typescript/lib/tsc` makes
    `nsh` use this copy, so nothing is fetched from npm, which a job
    couldn't reach anyway.
- **No artifacts or caches.** GitHub stores those in Azure blob storage,
  which the proxy doesn't allow. GitHub's documentation lists that storage
  for job summaries and logs too. Whether a job's logs still reach GitHub
  through this proxy is part of the pending live check.
- **A persistent runner, not a fresh machine per job.** Jobs run one after
  another in the same container, as the same user (UID 1001), for as long
  as it runs.
  - **When the container starts,** the runner program is copied fresh from
    the read-only image, and `HOME` and `/tmp` start empty.
  - **Around each job,** two hooks delete files:
    - *before the job:* the contents of `HOME`, `/tmp` and the tool cache
      (`_work/_tool`);
    - *after the job:* the contents of the whole work folder, `HOME` and
      `/tmp`.
  - **What persists between jobs, until the container restarts:**
    - processes a job leaves running;
    - changes to the runner's own writable copy of its program.

    Nothing is reset per job beyond those hook deletions.
  - **A job can read the runner's registration credentials.** They're in
    the `metro-ci-runner-state` volume, which this user can read. A job
    could use them to act as this runner.

  So run only code you trust on it.

## Prerequisites

- **Linux on x86_64** with a readable `/etc/machine-id`. The runner's name,
  `metro-ci-linux-<12 hex>`, is derived from it, so each machine gets its own
  name.
- **Docker Engine 28.0 or newer** with the Compose v2 plugin. The job
  network uses the `isolated` gateway mode, which needs 28.0. Also `bash`
  and `sha256sum`.
- **GitHub CLI (`gh`) on the host**, signed in as a repository admin of
  `wnzn/metro` (today, `jenerestain`). Check it with `gh auth status`. The
  login stays on the host and never enters a container.
- **Network access for the build.** Only the build reaches beyond GitHub;
  jobs never do. It downloads:
  - the base images from Docker Hub;
  - Ubuntu packages from a pinned `snapshot.ubuntu.com` date, over HTTPS
    checked against the CA bundle from the same digest-pinned `golang`
    image the egress proxy builds with;
  - the actions runner and NSH archives from GitHub;
  - the TypeScript compiler from `registry.npmjs.org`.

  Each is checked by digest, SHA-256 or npm integrity hash.
- **A local Docker daemon.** The helper refuses unless the Docker context
  is a local `unix://` socket, so a token can never go to another machine's
  daemon.
- **Free disk space and inodes** for Docker's data. If a build or start
  fails with "no space left on device" while `df -h` shows free space, you
  may be out of inodes. Check them with `df -i /tmp` and
  `df -i "$(docker info --format '{{.DockerRootDir}}')"`.
- **The repository setting** that `setup` checks
  ([below](#who-can-run-jobs-on-it)).

## First start (once)

Run from the repository root:

```sh
gh auth status                              # an admin of wnzn/metro
ops/github-runner/runner.sh setup           # checks the policy, builds, registers
docker compose -f ops/github-runner/compose.yaml up -d
ops/github-runner/runner.sh status
```

`setup`:

1. Refuses unless `wnzn/metro` is public and requires approval for all
   outside contributors. It never changes that setting.
2. Builds the images.
3. Asks GitHub, through your `gh` login, for a short-lived registration
   token and pipes it straight into the container's standard input. The
   token never appears in a command line, the output, a file or the image.
   There's nothing to paste.

Running `setup` again is safe. It reports that the runner is already
registered (checked by the GitHub runner id stored locally), or explains
what to fix. It never replaces or deletes a runner. If GitHub already lists
a runner with this machine's name but this installation has no
registration for it, `setup` stops. See [Lost registration](#lost-registration).

`status` shows:

- the local registration;
- what GitHub reports for the runner with exactly that stored id: online or
  offline, busy, and its labels;
- the Compose containers.

It never prints a token or a credential.

## Day to day

```sh
docker compose -f ops/github-runner/compose.yaml up -d    # start (enrollment kept)
docker compose -f ops/github-runner/compose.yaml down     # stop (enrollment kept)
ops/github-runner/runner.sh status
```

- **`down` keeps the enrollment.** It stops both containers. The
  registration stays in the `metro-ci-runner-state` volume, so the next
  `up -d` reconnects without a new setup. GitHub then lists the runner as
  **Offline**, and it stays offline until you start it again. A repository
  admin can still remove it there.
- **A running job is interrupted.** Stopping waits up to 2 minutes for the
  runner to shut down. A job that's still running then fails or is
  cancelled.
- **Don't use `down -v`.** It deletes the local registration, but the
  runner stays registered on GitHub, and this installation can no longer
  remove it ([Lost registration](#lost-registration)).

## Run the workflow

Only a manual dispatch on `main` starts it. That needs write access to
`wnzn/metro`. A hosted job
first checks the exact repository, branch and workflow. Then the
self-hosted job checks the runner and the pinned NSH, checks out exactly
that commit, and runs the desktop `check`, `build --release` and tests. It
uploads and publishes nothing.

```sh
gh workflow run self-hosted-ci.yml --repo wnzn/metro --ref main -f smoke=true    # fast: Shell tests only
gh workflow run self-hosted-ci.yml --repo wnzn/metro --ref main                  # all desktop tests
gh run list --repo wnzn/metro --workflow self-hosted-ci.yml --limit 5
```

If the runner is down, the job waits in the queue until it's back. GitHub
fails it after 24 hours in the queue.

This applies to `ci.yml`'s automatic `desktop` job too. While every runner
with this label is offline, that job waits; it isn't moved to a
GitHub-hosted runner. `down` doesn't hand a queued job to anything else
either.

## Remove it

```sh
ops/github-runner/runner.sh remove
docker compose -f ops/github-runner/compose.yaml down
docker volume rm metro-ci-runner-state     # optional: remove's work left it empty
```

`remove`:

1. Stops the runner.
2. Deregisters exactly the GitHub runner whose id is in the local
   registration, and only if GitHub still shows it with this name and the
   `metro-ci-linux-x64` label. It uses a short-lived removal token from your
   `gh` login, passed the same way as at setup.
3. Deletes the local registration.

If GitHub no longer has that runner, `remove` only forgets the local
registration. Running it again is safe.

### Lost registration

Without a local registration (the volume was deleted, or the host is gone),
`remove` deletes nothing on GitHub. It never deletes a runner by its name
alone, because a cloned machine can have the same name. If GitHub still
lists a runner with this machine's name, `remove` says so.

A repository admin then decides under **Settings → Actions → Runners**:

1. Open the runner and check its labels, its status and when it was last
   seen.
2. Delete it only if it's really the lost one.

After that, `setup` can register again.

## Who can run jobs on it

`wnzn/metro` is public, and any workflow file in a pull request can name a
runner label. So the only safeguard against a fork's pull request reaching
this runner is GitHub's approval of fork pull request workflows. `setup`
requires **Settings → Actions → General → Approval for running fork pull
request workflows from contributors → Require approval for all external
contributors** (`all_external_contributors`), and refuses otherwise.

That approval is the safeguard. **Never approve a fork's workflow run that
targets `metro-ci-linux-x64`**, or that changes workflow files, unless
you've read it and trust it. Approving it runs that code on this runner,
with the access described [above](#what-it-is-and-what-it-isnt). This is
the only access model the helper supports. There's no variant that limits
the runner to a selected group of workflows.

## Keeping it current

The runner's own automatic update is turned off, so updates come only from
a reviewed change to the `Dockerfile`:

- **actions/runner:** `RUNNER_VERSION` and `RUNNER_SHA256`. With automatic
  updates off, GitHub stops sending jobs to a runner 63 days after a newer
  version becomes available, or at once for a critical security update.
  "Available" can be later than the release date, so check the [end-of-life
  schedule](https://docs.github.com/en/rest/actions/self-hosted-runners#get-runner-version-end-of-life-schedule-for-a-repository)
  rather than counting from the release page. Plan to update with every
  runner release, well inside that window.
- **Base image and packages:** the `ubuntu:24.04` digest and
  `UBUNTU_SNAPSHOT`, which move forward together.
- **The CA bundle and the proxy:** the `golang` digest, the same in the
  runner `Dockerfile` (stage `ca`) and `egress/Dockerfile`, plus the
  `debian` digest in `egress/Dockerfile`.
- **NSH and its TypeScript compiler, together:** `NSH_VERSION` and
  `NSH_SHA256` (the same pin as `ci.yml`), plus `TS_VERSION`,
  `TS_INTEGRITY` and `TS_SHA256` for the compiler that NSH version pins.

Change these only in the `Dockerfile`; never install packages into a
running runner.

After such a change is on `main`:

```sh
docker compose -f ops/github-runner/compose.yaml build
docker compose -f ops/github-runner/compose.yaml up -d
```

## Files

| File | Role |
| --- | --- |
| `runner.sh` | Host helper: `setup`, `status`, `remove` (uses `gh`) |
| `compose.yaml` | The runner and `egress` services, their networks and the state volume |
| `Dockerfile` | The runner image: pinned base, packages, runner and NSH |
| `entrypoint.sh` | Inside the container: `run`, `register`, `remove`, `forget`, `status` |
| `job-started.sh`, `job-completed.sh` | Clean `HOME`, `/tmp` and the work folder around every job |
| `egress/` | The GitHub-only HTTPS proxy ([its README](egress/README.md)) |
| `test_github_runner.py` | Contract tests for the helper, the entrypoint and the Compose file |
