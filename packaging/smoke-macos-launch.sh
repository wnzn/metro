#!/bin/sh
# CI only (native macOS runners): a bounded start and shutdown of an installed
# Metro.app. Used for the ad hoc preview and for the signed, notarized release
# build (hardened runtime), so both run exactly the same check.
#
#   smoke-macos-launch.sh APP RELEASE_JSON OUT_DIR SHOT_NAME
#
# METRO_DESKTOP_HOME must point at a throwaway data folder. The launcher's log
# goes to OUT_DIR/launcher.log and is not printed. Every failure names its
# check; nothing here prints process arguments, the environment or tokens.
# shellcheck disable=SC3045 # ulimit -u: macOS sh is bash, which has it
set -eu

fail() { echo "macOS launch smoke failed: $*" >&2; exit 1; }
[ $# -eq 4 ] || { echo "usage: $0 APP RELEASE_JSON OUT_DIR SHOT_NAME" >&2; exit 2; }
app=$1 manifest=$2 out=$3 shot=$4
[ -n "${METRO_DESKTOP_HOME:-}" ] || { echo "METRO_DESKTOP_HOME must be set" >&2; exit 2; }
launcher="$app/Contents/MacOS/Metro"
server_exe="$app/Contents/MacOS/metro-server"
ui="$app/Contents/Helpers/MetroUI.app/Contents/MacOS/Metro"
mkdir -p "$out" "$METRO_DESKTOP_HOME"

# Preflight: the renderer is the packaged (pinned-runtime) one, and the
# launcher is neither the renderer nor the server.
digest() { shasum -a 256 "$1" | cut -c1-64; }
want_ui="$(python3 -I -c 'import json, sys; print(json.load(open(sys.argv[1]))["renderer"]["packaged_signed_sha256"])' "$manifest")" ||
  fail "preflight: RELEASE.json has no renderer.packaged_signed_sha256"
[ "$(digest "$ui")" = "$want_ui" ] || fail "preflight: the UI is not the packaged renderer named in RELEASE.json"
[ "$(digest "$launcher")" != "$want_ui" ] || fail "preflight: the launcher is the renderer"
[ "$(digest "$launcher")" != "$(digest "$server_exe")" ] || fail "preflight: the launcher is the server"

# Hard bound: RLIMIT_NPROC (per user on macOS) for the launcher and
# everything it starts, a little above what this user already runs.
used="$(ps -axo uid= | awk -v u="$(id -u)" '$1 == u' | wc -l | tr -d ' ')"
cap=$((used + 24))
( ulimit -u "$cap" && test "$(ulimit -u)" = "$cap" ) || fail "process limit: cannot enforce RLIMIT_NPROC $cap; not launching"

# Processes by exact executable path. Apple ps: comm is argv[0] (the launcher,
# guardian, UI and server all start from canonical absolute paths), args is
# argv joined by spaces, and output to a pipe is never truncated.
procs() { ps -axo pid=,ppid=,comm= | awk '{ pid = $1; ppid = $2; sub(/^ *[0-9]+ +[0-9]+ /, ""); print pid "\t" ppid "\t" $0 }'; }
children() { procs | awk -F '\t' -v p="$1" -v e="$2" '$2 == p && $3 == e { print $1 }'; }
in_bundle() { procs | awk -F '\t' -v pre="$app/Contents/" 'index($3, pre) == 1 { print $1 }'; }
count() { grep -c . || true; }
# Launcher, guardian, UI and server, plus one transient process.
budget() {
  n="$(in_bundle | count)"
  [ "$n" -le 5 ] || fail "process budget: $n processes from the bundle (at most 5)"
}
cleanup() { in_bundle | xargs kill -KILL 2>/dev/null || true; }
trap cleanup EXIT

( ulimit -u "$cap"; exec "$launcher" ) >"$out/launcher.log" 2>&1 &
pid=$!
healthy=0
for _ in $(seq 1 90); do
  budget
  kill -0 "$pid" 2>/dev/null || fail "start: the launcher exited early (log kept in OUT_DIR)"
  server="$(children "$pid" "$server_exe")"
  if [ "$(printf '%s\n' "$server" | count)" = 1 ]; then
    listen="$(lsof -nP -a -p "$server" -iTCP -sTCP:LISTEN -Fn 2>/dev/null | sed -n 's/^n//p' || true)"
    if printf '%s\n' "$listen" | grep . | grep -vqE '^(127\.0\.0\.1|\[::1\]):'; then
      fail "health: the bundled metro-server listens beyond loopback"
    fi
    port="$(printf '%s\n' "$listen" | sed -n 's/^127\.0\.0\.1://p' | head -n1)"
    if [ -n "$port" ] && [ "$(curl --noproxy '*' --silent --output /dev/null --write-out '%{http_code}' \
        --max-time 2 "http://127.0.0.1:$port/healthz" || true)" = 200 ]; then
      healthy=1; break
    fi
  fi
  sleep 1
done
[ "$healthy" = 1 ] || fail "health: no single metro-server under the launcher answered /healthz with 200 within 90 s"

# Exactly one guardian (the launcher's own executable, guardian argument
# only) under the launcher, and exactly one UI under that guardian.
guardian="$(children "$pid" "$launcher")"
[ "$(printf '%s\n' "$guardian" | count)" = 1 ] || fail "guardian: expected exactly one guardian under the launcher"
[ "$(ps -o args= -p "$guardian")" = "$launcher --metro-ui-guardian" ] || fail "guardian: unexpected command line"
uiproc=
for _ in $(seq 1 30); do
  budget
  uiproc="$(children "$guardian" "$ui")"
  [ -n "$uiproc" ] && break
  sleep 1
done
[ "$(printf '%s\n' "$uiproc" | count)" = 1 ] || fail "window: expected exactly one UI under the guardian within 30 s"
[ -z "$(children "$uiproc" "$launcher")" ] || fail "window: a launcher runs below the UI"
sleep 5
budget
screencapture -x "$out/$shot.png" || true

kill -TERM "$pid" || fail "shutdown: the launcher was gone before SIGTERM"
for _ in $(seq 1 20); do kill -0 "$pid" 2>/dev/null || break; sleep 1; done
if kill -0 "$pid" 2>/dev/null; then fail "shutdown: the launcher did not stop within 20 s of SIGTERM"; fi
sleep 3
[ -z "$(in_bundle)" ] || fail "shutdown: a guardian, UI or server outlived the launcher"
trap - EXIT
echo "launch smoke passed: $app"
