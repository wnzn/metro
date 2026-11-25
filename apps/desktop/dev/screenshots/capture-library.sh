#!/usr/bin/env bash
# Native Linux screenshots of Search › Documents & media against the real
# metro-server on loopback, with a SCRIPTED media-embedding fixture
# (library_fixture.py, the scripted vendor of tests/e2e/library.py). No real
# model, no weights, no vendor: the fixture "embeds" by reading the bytes it
# is sent (PNG pixels, PDF page text, WAV loudness, text paragraphs).
#
# The files are uploaded as raw bytes through the real upload API by this
# script (curl), because a native run can't drive the system's file picker;
# the app's picker path is covered by the headless feature tests. Choosing the
# media model, indexing each file, searching and opening matches happen in
# the app. Bob's run shows a Space being revoked while he has its file open.
#
# Everything is throwaway and removed on exit, on failure too: a temp
# directory with the database, logs, a snapshot of the binaries, the secrets
# (mode-600 files: two dev users' tokens, the provider master key, the
# fixture's key) and a disposable copy of the app, so the step file
# `nsh test App.tsx` writes (it holds the typed token) never lands in the
# checkout. Tokens and keys are read from files only.
#
#   docker build -t metro-desktop-shots:trixie apps/desktop/dev/screenshots
#   docker build -t metro-desktop-shots:trixie-media -f apps/desktop/dev/screenshots/Dockerfile.media apps/desktop/dev/screenshots
#   apps/desktop/dev/screenshots/capture-library.sh [OUT_DIR]   # default: docs/screenshots
#
# Env: NSH_DIST (default: from `nsh` on PATH), METRO_BIN (default: target/debug).
# Writes library-assets.png, library-search.png, library-open-page.png,
# library-open-image.png, library-revocation-before.png and
# library-revocation-after.png into OUT_DIR. (A recording's player isn't
# captured: in the container its preview folder isn't private, so it won't play.)
set -euo pipefail

here="$(cd "$(dirname "$0")" && pwd)"
app="$(cd "$here/../.." && pwd)"
repo="$(cd "$app/../.." && pwd)"
out_arg="${1:-$repo/docs/screenshots}"
mkdir -p "$out_arg"
out="$(cd "$out_arg" && pwd)"
bin="${METRO_BIN:-$repo/target/debug}"
image="metro-desktop-shots:trixie-media"
if [ -z "${NSH_DIST:-}" ]; then
  nsh_path="$(readlink -f "$(command -v nsh)")" || { echo "nsh not on PATH; set NSH_DIST" >&2; exit 1; }
  NSH_DIST="$(dirname "$(dirname "$nsh_path")")"
fi
[ -x "$NSH_DIST/bin/nsh" ] && [ -x "$NSH_DIST/runtime/linux/nsh-linux-host" ] || { echo "no NSH Linux distribution at $NSH_DIST" >&2; exit 1; }
[ -x "$bin/metro-server" ] || { echo "missing $bin/metro-server (cargo build)" >&2; exit 1; }
docker image inspect "$image" >/dev/null 2>&1 || { echo "build the image first (see the header of $0)" >&2; exit 1; }
for tool in node openssl curl python3 rsync sha256sum; do command -v "$tool" >/dev/null || { echo "$tool is required" >&2; exit 1; }; done

umask 077
tmp="$(mktemp -d "${TMPDIR:-/tmp}/metro-library-shots-XXXXXX")"
sec="$tmp/secrets"; mkdir -m 700 "$sec"
pids=()
cleanup() {
  for p in "${pids[@]:-}"; do [ -n "$p" ] && kill "$p" 2>/dev/null || true; done
  wait 2>/dev/null || true
  rm -rf "$tmp"
}
trap cleanup EXIT INT TERM

# --- a snapshot of the server binary (other builds may replace target/ mid-run)
mkdir "$tmp/bin"; cp "$bin/metro-server" "$tmp/bin/"
echo "== metro-server sha256 $(sha256sum "$tmp/bin/metro-server" | cut -d' ' -f1)"

# --- a disposable copy of the app (its .nsh/test step files die with it) ----
copy="$tmp/app"
rsync -a --exclude .nsh --exclude target --exclude node_modules "$app/" "$copy/"
# `nsh test` refuses file writes to an app that declares no capabilities,
# though the packaged app (no capabilities: everything allowed) may write its
# data directory. NSH 0.6.1's Linux player plays only `file:` sources (not
# `blob:` or `data:`), so the viewer falls back to a private preview file
# there. The copy declares, explicitly, the grants the app already has when
# packaged: its own data directory, ~/Downloads for "Save a copy", the
# network, the picker, the clipboard, the status bar and the home directory.
# Only the copy's manifest changes; the code is the checkout's.
python3 - "$copy/nsh.json" <<'PY'
import json, sys
p = sys.argv[1]
m = json.load(open(p))
m["capabilities"] = {
    "network": {"hosts": ["*"]},
    "fs": {"read": ["${dataDir}", "${homeDir}/Downloads"], "write": ["${dataDir}", "${homeDir}/Downloads"]},
    "picker": True, "clipboard": True, "statusBar": True, "homeDir": True, "audio": True,
}
json.dump(m, open(p, "w"), indent=2)
PY

