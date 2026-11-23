#!/usr/bin/env bash
# Host helper for the Metro CI runner: setup | status | remove.
#
# - Uses the host's own `gh` login (a wnzn/metro admin), always against
#   github.com (--hostname github.com, with GH_HOST, GH_REPO and the
#   enterprise-token variables cleared). Registration and removal tokens are
#   short-lived. They go from `gh` straight into the container's stdin
#   (`docker compose run -T --rm`), never into argv, output, files or the
#   image. No PAT or gh configuration enters the container.
# - Docker runs from this directory with a fixed project name, an empty env
#   file and no COMPOSE_* overrides, and only against a local (unix://)
#   daemon, so a token can't be sent to another machine's daemon.
# - setup refuses unless wnzn/metro is public and requires approval for all
#   outside contributors' fork pull request workflows
#   (approval_policy=all_external_contributors). It never changes that setting.
# - The local registration's agentId is authoritative. A GitHub runner is
#   only ever acted on through the exact id, repository and name stored in
#   this installation's state volume. Without a local registration, remove
#   deletes nothing on GitHub, even if a runner there has this machine's name
#   (machine ids can be duplicated on cloned machines).
# Each command can be run again safely.
set -euo pipefail
set +x
umask 077

readonly REPO="wnzn/metro"
readonly REPO_URL="https://github.com/wnzn/metro"
readonly LABEL="metro-ci-linux-x64"
readonly POLICY="all_external_contributors"
readonly PROJECT="metro-ci-runner"
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"
readonly HERE
cd "$HERE"

say() { printf 'runner.sh: %s\n' "$*"; }
die() { printf 'runner.sh: %s\n' "$*" >&2; exit 1; }
need() { command -v "$1" >/dev/null 2>&1 || die "$1 is required on the host"; }

# gh, always github.com and never redirected by the environment or the cwd.
gh_api() {
    env -u GH_HOST -u GH_REPO -u GH_ENTERPRISE_TOKEN -u GITHUB_ENTERPRISE_TOKEN \
        gh api --hostname github.com "$@"
}

# docker compose for this stack only: no COMPOSE_* overrides and no .env.
compose() {
    env -u COMPOSE_FILE -u COMPOSE_PROJECT_NAME -u COMPOSE_PROFILES -u COMPOSE_ENV_FILES \
        -u COMPOSE_PATH_SEPARATOR -u COMPOSE_REMOVE_ORPHANS \
        docker compose --project-directory "$HERE" -p "$PROJECT" --env-file /dev/null \
        -f "$HERE/compose.yaml" "$@"
}

# Tokens only ever go to a daemon on this machine.
check_local_daemon() {
    local endpoint="${DOCKER_HOST:-}"
    if [ -z "$endpoint" ]; then
        endpoint="$(docker context inspect --format '{{.Endpoints.docker.Host}}' 2>/dev/null)" \
            || die "cannot inspect the current Docker context"
    fi
    case "$endpoint" in
        unix://*) ;;
        *) die "the Docker daemon must be local (unix://...), not $endpoint: a runner token would leave this machine" ;;
    esac
}

# Stable per machine, never a secret: derived from /etc/machine-id (or the
# METRO_RUNNER_MACHINE_ID override for tests). Only a default for new
# registrations; existing ones are identified by their stored agentId.
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
    repo="$(gh_api "repos/$REPO" --jq '.full_name + " " + .visibility')" || die "cannot read $REPO with gh"
    [ "$repo" = "$REPO public" ] || die "$REPO must be a public repository (got: $repo)"
    policy="$(gh_api "repos/$REPO/actions/permissions/fork-pr-contributor-approval" --jq .approval_policy)" \
        || die "cannot read the fork pull request approval policy of $REPO"
    [ "$policy" = "$POLICY" ] || die "$REPO requires approval only for '$policy'. A repository admin must set
  Settings > Actions > General > Approval for running fork pull request workflows from contributors
  to 'Require approval for all external contributors' ($POLICY). This helper never changes it."
}

# "<name> <agentId> <gitHubUrl>" of the local registration, or nothing.
local_identity() { compose run -T --rm --no-deps runner status --identity </dev/null; }

