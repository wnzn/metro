#!/usr/bin/env bash
# Native Linux screenshots of a room's transcript (names, pictures, "you",
# the agent, local times and day lines) and Settings › Profile, in two runs:
#
# 1. Fixture: the real app views on NSH's Linux host against the test fake
#    server and profile fixture (src/features/profile/test/fixtures.ts), with
#    fixed times, an agent reply and an error. No server, no network. The test
#    network carries only text, so pictures show as initials:
#    chat-transcript-fixture.png.
# 2. Live: the same views against a real metro-server on loopback (dev users,
#    throwaway database). Messages are posted and drawn fixture PNGs uploaded
#    with curl; no model or vendor is called: profile-settings.png and
#    chat-transcript.png.
#
# Everything is throwaway and removed on exit: a temp directory with the
# database, logs, the dev tokens (mode-600 files, never printed or in argv)
# and a disposable copy of the app, which alone holds the filled-in capture
# test. The output is scanned for the tokens before the script succeeds.
#
#   docker build -t metro-desktop-shots:trixie apps/desktop/dev/screenshots
#   NSH_DIST=… apps/desktop/dev/screenshots/capture-profile.sh [OUT_DIR]   # default: docs/screenshots
#
# Env: NSH_DIST (default: from `nsh` on PATH), METRO_BIN (default: target/debug).
set -euo pipefail

here="$(cd "$(dirname "$0")" && pwd)"
app="$(cd "$here/../.." && pwd)"
repo="$(cd "$app/../.." && pwd)"
out_arg="${1:-$repo/docs/screenshots}"
mkdir -p "$out_arg"
out="$(cd "$out_arg" && pwd)"
bin="${METRO_BIN:-$repo/target/debug}"
image="metro-desktop-shots:trixie"

if [ -z "${NSH_DIST:-}" ]; then
  nsh_path="$(readlink -f "$(command -v nsh)")" || { echo "nsh not on PATH; set NSH_DIST" >&2; exit 1; }
  NSH_DIST="$(dirname "$(dirname "$nsh_path")")"
fi
[ -x "$NSH_DIST/bin/nsh" ] && [ -x "$NSH_DIST/runtime/linux/nsh-linux-host" ] || { echo "no NSH Linux distribution at $NSH_DIST" >&2; exit 1; }
[ -x "$bin/metro-server" ] || { echo "missing $bin/metro-server (cargo build)" >&2; exit 1; }
docker image inspect "$image" >/dev/null 2>&1 || { echo "build the image first: docker build -t $image $here" >&2; exit 1; }
for tool in openssl curl python3 rsync; do command -v "$tool" >/dev/null || { echo "$tool is required" >&2; exit 1; }; done

umask 077
tmp="$(mktemp -d "${TMPDIR:-/tmp}/metro-profile-shots-XXXXXX")"
sec="$tmp/secrets"; mkdir -m 700 "$sec"
pids=()
cleanup() {
  for p in "${pids[@]:-}"; do [ -n "$p" ] && kill "$p" 2>/dev/null || true; done
  wait 2>/dev/null || true
  rm -rf "$tmp"
}
trap cleanup EXIT INT TERM

copy="$tmp/app"
rsync -a --exclude .nsh --exclude target --exclude node_modules "$app/" "$copy/"
shots_test="src/features/profile/test/shots.test.tsx"

run_native() { # run_native <screen> ; runs $copy/$shots_test on the Linux host
  docker run --rm -i --init --user "$(id -u):$(id -g)" -e HOME=/tmp -e SCREEN="$1" --network host \
    -v "$NSH_DIST":/opt/nsh:ro -v "$copy":/app -v "$out":/shots -w /app "$image" \
    sh /app/dev/screenshots/with-display.sh /opt/nsh/bin/nsh test "$shots_test" --target linux </dev/null
}

# --- 1. fixture ---------------------------------------------------------------
cp "$here/profile-shots.tsx.in" "$copy/$shots_test"
run_native 1280x1000x24

# --- 2. live: a real server on loopback ----------------------------------------
openssl rand -hex 24 >"$sec/alice"
openssl rand -hex 24 >"$sec/bob"
for u in alice bob; do printf 'Authorization: Bearer %s\n' "$(cat "$sec/$u")" >"$sec/$u.hdr"; done
python3 - "$sec" >"$sec/dev-users.json" <<'PY'
import json, sys, pathlib
d = pathlib.Path(sys.argv[1])
print(json.dumps({u: (d / u).read_text().strip() for u in ("alice", "bob")}))
PY
port="$(python3 -c 'import socket; s=socket.socket(); s.bind(("127.0.0.1",0)); print(s.getsockname()[1])')"
url="http://127.0.0.1:$port"
env -i PATH=/usr/bin:/bin HOME="$tmp" METRO_BIND="127.0.0.1:$port" METRO_DB="$tmp/metro.sqlite" \
  METRO_DEV_USERS="$(cat "$sec/dev-users.json")" \
  "$bin/metro-server" >"$tmp/server.log" 2>&1 &
