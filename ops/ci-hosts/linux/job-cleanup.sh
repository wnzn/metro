#!/bin/bash
# Job hook for the Debian runner (user metro-ci). Usage: job-cleanup.sh started|completed
# Persists only: actions-runner, ~/.rustup (toolchains), ~/.cargo registry/git download
# caches and rustup proxies, ~/.ssh, the rootless Docker daemon's config and data
# (~/.config/{docker,systemd}, ~/.local/share/docker: pulled images and build cache,
# capped below). Containers, volumes, networks and CLI config (~/.docker, registry
# logins) go. /tmp and /var/tmp are the unit's private ones. Hygiene only: jobs run as
# the same user and could edit this file.
set -u
phase="${1:-completed}"
home="${METRO_CI_TEST_HOME:-/home/metro-ci}"
runner="$home/actions-runner"

wipe() { [ -d "$1" ] && find "$1" -mindepth 1 -maxdepth 1 -exec rm -rf -- {} + 2>/dev/null; return 0; }
prune_dir() {  # prune_dir DIR "names to keep"
  local p
  for p in "$1"/* "$1"/.[!.]* "$1"/..?*; do
    [ -e "$p" ] || [ -L "$p" ] || continue
    case " $2 " in *" ${p##*/} "*) ;; *) rm -rf -- "$p" ;; esac
  done
}

if [ -z "${METRO_CI_TEST_HOME:-}" ] && command -v docker >/dev/null; then
  ids="$(docker ps -aq 2>/dev/null)"; [ -z "$ids" ] || docker rm -f $ids >/dev/null 2>&1
  docker volume prune -af >/dev/null 2>&1
  docker network prune -f >/dev/null 2>&1
  if [ "$phase" = completed ]; then
    docker image prune -f >/dev/null 2>&1
    docker image prune -af --filter until=168h >/dev/null 2>&1
    docker builder prune -f --max-used-space 20GB >/dev/null 2>&1
  fi
fi

prune_dir "$home" "actions-runner .rustup .cargo .ssh .config .local"
prune_dir "$home/.config" "docker systemd"
prune_dir "$home/.local" "share"
prune_dir "$home/.local/share" "docker"
if [ -d "$home/.cargo" ]; then
  prune_dir "$home/.cargo" "registry git bin env"
  rm -rf -- "$home/.cargo/registry/src"
  prune_dir "$home/.cargo/bin" "cargo cargo-clippy cargo-fmt cargo-miri clippy-driver rls rust-analyzer rust-gdb rust-gdbgui rust-lldb rustc rustdoc rustfmt rustup"
fi

[ -n "${METRO_CI_TEST_HOME:-}" ] || { wipe /tmp; wipe /var/tmp; }

if [ "$phase" = started ]; then
  rm -rf -- "$runner/_work/_tool"
else
  wipe "$runner/_work"
fi
exit 0