# "<name> <has label>" of the GitHub runner with exactly this id, or nothing if
# there is none (HTTP 404).
remote_by_id() {
    local out
    if out="$(gh_api "repos/$REPO/actions/runners/$1" \
        --jq '.name + " " + ([.labels[].name] | index("'"$LABEL"'") != null | tostring)' 2>&1)"; then
        printf '%s' "$out"
    elif [[ "$out" == *"HTTP 404"* ]]; then
        return 0
    else
        die "cannot read GitHub runner id $1"
    fi
}

remote_name_exists() {
    [ -n "$(gh_api --paginate "repos/$REPO/actions/runners" --jq ".runners[] | select(.name == \"$1\") | .id")" ]
}

# Parses and checks the local identity. Prints "<name> <id>".
checked_local() {
    local name id url extra
    read -r name id url extra <<<"$1" || true
    [ -z "${extra:-}" ] && [ "$url" = "$REPO_URL" ] && [[ "$name" =~ ^metro-ci-linux-[a-z0-9][a-z0-9-]{0,39}$ ]] \
        && [[ "$id" =~ ^[1-9][0-9]{0,18}$ ]] || die "the local registration is not a $REPO runner with a valid agent id"
    printf '%s %s' "$name" "$id"
}

cmd_setup() {
    local name ident ids lname lid remote
    check_local_daemon
    check_policy
    name="$(runner_name)"
    compose build
    ident="$(local_identity)"
    if [ -n "$ident" ]; then
        ids="$(checked_local "$ident")"
        read -r lname lid <<<"$ids"
        remote="$(remote_by_id "$lid")"
        [ -n "$remote" ] || die "$lname (id $lid) is registered locally but no longer exists on GitHub; run remove (it only forgets the local registration), then setup"
        [ "$remote" = "$lname true" ] || die "GitHub runner id $lid is not $lname with label $LABEL; refusing to use it"
        say "already registered as $lname (GitHub id $lid)"
        return 0
    fi
    if remote_name_exists "$name"; then
        die "a runner named $name already exists on $REPO, but this installation has no credentials for it.
  It may belong to a cloned machine or a lost volume. A repository admin can delete it under
  Settings > Actions > Runners if it is really gone; this helper never deletes or replaces it."
    fi
    gh_api -X POST "repos/$REPO/actions/runners/registration-token" --jq .token \
        | compose run -T --rm -e METRO_RUNNER_NAME="$name" runner register
    say "registered $name. Start it with: docker compose -f ops/github-runner/compose.yaml up -d"
}

cmd_status() {
    local ident ids lname lid remote
    compose run -T --rm --no-deps runner status </dev/null
    ident="$(local_identity)"
    if [ -n "$ident" ]; then
        ids="$(checked_local "$ident")"
        read -r lname lid <<<"$ids"
        remote="$(remote_by_id "$lid")"
        if [ -z "$remote" ]; then
            say "GitHub: no runner with id $lid (removed there)"
        else
            gh_api "repos/$REPO/actions/runners/$lid" \
                --jq '"GitHub: \(.name) id \(.id) \(.status) busy=\(.busy) labels=\([.labels[].name] | join(","))"'
        fi
    fi
    compose ps
}

cmd_remove() {
    local ident ids lname lid remote name
    compose stop runner >/dev/null 2>&1 || true
    ident="$(local_identity)"
    if [ -z "$ident" ]; then
        name="$(runner_name)"
        if remote_name_exists "$name"; then
            die "no local registration here, so nothing was deleted. GitHub still lists a runner named $name;
  if it is really this machine's, a repository admin can delete it under Settings > Actions > Runners."
        fi
        say "nothing to remove"
        return 0
    fi
    check_local_daemon
    ids="$(checked_local "$ident")"
    read -r lname lid <<<"$ids"
    remote="$(remote_by_id "$lid")"
    if [ -z "$remote" ]; then
        compose run -T --rm --no-deps runner forget </dev/null
        say "GitHub had no runner id $lid; forgot the local registration"
    elif [ "$remote" = "$lname true" ]; then
        gh_api -X POST "repos/$REPO/actions/runners/remove-token" --jq .token \
            | compose run -T --rm runner remove
    else
        die "GitHub runner id $lid is not $lname with label $LABEL; deleting nothing"
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
