# Self-hosted GitHub Actions runner (`metro-ci-linux-x64`)

One Linux x64 runner for the public repository `wnzn/metro`, run with Docker
Compose. It's for Metro's own CI only. It has nothing to do with Metro's
personal or Space runners (`metro-runner`), and it never signs anything:
the Windows signer and every release job stay on their own workflows.
GitHub-hosted Windows and macOS jobs aren't affected.

> **Status: not yet verified live.** The files and their contract tests are
> in the source:
>
> - the runner: `fdbbec1`;
> - the egress proxy: `5d06302`;
> - the manual workflow: `b9934f1`.
>
> Nobody has yet recorded the runner online in GitHub, running a job, then
> showing offline after `down`. Two setup choices are still open:
>
> - the repository's fork pull request approval policy (see
>   [Who can run jobs on it](#who-can-run-jobs-on-it));
> - how to handle the host's inode limit (see [Prerequisites](#prerequisites)).

## What it is, and what it isn't

- **One runner, one label.** It registers to `wnzn/metro` only, with the
  single label `metro-ci-linux-x64` and none of GitHub's default labels
  (`self-hosted`, `Linux`, `X64`). A workflow reaches it only with
  `runs-on: metro-ci-linux-x64`.
- **Its only intended workload** is `.github/workflows/self-hosted-ci.yml`.
  That's a manual run on the literal `main` branch: it checks, builds and
  tests the desktop app, and publishes nothing. Untrusted pull requests
  from forks are never its intended workload. Other workflows don't use
  this label.
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
  the image (Ubuntu 24.04, GTK 4, WebKitGTK 6, Xvfb, the pinned NSH).
- **No artifacts or caches.** GitHub stores those in Azure blob storage,
  which the proxy doesn't allow. GitHub's documentation lists that storage
  for job summaries and logs too. Whether a job's logs still reach GitHub
  through this proxy is part of the pending live check.
- **Each job starts clean.** Only the registration is kept, in the Docker
  volume `metro-ci-runner-state`. The runner program, the work folder,
  `HOME` and `/tmp` are recreated at every start, and they're cleaned before
  and after every job.

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
- **Room for the build.** Building the images downloads Ubuntu packages from
  a pinned `snapshot.ubuntu.com` date, the base images from Docker Hub, and
  the actions runner and NSH archives from GitHub. Each is checked by digest
  or SHA-256.
- **Free disk and inodes** where Docker keeps its data. **Open:** this host
  has run out of inodes on a tmpfs before, and how to handle that here is
  still to be decided. Check `df -h` and `df -i` for Docker's data folder
  first.
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
registered, or explains what to fix.

`status` shows:

- the local registration;
- what GitHub reports for this runner: online or offline, busy, and its
  labels;
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
- **Don't use `down -v`.** It deletes the local registration, but the runner
  stays registered on GitHub. If that happens, run `runner.sh remove`, then
  `setup`.

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

## Remove it

```sh
ops/github-runner/runner.sh remove
docker compose -f ops/github-runner/compose.yaml down
docker volume rm metro-ci-runner-state     # optional: remove's work left it empty
```

`remove`:

1. Stops the runner.
2. Deregisters it with a short-lived removal token from your `gh` login,
   passed the same way as at setup.
3. Deletes the local registration.

It also cleans up a half-finished state. If only GitHub knows the runner, it
deletes that one runner, and only if it carries the `metro-ci-linux-x64`
label. Running it again is safe.

If this host is gone, a repository admin can remove the runner on GitHub
under **Settings → Actions → Runners**.

## Who can run jobs on it

`wnzn/metro` is public, and any workflow file in a pull request can name a
runner label. So the only safeguard against a fork's pull request reaching
this runner is GitHub's approval of fork pull request workflows. `setup`
requires **Settings → Actions → General → Approval for running fork pull
request workflows from contributors → Require approval for all external
contributors** (`all_external_contributors`), and refuses otherwise.

**Pending JM's decision:** keep that setting, or use a different access
model. Only the setting above is implemented today. There's no variant that
limits the runner to a selected group of workflows.

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
- **The proxy:** the `golang` and `debian` digests in `egress/Dockerfile`.
- **NSH:** `NSH_VERSION` and `NSH_SHA256`, the same pin as `ci.yml`.

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
