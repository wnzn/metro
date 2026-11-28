#!/bin/bash
# Run as metro-ci on the Mac, from a copy of ops/ci-hosts/mac. No sudo.
# Installs the build toolchain for the macOS desktop job, the job hooks and the
# runner's job PATH. Restart the runner afterwards (README).
set -euo pipefail
here="$(cd "$(dirname "$0")" && pwd)"
runner="$HOME/actions-runner"

# Python >= 3.11 (system python3 is 3.9): python-build-standalone, pinned by sha256.
PY_URL='https://github.com/astral-sh/python-build-standalone/releases/download/20261124/cpython-3.13.18%2B20261124-aarch64-apple-darwin-install_only.tar.gz'
PY_SHA256='ae7030747b3daddea6925f4e270918c5d977f0347fe5d03adae94ce3a630ed74'
py="$HOME/.local/opt/python-3.13.18"
if [ ! -x "$py/bin/python3" ]; then
  dl="$(mktemp -d)"
  curl --fail --silent --show-error --location --retry 3 -o "$dl/py.tgz" "$PY_URL"
  printf '%s  %s\n' "$PY_SHA256" "$dl/py.tgz" | shasum -a 256 -c -
  mkdir -p "$dl/x" "$HOME/.local/opt"
  tar -xzf "$dl/py.tgz" -C "$dl/x"
  mv "$dl/x/python" "$py"
  rm -rf "$dl"
fi
ln -sfn "$py" "$HOME/.local/opt/python"
"$HOME/.local/opt/python/bin/python3" -I -c 'import sys; assert sys.version_info >= (3, 11)'

# Rust: the repo's pinned toolchain with the macOS target (jobs also run rustup install).
"$HOME/.cargo/bin/rustup" toolchain install 1.98.1 --profile minimal --target aarch64-apple-darwin

# Hooks live inside actions-runner (kept by the cleanup allowlist).
install -d -m 700 "$runner/hooks"
install -m 700 "$here/job-cleanup.sh" "$runner/hooks/job-cleanup.sh"
printf '#!/bin/bash\nexec "%s" started\n' "$runner/hooks/job-cleanup.sh" > "$runner/hooks/job-started.sh"
printf '#!/bin/bash\nexec "%s" completed\n' "$runner/hooks/job-cleanup.sh" > "$runner/hooks/job-completed.sh"
chmod 700 "$runner/hooks/job-started.sh" "$runner/hooks/job-completed.sh"

env_file="$runner/.env"
touch "$env_file"; chmod 600 "$env_file"
grep -v -E '^ACTIONS_RUNNER_HOOK_JOB_(STARTED|COMPLETED)=' "$env_file" > "$env_file.new" || true
printf 'ACTIONS_RUNNER_HOOK_JOB_STARTED=%s\nACTIONS_RUNNER_HOOK_JOB_COMPLETED=%s\n' \
  "$runner/hooks/job-started.sh" "$runner/hooks/job-completed.sh" >> "$env_file.new"
mv "$env_file.new" "$env_file"

# Job PATH (the runner reads .path at start).
printf '%s\n' "$HOME/.local/opt/python/bin:$HOME/.cargo/bin:/usr/bin:/bin:/usr/sbin:/sbin" > "$runner/.path"
echo "installed; restart the runner to load .env and .path"