# --- secrets (files only) ----------------------------------------------------
openssl rand -hex 24 >"$sec/alice"
openssl rand -hex 24 >"$sec/bob"
for u in alice bob; do printf 'Authorization: Bearer %s\n' "$(cat "$sec/$u")" >"$sec/$u.hdr"; done
python3 - "$sec" >"$sec/dev-users.json" <<'PY'
import json, sys, pathlib
d = pathlib.Path(sys.argv[1])
print(json.dumps({u: (d / u).read_text().strip() for u in ("alice", "bob")}))
PY
openssl rand -base64 32 >"$sec/provider.key"

# --- the scripted media vendor and the inert media files ----------------------
media="$tmp/media"
python3 "$here/library_fixture.py" "$sec/vendor.json" "$media" >"$tmp/fixture.log" 2>&1 &
pids+=($!)
for _ in $(seq 100); do [ -s "$sec/vendor.json" ] && break; sleep 0.1; done
[ -s "$sec/vendor.json" ] || { echo "fixture did not start" >&2; cat "$tmp/fixture.log" >&2; exit 1; }
allowed="$(python3 -c 'import json,sys; print(json.dumps([json.load(open(sys.argv[1]))["base_url"]]))' "$sec/vendor.json")"

# --- server --------------------------------------------------------------------
port="$(python3 -c 'import socket; s=socket.socket(); s.bind(("127.0.0.1",0)); print(s.getsockname()[1])')"
url="http://127.0.0.1:$port"
env -i PATH=/usr/bin:/bin HOME="$tmp" \
  METRO_BIND="127.0.0.1:$port" METRO_DB="$tmp/metro.sqlite" \
  METRO_DEV_USERS="$(cat "$sec/dev-users.json")" METRO_ORG_ADMINS='["alice"]' \
  METRO_PROVIDER_KEY_FILE="$sec/provider.key" METRO_PROVIDER_ALLOWED_BASE_URLS="$allowed" \
  "$tmp/bin/metro-server" >"$tmp/server.log" 2>&1 &
pids+=($!)
for _ in $(seq 150); do curl -fs -o /dev/null "$url/healthz" && break; sleep 0.1; done
curl -fs -o /dev/null "$url/healthz" || { echo "server did not start" >&2; tail -20 "$tmp/server.log" >&2; exit 1; }

api() { # api <user> <method> <path> [json | -]
  local user="$1" method="$2" path="$3" body="${4:-}"
  if [ "$body" = - ]; then
    curl -fsS -X "$method" -H @"$sec/$user.hdr" -H 'Content-Type: application/json' --data-binary @- "$url$path"
  elif [ -n "$body" ]; then
    curl -fsS -X "$method" -H @"$sec/$user.hdr" -H 'Content-Type: application/json' --data-binary "$body" "$url$path"
  else
    curl -fsS -X "$method" -H @"$sec/$user.hdr" "$url$path"
  fi
}
field() { node -e 'let s="";process.stdin.on("data",d=>s+=d).on("end",()=>{let v=JSON.parse(s);for(const k of process.argv[1].split("."))v=v[k];process.stdout.write(String(v))})' "$1"; }
# upload <user> <file> <mime> <scope query> — raw bytes, the file's own type; prints the asset id
upload() {
  local name; name="$(basename "$2")"
  curl -fsS -X POST -H @"$sec/$1.hdr" -H "Content-Type: $3" --data-binary @"$2" \
    "$url/v1/library/assets?$4&name=$(python3 -c 'import sys,urllib.parse; print(urllib.parse.quote(sys.argv[1], safe=""))' "$name")" | field id
}

