#!/bin/sh
# CI only (native macOS runners): a bounded start and shutdown of an installed
# Metro.app. Used for the ad hoc preview and for the signed, notarized release
# build (hardened runtime), so both run exactly the same check.
#
#   smoke-macos-launch.sh APP RELEASE_JSON OUT_DIR SHOT_NAME
#
# METRO_DESKTOP_HOME must point at a throwaway data folder. The launcher's log
# goes to OUT_DIR/launcher.log and is not printed.
# shellcheck disable=SC3045 # ulimit -u: macOS sh is bash, which has it
set -eu

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
want_ui="$(python3 -I -c 'import json, sys; print(json.load(open(sys.argv[1]))["renderer"]["packaged_signed_sha256"])' "$manifest")"
test "$(digest "$ui")" = "$want_ui"
test "$(digest "$launcher")" != "$want_ui"
test "$(digest "$launcher")" != "$(digest "$server_exe")"

# Hard bound: RLIMIT_NPROC (per user on macOS) for the launcher and
# everything it starts, a little above what this user already runs.
used="$(ps -axo uid= | awk -v u="$(id -u)" '$1 == u' | wc -l | tr -d ' ')"
cap=$((used + 24))
( ulimit -u "$cap" && test "$(ulimit -u)" = "$cap" ) || { echo "cannot enforce a process limit; not launching" >&2; exit 1; }

# Processes by exact executable path (ps comm is the full path on macOS).
procs() { ps -axo pid=,ppid=,comm= | awk '{ pid = $1; ppid = $2; sub(/^ *[0-9]+ +[0-9]+ /, ""); print pid "\t" ppid "\t" $0 }'; }
children() { procs | awk -F '\t' -v p="$1" -v e="$2" '$2 == p && $3 == e { print $1 }'; }
in_bundle() { procs | awk -F '\t' -v pre="$app/Contents/" 'index($3, pre) == 1 { print $1 }'; }
count() { grep -c . || true; }
# Launcher, guardian, UI and server, plus one transient process.
budget() {
  n="$(in_bundle | count)"
  [ "$n" -le 5 ] || { echo "process budget exceeded: $n processes from the bundle" >&2; exit 1; }
}
cleanup() { in_bundle | xargs kill -KILL 2>/dev/null || true; }
trap cleanup EXIT

( ulimit -u "$cap"; exec "$launcher" ) >"$out/launcher.log" 2>&1 &
pid=$!
healthy=0
for _ in $(seq 1 90); do
  budget
  kill -0 "$pid" 2>/dev/null || { echo "the launcher exited early" >&2; exit 1; }
  server="$(children "$pid" "$server_exe")"
  if [ "$(printf '%s\n' "$server" | count)" = 1 ]; then
    listen="$(lsof -nP -a -p "$server" -iTCP -sTCP:LISTEN -Fn 2>/dev/null | sed -n 's/^n//p' || true)"
    if printf '%s\n' "$listen" | grep . | grep -vqE '^(127\.0\.0\.1|\[::1\]):'; then
      echo "the bundled metro-server listens beyond loopback" >&2; exit 1
    fi
    port="$(printf '%s\n' "$listen" | sed -n 's/^127\.0\.0\.1://p' | head -n1)"
    if [ -n "$port" ] && [ "$(curl --noproxy '*' --silent --output /dev/null --write-out '%{http_code}' \
        --max-time 2 "http://127.0.0.1:$port/healthz" || true)" = 200 ]; then
      healthy=1; break
    fi
  fi
  sleep 1
done
[ "$healthy" = 1 ] || { echo "the bundled metro-server never answered /healthz with 200" >&2; exit 1; }

# Exactly one guardian (the launcher's own executable, guardian argument
# only) under the launcher, and exactly one UI under that guardian.
guardian="$(children "$pid" "$launcher")"
[ "$(printf '%s\n' "$guardian" | count)" = 1 ] || { echo "expected exactly one guardian under the launcher" >&2; exit 1; }
[ "$(ps -o args= -p "$guardian")" = "$launcher --metro-ui-guardian" ] || { echo "unexpected guardian command line" >&2; exit 1; }
uiproc=
for _ in $(seq 1 30); do
  budget
  uiproc="$(children "$guardian" "$ui")"
  [ -n "$uiproc" ] && break
  sleep 1
done
[ "$(printf '%s\n' "$uiproc" | count)" = 1 ] || { echo "expected exactly one UI under the guardian" >&2; exit 1; }
[ -z "$(children "$uiproc" "$launcher")" ] || { echo "a launcher runs below the UI" >&2; exit 1; }
sleep 5
budget
screencapture -x "$out/$shot.png" || true

kill -TERM "$pid"
for _ in $(seq 1 20); do kill -0 "$pid" 2>/dev/null || break; sleep 1; done
if kill -0 "$pid" 2>/dev/null; then echo "the launcher did not stop on SIGTERM" >&2; exit 1; fi
sleep 3
[ -z "$(in_bundle)" ] || { echo "a guardian, UI or server outlived the launcher" >&2; exit 1; }
trap - EXIT
echo "launch smoke passed: $app"
