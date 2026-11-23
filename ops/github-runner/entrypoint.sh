#!/usr/bin/env bash
# Metro CI runner container entrypoint: run | register | remove | forget | status.
#
# - The repository is fixed to wnzn/metro, the label to metro-ci-linux-x64,
#   and the scope to the repository. There's no --replace, so registration
#   never takes over a runner that another machine registered under that name.
# - The registration or removal token arrives only on stdin. It is passed to
#   config.sh in that process's environment (ACTIONS_RUNNER_INPUT_TOKEN), never
#   in argv, and it is never echoed or written to disk.
# - /state keeps only .runner, .credentials and .credentials_rsaparams, so the
#   registration survives `docker compose down` and `up`.
# - Every start copies the runner from the read-only image into the empty
#   /runner scratch volume, and runs with automatic updates disabled.
set -euo pipefail
umask 077

readonly REPO_URL="https://github.com/wnzn/metro"
readonly LABEL="metro-ci-linux-x64"
readonly SRC="${METRO_RUNNER_SRC:-/opt/actions-runner}"
readonly SCRATCH="${METRO_RUNNER_SCRATCH:-/runner}"
readonly ROOT="$SCRATCH/actions-runner"
readonly STATE="${METRO_RUNNER_STATE:-/state}"
readonly HOOKS="${METRO_RUNNER_HOOKS:-/opt/metro-runner}"
readonly STATE_FILES=(.runner .credentials .credentials_rsaparams)
readonly NAME_RE='^metro-ci-linux-[a-z0-9][a-z0-9-]{0,39}$'
readonly TOKEN_RE='^[A-Za-z0-9]{20,200}$'

say() { printf 'metro-ci-runner: %s\n' "$*"; }
die() { printf 'metro-ci-runner: %s\n' "$*" >&2; exit 1; }

configured() {
    local f
    for f in "${STATE_FILES[@]}"; do [ -s "$STATE/$f" ] || return 1; done
}

# A field of the saved .runner settings (written with a UTF-8 BOM).
setting() {
    python3 -I -c 'import json,sys; v=json.load(open(sys.argv[1], encoding="utf-8-sig")).get(sys.argv[2]); print("" if v is None else v)' \
        "$STATE/.runner" "$1"
}

# Reads one token line from stdin; the value never reaches argv or output.
read_token() {
    local token=""
    IFS= read -r token || true
    token="${token%$'\r'}"
    [[ "$token" =~ $TOKEN_RE ]] || die "expected a GitHub runner token on stdin (from the host helper)"
    printf '%s' "$token"
}

# A pristine runner in the scratch volume: nothing from a previous start remains.
fresh_root() {
    [ -d "$SCRATCH" ] && [ -w "$SCRATCH" ] || die "$SCRATCH must be a writable scratch volume"
    find "$SCRATCH" -mindepth 1 -maxdepth 1 -exec rm -rf -- {} +
    cp -R "$SRC" "$ROOT"
    chmod -R u+w "$ROOT"
}

restore_state() {
    local f
    for f in "${STATE_FILES[@]}"; do install -m 0600 "$STATE/$f" "$ROOT/$f"; done
}

save_state() {
    local f
    for f in "${STATE_FILES[@]}"; do
        [ -s "$ROOT/$f" ] || die "config.sh did not write $f"
        install -m 0600 "$ROOT/$f" "$STATE/$f.new"
    done
    for f in "${STATE_FILES[@]}"; do mv -f "$STATE/$f.new" "$STATE/$f"; done
}

forget_state() {
    local f
    for f in "${STATE_FILES[@]}"; do rm -f -- "$STATE/$f" "$STATE/$f.new"; done
}

check_saved() {
    local url name
    url="$(setting gitHubUrl)"
    name="$(setting agentName)"
    [ "$url" = "$REPO_URL" ] || die "the saved registration is not for $REPO_URL"
    [[ "$name" =~ $NAME_RE ]] || die "the saved registration has an unexpected runner name"
}

cmd_register() {
    local name="${METRO_RUNNER_NAME:-}" token
    [[ "$name" =~ $NAME_RE ]] || die "METRO_RUNNER_NAME must match $NAME_RE"
    if configured; then
        cat >/dev/null  # drain the unused token
        check_saved
        [ "$(setting agentName)" = "$name" ] || die "this volume is registered as $(setting agentName); remove it first"
        say "already registered as $name"
        return 0
    fi
    token="$(read_token)"
    fresh_root
    cd "$ROOT"
    ACTIONS_RUNNER_INPUT_TOKEN="$token" ./config.sh --unattended \
        --url "$REPO_URL" --name "$name" --labels "$LABEL" --no-default-labels \
        --work _work --disableupdate
    token=""
    save_state
    say "registered $name for $REPO_URL with label $LABEL"
}

cmd_remove() {
    local token
    if ! configured; then
        cat >/dev/null
        say "no local registration"
        return 0
    fi
    check_saved
    token="$(read_token)"
    fresh_root
    restore_state
    cd "$ROOT"
    ACTIONS_RUNNER_INPUT_TOKEN="$token" ./config.sh remove --unattended
    token=""
    forget_state
    say "removed the registration from $REPO_URL"
}

cmd_forget() {
    forget_state
    say "local registration files deleted"
}

cmd_status() {
    if configured; then
        check_saved
        if [ "${1:-}" = "--name-only" ]; then setting agentName; return 0; fi
        if [ "${1:-}" = "--identity" ]; then
            printf '%s %s %s\n' "$(setting agentName)" "$(setting agentId)" "$(setting gitHubUrl)"
            return 0
        fi
        say "registered: $(setting agentName) (id $(setting agentId)) for $(setting gitHubUrl)"
    else
        case "${1:-}" in --name-only|--identity) return 0 ;; esac
        say "not registered"
    fi
}

cmd_run() {
    local stop=0
    trap 'stop=1' TERM INT
    while ! configured; do
        [ "$stop" = 1 ] && exit 0
        say "not registered: run ops/github-runner/runner.sh setup on the host"
        sleep 30 & wait $! || true
    done
    trap - TERM INT
    check_saved
    fresh_root
    restore_state
    # Fresh HOME and /tmp (tmpfs) on every start; the hooks clean between jobs.
    find "$HOME" /tmp -mindepth 1 -maxdepth 1 -exec rm -rf -- {} + 2>/dev/null || true
    cd "$ROOT"
    {
        local v
        for v in HTTP_PROXY HTTPS_PROXY http_proxy https_proxy NO_PROXY no_proxy; do
            [ -n "${!v:-}" ] && printf '%s=%s\n' "$v" "${!v}"
        done
        printf 'LANG=%s\n' "${LANG:-C.UTF-8}"
    } > .env
    printf '%s\n' "$PATH" > .path
    export ACTIONS_RUNNER_HOOK_JOB_STARTED="$HOOKS/job-started.sh"
    export ACTIONS_RUNNER_HOOK_JOB_COMPLETED="$HOOKS/job-completed.sh"
    say "starting $(setting agentName)"
    exec ./run.sh
}

case "${1:-run}" in
    run) cmd_run ;;
    register) cmd_register ;;
    remove) cmd_remove ;;
    forget) cmd_forget ;;
    status) shift; cmd_status "$@" ;;
    *) die "usage: run | register | remove | forget | status [--name-only | --identity]" ;;
esac
