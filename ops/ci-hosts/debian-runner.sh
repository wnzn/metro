#!/bin/bash
# metro-ci part of the Debian CI host, after debian-setup.sh. No sudo.
# Usage (as metro-ci, from a copy of ops/ci-hosts): debian-runner.sh TOKEN_FILE
# TOKEN_FILE holds a repo runner registration token (mode 600); it is deleted after use.
set -euo pipefail
here="$(cd "$(dirname "$0")" && pwd)"
token_file="${1:?usage: debian-runner.sh TOKEN_FILE}"
[ "$(id -un)" = metro-ci ] || { echo "run as metro-ci" >&2; exit 1; }
trap 'rm -f "$token_file"' EXIT
runner="$HOME/actions-runner"
export XDG_RUNTIME_DIR="/run/user/$(id -u)"

RUNNER_VERSION=2.338.2
RUNNER_SHA256=baebf204307bac6193480852732f0aba2ae97b6c5597c244f4d5558390147c40   # release asset digest
RUSTUP_SHA256=b1c8cb14e30889e7e2999e6b69e62d9e9b9ffc0f4df8180564d306d9680e6c55   # rustup-init 1.29.3
RUST_TOOLCHAIN=1.98.1

fetch() {  # fetch URL SHA256 DEST
  curl --fail --silent --show-error --location --retry 3 -o "$3" "$1"
  printf '%s  %s\n' "$2" "$3" | sha256sum -c --quiet -
}

# Rootless Docker for this user (systemd --user unit, kept alive by linger).
if ! systemctl --user is-active --quiet docker; then
  PATH="/usr/sbin:/sbin:$PATH" dockerd-rootless-setuptool.sh install   # unit keeps this PATH (iptables)
fi
mkdir -p "$HOME/.config/docker"
cat > "$HOME/.config/docker/daemon.json" <<'EOF'
{ "log-driver": "local", "log-opts": { "max-size": "20m", "max-file": "2" } }
EOF
systemctl --user restart docker
# Desktop audio services the user manager would otherwise start for this account.
systemctl --user mask --now pipewire.service pipewire.socket pipewire-pulse.service \
  pipewire-pulse.socket wireplumber.service filter-chain.service >/dev/null 2>&1 || true
DOCKER_HOST="unix://$XDG_RUNTIME_DIR/docker.sock" docker info --format '{{.SecurityOptions}}' | grep -q rootless

# Rust: rustup with the repo's pinned toolchain.
if [ ! -x "$HOME/.cargo/bin/rustup" ]; then
  dl="$(mktemp -d)"
  fetch https://static.rust-lang.org/rustup/archive/1.29.3/x86_64-unknown-linux-gnu/rustup-init "$RUSTUP_SHA256" "$dl/rustup-init"
  chmod +x "$dl/rustup-init"
  "$dl/rustup-init" -y --no-modify-path --profile minimal --default-toolchain none
  rm -rf "$dl"
fi
"$HOME/.cargo/bin/rustup" toolchain install "$RUST_TOOLCHAIN" --profile minimal --component rustfmt --component clippy
"$HOME/.cargo/bin/rustup" default "$RUST_TOOLCHAIN"

# actions/runner, pinned by the release asset's sha256.
if [ ! -x "$runner/config.sh" ]; then
  dl="$(mktemp -d)"
  fetch "https://github.com/actions/runner/releases/download/v$RUNNER_VERSION/actions-runner-linux-x64-$RUNNER_VERSION.tar.gz" "$RUNNER_SHA256" "$dl/runner.tgz"
  mkdir -p "$runner"
  tar -xzf "$dl/runner.tgz" -C "$runner"
  rm -rf "$dl"
fi

install -d -m 700 "$runner/hooks"
install -m 700 "$here/linux/job-cleanup.sh" "$runner/hooks/job-cleanup.sh"
printf '#!/bin/bash\nexec "%s" started\n' "$runner/hooks/job-cleanup.sh" > "$runner/hooks/job-started.sh"
printf '#!/bin/bash\nexec "%s" completed\n' "$runner/hooks/job-cleanup.sh" > "$runner/hooks/job-completed.sh"
chmod 700 "$runner/hooks/job-started.sh" "$runner/hooks/job-completed.sh"

cd "$runner"
if [ ! -s .runner ]; then
  token="$(tr -d '\r\n' < "$token_file")"
  ACTIONS_RUNNER_INPUT_TOKEN="$token" ./config.sh --unattended --replace \
    --url https://github.com/wnzn/metro --name metro-ci-linux-docker \
    --labels metro-ci-linux-docker --no-default-labels --work _work >/dev/null
  token=""
fi

umask 077
cat > .env <<EOF
LANG=C.UTF-8
DOCKER_HOST=unix://$XDG_RUNTIME_DIR/docker.sock
ACTIONS_RUNNER_HOOK_JOB_STARTED=$runner/hooks/job-started.sh
ACTIONS_RUNNER_HOOK_JOB_COMPLETED=$runner/hooks/job-completed.sh
EOF
printf '%s\n' "$HOME/.cargo/bin:/usr/local/bin:/usr/bin:/bin" > .path
touch .metro-configured   # metro-ci-runner.path starts the service
echo "configured metro-ci-linux-docker"
