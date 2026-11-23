#!/usr/bin/env bash
# Runner job-completed hook: deletes everything the job left behind (the whole
# work folder, including downloaded actions and the tool cache, plus HOME and
# /tmp), so the next job can't pick up files from this one.
set -euo pipefail
root=""
if [ -n "${METRO_RUNNER_TEST_ROOT:-}" ]; then root="$METRO_RUNNER_TEST_ROOT"; fi  # unit tests only
for dir in "$root/runner/actions-runner/_work" "$root/home/runner" "$root/tmp"; do
    if [ -d "$dir" ]; then find "$dir" -mindepth 1 -maxdepth 1 -exec rm -rf -- {} +; fi
done