pids+=($!)
for _ in $(seq 150); do curl -fs -o /dev/null "$url/healthz" && break; sleep 0.1; done
curl -fs -o /dev/null "$url/healthz" || { echo "server did not start" >&2; tail -20 "$tmp/server.log" >&2; exit 1; }

api() { # api <user> <method> <path> [json]
  if [ -n "${4:-}" ]; then curl -fsS -X "$2" -H @"$sec/$1.hdr" -H 'Content-Type: application/json' --data-binary "$4" "$url$3"
  else curl -fsS -X "$2" -H @"$sec/$1.hdr" "$url$3"; fi
}
field() { python3 -c 'import json,sys; v=json.load(sys.stdin)
for k in sys.argv[1].split("."): v=v[k]
print(v)' "$1"; }

# Drawn portraits (fixtures, not photos).
python3 - "$tmp" <<'PY'
import math, struct, sys, zlib
def png(w, h, f):
    raw = b"".join(b"\x00" + b"".join(bytes(f(x, y)) for x in range(w)) for y in range(h))
    chunk = lambda t, d: struct.pack(">I", len(d)) + t + d + struct.pack(">I", zlib.crc32(t + d) & 0xffffffff)
    return b"\x89PNG\r\n\x1a\n" + chunk(b"IHDR", struct.pack(">IIBBBBB", w, h, 8, 2, 0, 0, 0)) + chunk(b"IDAT", zlib.compress(raw, 9)) + chunk(b"IEND", b"")
def alice(x, y):
    cx, cy = x - 64, y - 64
    if math.hypot(cx, cy + 16) < 24: return (241, 200, 160)
    if math.hypot(cx, cy + 24) < 36 and y < 52: return (70, 40, 30)
    if math.hypot(cx, cy - 60) < 52 and y > 80: return (200, 70, 90)
    return (int(230 - y * 0.6), int(180 - x * 0.3), int(120 + y * 0.5))
def bob(x, y):
    cx, cy = x - 64, y - 64
    if math.hypot(cx, cy + 12) < 24: return (250, 214, 165)
    if math.hypot(cx, cy - 58) < 52 and y > 84: return (40, 120, 200)
    return (int(90 + x * 0.8), int(60 + y * 0.8), 170)
for name, f in (("alice", alice), ("bob", bob)):
    open(f"{sys.argv[1]}/{name}.png", "wb").write(png(128, 128, f))
PY

api alice PATCH /v1/me/profile '{"display_name":"Alice Moreno","bio":"Release manager for Acme. Ask me about rollouts."}' >/dev/null
api bob PATCH /v1/me/profile '{"display_name":"Bob Tan"}' >/dev/null
for u in alice bob; do
  curl -fsS -X PUT -H @"$sec/$u.hdr" -H 'Content-Type: image/png' --data-binary @"$tmp/$u.png" "$url/v1/me/profile/avatar" >/dev/null
done
sid="$(api alice POST /v1/spaces '{"name":"Acme"}' | field id)"
api alice POST "/v1/spaces/$sid/members" '{"user_id":"bob"}' >/dev/null
say() { api "$1" POST "/v1/spaces/$sid/rooms/lobby/messages" "$2" >/dev/null; }
say bob '{"text":"Staging deploy finished, all checks green.","invoke_agent":false}'
say bob '{"text":"I will leave prod for the morning.","invoke_agent":false}'
say alice '{"text":"Thanks Bob. Starting the prod rollout now.","invoke_agent":false,"mentions":[{"kind":"user","id":"bob"}]}'
say alice '{"text":"Tag is v2.4.0; changelog after.","invoke_agent":false}'
say bob '{"text":"@Alice Moreno can you check the rollout dashboard?","invoke_agent":false,"mentions":[{"kind":"user","id":"alice"}]}'

python3 - "$here/profile-live-shots.tsx.in" "$copy/$shots_test" "$url" "$sec/alice" "$sid" <<'PY'
import json, sys
src, dst, url, token_file, sid = sys.argv[1:]
token = open(token_file).read().strip()
s = open(src).read()
for k, v in (("__URL__", url), ("__TOKEN__", token), ("__SPACE__", sid)):
    s = s.replace(f'"{k}"', json.dumps(v))
open(dst, "w").write(s)
PY
run_native 1280x1000x24

# The dev tokens only ever lived in $tmp; make sure none reached the output.
python3 - "$sec" "$out" <<'PY' || { echo "a token was found in the output; refusing to keep it" >&2; exit 1; }
import pathlib, sys
sec, out = map(pathlib.Path, sys.argv[1:])
secrets = [(sec / n).read_text().strip().encode() for n in ("alice", "bob")]
for p in out.rglob("*"):
    if p.is_file() and any(s in p.read_bytes() for s in secrets):
        sys.exit(1)
PY
echo "== wrote:"; ls -l "$out"/chat-transcript-fixture.png "$out"/profile-settings.png "$out"/chat-transcript.png
