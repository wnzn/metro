#!/usr/bin/env bash
# Host helper for the Metro CI runner: setup | status | remove.
#
# Uses the host's own `gh` login (a repository admin). It fetches short-lived
# registration and removal tokens and pipes them straight into the container's
# stdin (`docker compose run -T --rm`): never into argv, output, files or the
# image. No PAT or gh configuration ever enters the container.
#
# setup refuses unless wnzn/metro is public and requires approval for all
# outside contributors' fork pull request workflows
# (approval_policy=all_external_contributors). It never changes that setting.
# Every command can be run again safely.
set -euo pipefail
set +x
umask 077

readonly REPO="wnzn/metro"
readonly LABEL="metro-ci-linux-x64"
readonly POLICY="all_external_contributors"
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
readonly HERE
COMPOSE=(docker compose -f "$HERE/compose.yaml")

say() { printf 'runner.sh: %s\n' "$*"; }
die() { printf 'runner.sh: %s\n' "$*" >&2; exit 1; }

need() { command -v "$1" >/dev/null 2>&1 || die "$1 is required on the host"; }

# Stable per machine, never a secret: derived from /etc/machine-id (or the
# METRO_RUNNER_MACHINE_ID override for tests). Another machine gets another name.
runner_name() {
    local id="${METRO_RUNNER_MACHINE_ID:-}"
    if [ -z "$id" ]; then
        [ -r /etc/machine-id ] || die "cannot read /etc/machine-id for a stable runner name"
        id="$(cat /etc/machine-id)"
    fi
    [ -n "$id" ] || die "empty machine id"
    printf 'metro-ci-linux-%s' "$(printf 'metro-ci:%s' "$id" | sha256sum | cut -c1-12)"
}

check_policy() {
    local repo policy
    repo="$(gh api "repos/$REPO" --jq '.full_name + " " + .visibility')" || die "cannot read $REPO with gh"
    [ "$repo" = "$REPO public" ] || die "$REPO must be a public repository (got: $repo)"
    policy="$(gh api "repos/$REPO/actions/permissions/fork-pr-contributor-approval" --jq .approval_policy)" \
        || die "cannot read the fork pull request approval policy of $REPO"
    [ "$policy" = "$POLICY" ] || die "$REPO requires approval only for '$policy'. A repository admin must set
  Settings > Actions > General > Approval for running fork pull request workflows from contributors
  to 'Require approval for all external contributors' ($POLICY). This helper never changes it."
}

local_name() { "${COMPOSE[@]}" run -T --rm --no-deps runner status --name-only </dev/null; }

# id of the GitHub runner with exactly this name, or nothing.
remote_id() {
    gh api --paginate "repos/$REPO/actions/runners" \
        --jq ".runners[] | select(.name == \"$1\") | .id"
}

remote_has_label() {
    gh api "repos/$REPO/actions/runners/$1" --jq "[.labels[].name] | index(\"$LABEL\") != null"
}

cmd_setup() {
    local name local_reg remote
    check_policy
    name="$(runner_name)"
    "${COMPOSE[@]}" build
    local_reg="$(local_name)"
    remote="$(remote_id "$name")"
    if [ -n "$local_reg" ] && [ "$local_reg" != "$name" ]; then
        die "the state volume is registered as $local_reg, not $name; run remove first"
    elif [ -n "$local_reg" ] && [ -n "$remote" ]; then
        say "already registered as $name (GitHub id $remote)"
    elif [ -n "$local_reg" ]; then
        die "$name is registered locally but no longer exists on GitHub; run remove, then setup"
    elif [ -n "$remote" ]; then
        die "a runner named $name already exists on GitHub without local credentials; run remove (it deletes only that runner), then setup"
    else
        gh api -X POST "repos/$REPO/actions/runners/registration-token" --jq .token \
            | "${COMPOSE[@]}" run -T --rm -e METRO_RUNNER_NAME="$name" runner register
        say "registered $name. Start it with: docker compose -f ops/github-runner/compose.yaml up -d"
    fi
}

cmd_status() {
    local name
    name="$(runner_name)"
    "${COMPOSE[@]}" run -T --rm --no-deps runner status </dev/null
    gh api --paginate "repos/$REPO/actions/runners" \
        --jq ".runners[] | select(.name == \"$name\") | \"GitHub: \\(.name) id \\(.id) \\(.status) busy=\\(.busy) labels=\\([.labels[].name] | join(\",\"))\""
    "${COMPOSE[@]}" ps
}

cmd_remove() {
    local name local_reg remote
    name="$(runner_name)"
    "${COMPOSE[@]}" stop runner >/dev/null 2>&1 || true
    local_reg="$(local_name)"
    remote="$(remote_id "$name")"
    if [ -n "$local_reg" ] && [ -n "$(remote_id "$local_reg")" ]; then
        gh api -X POST "repos/$REPO/actions/runners/remove-token" --jq .token \
            | "${COMPOSE[@]}" run -T --rm runner remove
    elif [ -n "$local_reg" ]; then
        "${COMPOSE[@]}" run -T --rm --no-deps runner forget </dev/null
    elif [ -n "$remote" ]; then
        [ "$(remote_has_label "$remote")" = true ] || die "GitHub runner $name has no $LABEL label; not deleting it"
        gh api -X DELETE "repos/$REPO/actions/runners/$remote" >/dev/null
        say "deleted GitHub runner $name (id $remote)"
    else
        say "nothing to remove"
    fi
}

need gh
need docker
case "${1:-}" in
    setup) cmd_setup ;;
    status) cmd_status ;;
    remove) cmd_remove ;;
    *) die "usage: ops/github-runner/runner.sh setup | status | remove" ;;
esac
