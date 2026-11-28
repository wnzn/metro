#!/bin/sh
# CI only (native macOS runners without a GUI session, such as a runner
# started by a LaunchDaemon): the checks of an installed Metro.app that need
# no window. release.yml runs it instead of smoke-macos-launch.sh when
# `launchctl managername` isn't Aqua, and fails the job if any check fails.
#
#   smoke-macos-headless.sh APP RELEASE_JSON OUT_DIR
#
# Checks: the packaged renderer is the one RELEASE.json names; every Mach-O
# is arm64; ad hoc signatures verify (codesign) and say so (codesign -dv);
# both Info.plist files lint and name the expected executables and bundle
# identifiers; the bundled metro-server starts on loopback, answers /healthz
# with 200 and stops on SIGTERM. The launcher and the UI aren't started (they
# open a window). Every failure names its check; nothing here prints process
# arguments, the environment or tokens. The server's log goes to OUT_DIR.
set -eu

fail() { echo "macOS headless check failed: $*" >&2; exit 1; }
[ $# -eq 3 ] || { echo "usage: $0 APP RELEASE_JSON OUT_DIR" >&2; exit 2; }
app=$1 manifest=$2 out=$3
contents="$app/Contents"
launcher="$contents/MacOS/Metro"
server_exe="$contents/MacOS/metro-server"
tui="$contents/MacOS/metro-tui"
helper="$contents/Helpers/MetroUI.app"
ui="$helper/Contents/MacOS/Metro"
mkdir -p "$out"

# Same preflight as smoke-macos-launch.sh.
digest() { shasum -a 256 "$1" | cut -c1-64; }
want_ui="$(python3 -I -c 'import json, sys; print(json.load(open(sys.argv[1]))["renderer"]["packaged_signed_sha256"])' "$manifest")" ||
  fail "preflight: RELEASE.json has no renderer.packaged_signed_sha256"
[ "$(digest "$ui")" = "$want_ui" ] || fail "preflight: the UI is not the packaged renderer named in RELEASE.json"
[ "$(digest "$launcher")" != "$want_ui" ] || fail "preflight: the launcher is the renderer"
[ "$(digest "$launcher")" != "$(digest "$server_exe")" ] || fail "preflight: the launcher is the server"

for exe in "$launcher" "$server_exe" "$tui" "$ui"; do
  [ "$(lipo -archs "$exe" 2>/dev/null)" = arm64 ] || fail "architecture: ${exe#"$app"/} is not arm64 only"
done

codesign --verify --deep --strict "$app" 2>/dev/null || fail "signature: Metro.app does not verify"
for code in "$app" "$helper" "$server_exe" "$tui"; do
  codesign --verify --strict "$code" 2>/dev/null || fail "signature: ${code#"$app"/} does not verify"
  codesign -dv "$code" 2>"$out/codesign.txt" || fail "signature: codesign -dv failed for ${code#"$app"/}"
  grep -qx 'Signature=adhoc' "$out/codesign.txt" || fail "signature: ${code#"$app"/} is not ad hoc signed"
done

plist() { plutil -extract "$2" raw "$1/Info.plist" 2>/dev/null; }
for bundle in "$contents" "$helper/Contents"; do
  plutil -lint "$bundle/Info.plist" >/dev/null || fail "Info.plist: ${bundle#"$app"/}/Info.plist does not lint"
done
[ "$(plist "$contents" CFBundleIdentifier)" = dev.metro.desktop ] || fail "Info.plist: outer bundle identifier"
[ "$(plist "$contents" CFBundleExecutable)" = Metro ] || fail "Info.plist: outer executable"
[ "$(plist "$helper/Contents" CFBundleIdentifier)" = dev.metro.desktop.ui ] || fail "Info.plist: inner bundle identifier"
[ "$(plist "$helper/Contents" CFBundleExecutable)" = Metro ] || fail "Info.plist: inner executable"

# The bundled server: loopback only, a random throwaway dev token that is
# never printed (the server refuses to start without an auth mode).
work="$(mktemp -d "${TMPDIR:-/tmp}/metro-headless.XXXXXX")"
port=18797
token="$(head -c 24 /dev/urandom | od -An -tx1 | tr -d ' \n')"
METRO_DEV_USERS="{\"ci\":\"$token\"}" METRO_BIND="127.0.0.1:$port" METRO_DB="$work/metro.sqlite3" \
  "$server_exe" >"$out/server.log" 2>&1 &
pid=$!
token=
trap 'kill -KILL "$pid" 2>/dev/null || true; rm -rf "$work"' EXIT
healthy=0
for _ in $(seq 1 30); do
  kill -0 "$pid" 2>/dev/null || fail "server: the bundled metro-server exited early (log kept in OUT_DIR)"
  if [ "$(curl --noproxy '*' --silent --output /dev/null --write-out '%{http_code}' --max-time 2 \
      "http://127.0.0.1:$port/healthz" || true)" = 200 ]; then
    healthy=1; break
  fi
  sleep 1
done
[ "$healthy" = 1 ] || fail "server: the bundled metro-server did not answer /healthz with 200 within 30 s"
kill -TERM "$pid"
for _ in $(seq 1 20); do kill -0 "$pid" 2>/dev/null || break; sleep 1; done
if kill -0 "$pid" 2>/dev/null; then fail "server: metro-server did not stop within 20 s of SIGTERM"; fi
echo "headless check passed: $app"
