# CI hosts

Self-hosted GitHub Actions runners for `wnzn/metro`, from 0.6.5 on:

- Linux and macOS jobs (CI and release builds) run on these hosts.
- Jobs that need Windows stay on GitHub-hosted `windows-2025`.
- Fork pull requests run only on hosted runners. Workflows must never send a
  fork PR to these labels, because jobs here run with the host user's access.

| Host | Label (only label) | User | Service |
| --- | --- | --- | --- |
| Debian 13 (`debian`) | `metro-ci-linux-docker` | `metro-ci`: system user, no sudo, not in `docker`, password locked | `metro-ci-runner.service` (system unit, `User=metro-ci`) |
| Mac (arm64) | `metro-ci-macos-arm64` | `metro-ci` | LaunchDaemon `dev.metro.ci-runner` |

Never print or log a runner token. Write it to a mode-600 file on the host,
and let the script delete it after use.

## Debian

1. As root: `sudo bash ops/ci-hosts/debian-setup.sh`. This script:
   - installs `uidmap`, `slirp4netns`, `dbus-user-session` and the packages
     that jobs would otherwise `sudo apt-get install`
   - creates `metro-ci` with subuid/subgid `231072:65536` and linger
   - installs the invoking user's `~/.ssh/id_rsa.pub` as metro-ci's only SSH key
   - writes the units: the runner service, plus `metro-ci-runner.path`, which
     starts the service once `~/actions-runner/.metro-configured` exists
2. As metro-ci, with no sudo:

   ```sh
   scp -r ops/ci-hosts metro-ci@localhost:ci-hosts-src
   gh api -X POST repos/wnzn/metro/actions/runners/registration-token --jq .token \
     | ssh metro-ci@localhost 'umask 077; cat > .reg-token'
   ssh metro-ci@localhost 'bash ci-hosts-src/debian-runner.sh ~/.reg-token'
   ```

   `debian-runner.sh` does the following:
   - sets up rootless Docker as a systemd `--user` unit
   - installs rustup 1.29.3 and Rust 1.98.1, checked by sha256
   - installs actions/runner 2.338.2, checked by the release asset's sha256
   - installs the hooks
   - runs `config.sh`, passing the token in its environment
   - writes `.env` and `.path`, then the marker file
3. Optional: `sudo bash ops/ci-hosts/debian-setup.sh finalize` removes the
   SSH key and sets the shell to nologin.

Limits apply to `user-<uid>.slice`, which holds the runner, the rootless
dockerd and its containers: `MemoryHigh=14G`, `MemoryMax=16G`,
`MemorySwapMax=2G`, `CPUQuota=1200%`, `TasksMax=8192`.

The unit uses `NoNewPrivileges`, `PrivateTmp`, `ProtectSystem=full`, and
`ProtectHome=tmpfs` with only `/home/metro-ci` and `/run/user/<uid>` bound.
The rootless dockerd runs outside that unit, so a job's `docker run -v` can
reach anything metro-ci can read. Keep other homes at mode 700.

## Mac

As metro-ci on the Mac:

```sh
scp mac/* metro-ci@100.109.87.53:actions-runner/ci-hosts-src/
ssh metro-ci@100.109.87.53 'bash ~/actions-runner/ci-hosts-src/install.sh'
```

This installs the following:
- Python 3.13.18 (python-build-standalone, checked by sha256) in
  `~/.local/opt/python`. The system Python is 3.9, and the build needs 3.11 or
  later.
- Rust 1.98.1 with the target `aarch64-apple-darwin`.
- The hooks, set in `.env`.
- A job PATH, set in `.path`.

Xcode Command Line Tools provide `codesign`, `plutil` and `git`. Jobs
download NSH themselves, pinned by digest.

Restart the runner to load `.env` and `.path`. Use either:
- `sudo launchctl kickstart -k system/dev.metro.ci-runner`
- or, as metro-ci while the runner is idle, `pkill -TERM -f Runner.Listener`.
  launchd's `KeepAlive` starts it again.

## Job hooks

`linux/job-cleanup.sh` and `mac/job-cleanup.sh` run as both
`ACTIONS_RUNNER_HOOK_JOB_STARTED` (`started`) and
`ACTIONS_RUNNER_HOOK_JOB_COMPLETED` (`completed`).

| Phase | What it clears |
| --- | --- |
| `started` | the tool cache (`_work/_tool`) |
| `completed` | all of `_work` |
| both | temp dirs, and everything in HOME outside the persist list below |

On Linux, temp is the unit's private `/tmp` and `/var/tmp`. On the Mac, it's
the per-user `DARWIN_USER_TEMP_DIR` and metro-ci's own entries in `/private/tmp`.

What persists between jobs:
- `~/.rustup` (toolchains).
- `~/.cargo/registry` and `~/.cargo/git`: download caches, without unpacked
  `registry/src`.
- `~/.cargo/bin`, trimmed to the rustup proxies.
- `~/.ssh`.
- Linux only:
  - rootless Docker config and data (`~/.config/docker`,
    `~/.config/systemd`, `~/.local/share/docker`)
  - pulled images, deleted when unused for 7 days
  - build cache, capped at 20 GB
- Mac only:
  - `~/.local/opt` (Python)
  - `~/Library`, without `Caches`

What doesn't persist:
- On Linux, containers, volumes and networks are removed.
- `~/.docker` (registry logins), cargo config, `cargo install` binaries,
  Node, npm and pip caches are removed.

The persistent caches assume trusted jobs: same-repo branches only, never
forks. A job could poison an image tag or a toolchain for the next job.
Jobs run as the same user as the hooks, so the hooks are hygiene, not an
isolation boundary.
