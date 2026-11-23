#!/usr/bin/env bash
# Runner job-started hook: the job starts from an empty HOME, /tmp and tool
# cache. It runs after the runner has fetched the job's actions, so
# _work/_actions and the checkout folders stay.
set -euo pipefail
root=""
if [ -n "${METRO_RUNNER_TEST_ROOT:-}" ]; then root="$METRO_RUNNER_TEST_ROOT"; fi  # unit tests only
for dir in "$root/home/runner" "$root/tmp" "$root/runner/actions-runner/_work/_tool"; do
    if [ -d "$dir" ]; then find "$dir" -mindepth 1 -maxdepth 1 -exec rm -rf -- {} +; fi
done