# --- a Space with Bob, a media connection, and the uploads ---------------------
sid="$(api alice POST /v1/spaces '{"name":"Acme"}' | field id)"
api alice POST "/v1/spaces/$sid/members" '{"user_id":"bob"}' >/dev/null
# The vendor's key goes from its file to curl's stdin, never argv.
conn="$(python3 - "$sec/vendor.json" <<'PY' | api alice POST /v1/providers - | field id
import json, sys
v = json.load(open(sys.argv[1]))
print(json.dumps({"name": "Capture EG2 fixture", "provider_id": "embeddinggemma-2", "base_url": v["base_url"], "api_key": v["key"], "model": v["model"]}))
PY
)"
printf 'Bob keeps his own to-do list here.\n\nCall the harbor office on Monday.\n' >"$media/bob-todo.txt"
declare -A ids
ids[whiteboard-crimson.png]="$(upload alice "$media/whiteboard-crimson.png" image/png scope=personal)"
ids[sky-azure.png]="$(upload alice "$media/sky-azure.png" image/png scope=personal)"
ids[standup-recording.wav]="$(upload alice "$media/standup-recording.wav" audio/wav scope=personal)"
ids[shipping-contract.pdf]="$(upload alice "$media/shipping-contract.pdf" application/pdf "scope=space&space_id=$sid")"
ids[harbor-notes.txt]="$(upload alice "$media/harbor-notes.txt" text/plain "scope=space&space_id=$sid")"
bob_todo="$(upload bob "$media/bob-todo.txt" text/plain scope=personal)"
alice_ids="$(for k in "${!ids[@]}"; do printf '%s\n%s\n' "$k" "${ids[$k]}"; done | python3 -c 'import sys,json; l=sys.stdin.read().split("\n"); print(json.dumps(dict(zip(l[0::2], l[1::2]))))')"
bob_ids="$(python3 -c 'import sys,json; print(json.dumps({"harbor-notes.txt": sys.argv[1], "bob-todo.txt": sys.argv[2]}))' "${ids[harbor-notes.txt]}" "$bob_todo")"

run_app() { # run_app <user> <steps> <ids json>
  TOKEN_FILE="$sec/$1" URL="$url" IDS="$3" MEDIA="$conn" SHOTS=/shots node "$here/library-steps.mjs" "$2" |
    # A private, uncommon X display per run: --network host shares abstract X sockets.
    docker run --rm -i --init --network host --user "$(id -u):$(id -g)" -e HOME=/tmp -e SCREEN=1280x1080x24 \
      -e DISPLAY_NUM=$((200 + RANDOM % 600)) \
      -v "$NSH_DIST":/opt/nsh:ro -v "$copy":/app -v "$out":/shots -w /app "$image" \
      sh /app/dev/screenshots/with-display.sh /opt/nsh/bin/nsh test App.tsx --target linux
}

# --- Alice: choose the model, index, search, open a page and an image ---------
run_app alice alice-model "$alice_ids" # Settings › Model providers (each session gets 30 s)
run_app alice alice-index "$alice_ids"
run_app alice alice "$alice_ids"
run_app alice alice-open "$alice_ids"

# --- Bob: has Acme's file open when Alice removes him from Acme ----------------
rm -f "$out/library-revocation-before.png"
(
  for _ in $(seq 900); do [ -s "$out/library-revocation-before.png" ] && break; sleep 0.1; done
  sleep 1
  api alice DELETE "/v1/spaces/$sid/members" '{"user_id":"bob"}' >/dev/null
) &
pids+=($!)
run_app bob bob "$bob_ids"
# The server agrees: Bob can no longer read the Space's file, his own still reads.
code="$(curl -s -o /dev/null -w '%{http_code}' -H @"$sec/bob.hdr" "$url/v1/library/assets/${ids[harbor-notes.txt]}/content")"
[ "$code" = 404 ] || { echo "Bob still reads the Space's file after removal (HTTP $code)" >&2; exit 1; }
code="$(curl -s -o /dev/null -w '%{http_code}' -H @"$sec/bob.hdr" "$url/v1/library/assets/$bob_todo/content")"
[ "$code" = 200 ] || { echo "Bob's personal file stopped reading (HTTP $code)" >&2; exit 1; }

grep -q "fixture handler errors" "$tmp/fixture.log" && { echo "the media fixture rejected a request" >&2; cat "$tmp/fixture.log" >&2; exit 1; }

# Tokens and keys only ever lived in $sec; make sure none reached the output or the checkout.
python3 - "$sec" "$out" "$app" <<'PY' || { echo "a secret was found in the output or the checkout; refusing to keep it" >&2; exit 1; }
import json, pathlib, sys
sec, *roots = map(pathlib.Path, sys.argv[1:])
secrets = [(sec / n).read_text().strip() for n in ("alice", "bob", "provider.key")]
secrets.append(json.loads((sec / "vendor.json").read_text())["key"])
for root in roots:
    for p in root.rglob("*"):
        if p.is_file() and p.stat().st_size < 50_000_000:
            data = p.read_bytes()
            if any(s.encode() in data for s in secrets):
                sys.exit(1)
PY
echo "== wrote:"; ls -l "$out"/library-*.png
echo "note: media embeddings come from a scripted loopback fixture (tests/e2e/library.py's vendor); the server, uploads, index, search and the app are real."
