#!/bin/bash
# Job hook for the macOS runner (user metro-ci). Usage: job-cleanup.sh started|completed
# Removes what a job may leave behind. Persists only: actions-runner itself,
# ~/.rustup (toolchains), ~/.cargo registry/git download caches and rustup proxies,
# ~/.local/opt (pinned Python), ~/.ssh, ~/Library (minus Caches) and the macOS
# standard folders. Hygiene only: jobs run as the same user and could edit this file.
set -u
phase="${1:-completed}"
home="${METRO_CI_TEST_HOME:-/Users/metro-ci}"
runner="$home/actions-runner"

wipe() { [ -d "$1" ] && find "$1" -mindepth 1 -maxdepth 1 -exec rm -rf -- {} + 2>/dev/null; return 0; }

# HOME: allowlist of top-level entries.
keep=" actions-runner .rustup .cargo .local .ssh Library .CFUserTextEncoding Desktop Documents Downloads Movies Music Pictures Public .Trash "
for p in "$home"/* "$home"/.[!.]* "$home"/..?*; do
  [ -e "$p" ] || [ -L "$p" ] || continue
  case "$keep" in *" ${p##*/} "*) ;; *) rm -rf -- "$p" ;; esac
done
# ~/.cargo: keep download caches and the rustup proxies only (no config, no cargo-installed bins).
if [ -d "$home/.cargo" ]; then
  for p in "$home"/.cargo/* "$home"/.cargo/.[!.]*; do
    [ -e "$p" ] || [ -L "$p" ] || continue
    case "${p##*/}" in registry|git|bin|env) ;; *) rm -rf -- "$p" ;; esac
  done
  rm -rf -- "$home/.cargo/registry/src"   # unpacked sources; re-extracted from registry/cache
  proxies=" cargo cargo-clippy cargo-fmt cargo-miri clippy-driver rls rust-analyzer rust-gdb rust-gdbgui rust-lldb rustc rustdoc rustfmt rustup "
  for p in "$home"/.cargo/bin/* "$home"/.cargo/bin/.[!.]*; do
    [ -e "$p" ] || [ -L "$p" ] || continue
    case "$proxies" in *" ${p##*/} "*) ;; *) rm -rf -- "$p" ;; esac
  done
fi
# ~/.local: only the pinned toolchains under opt.
for p in "$home"/.local/* "$home"/.local/.[!.]*; do
  [ -e "$p" ] || continue
  [ "${p##*/}" = opt ] || rm -rf -- "$p"
done
for d in Desktop Documents Downloads Movies Music Pictures Public .Trash; do wipe "$home/$d"; done
wipe "$home/Library/Caches"

# Per-user temp dirs and this user's files in the shared /tmp.
if [ -z "${METRO_CI_TEST_HOME:-}" ]; then
  wipe "$(getconf DARWIN_USER_TEMP_DIR 2>/dev/null || echo /nonexistent)"
  find /private/tmp /private/var/tmp -mindepth 1 -maxdepth 1 -user "$(id -u)" -exec rm -rf -- {} + 2>/dev/null
fi

# Work dir: started clears the tool cache; completed clears everything.
if [ "$phase" = started ]; then
  rm -rf -- "$runner/_work/_tool"
else
  wipe "$runner/_work"
fi
exit 0
