#!/bin/sh
set -eu

: "${METRO_SERVER_URL:?set the Metro service URL}"
: "${METRO_WORKSPACE:?set the runner workspace directory}"
: "${METRO_RUNNER_TOKEN_FILE:?set the path to your mode-600 runner token file}"
if ! [ -f "$METRO_RUNNER_TOKEN_FILE" ] || [ -L "$METRO_RUNNER_TOKEN_FILE" ] ||
    [ "$(stat -c '%u:%a' -- "$METRO_RUNNER_TOKEN_FILE")" != "$(id -u):600" ]; then
    printf '%s\n' 'Runner token file must be a regular mode-600 file owned by the runner user.' >&2
    exit 1
fi
METRO_RUNNER_TOKEN=$(cat -- "$METRO_RUNNER_TOKEN_FILE")
export METRO_RUNNER_TOKEN
unset METRO_RUNNER_TOKEN_FILE
exec "$(dirname -- "$0")/metro-runner"
