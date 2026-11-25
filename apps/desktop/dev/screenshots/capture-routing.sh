#!/usr/bin/env bash
# Native Linux screenshots of Model routing, connection sharing and a routed
# turn, against the real metro-server on loopback with local fixture vendors
# (typed_fixtures.py: System One, embeddings and two chat endpoints from
# tests/e2e/intelligence.py). Nothing here reaches a real vendor.
#
# Everything is throwaway and removed on exit, on failure too: a temp
# directory with the database, logs, the secrets (mode-600 files: two dev
# users' tokens, the provider master key and the fixtures' keys) and a
# disposable copy of the app. The app runs from that copy, so the step file
# `nsh test App.tsx` writes (it holds the typed token) never lands in the
# checkout. Tokens and keys are read from files only: never printed, never in
# argv, never on screen when a screenshot is taken.
#
#   docker build -t metro-desktop-shots:trixie apps/desktop/dev/screenshots
#   apps/desktop/dev/screenshots/capture-routing.sh [OUT_DIR]   # default: docs/screenshots
#
# Env: NSH_DIST (default: from `nsh` on PATH), METRO_BIN (default: target/debug).
# Writes model-routing.png, model-routing-dataflow.png, model-sharing.png and
# route-attribution.png into OUT_DIR.
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
for tool in node openssl curl python3 rsync; do command -v "$tool" >/dev/null || { echo "$tool is required" >&2; exit 1; }; done

umask 077
tmp="$(mktemp -d "${TMPDIR:-/tmp}/metro-routing-shots-XXXXXX")"
sec="$tmp/secrets"; mkdir -m 700 "$sec"
pids=()
cleanup() {
  for p in "${pids[@]:-}"; do [ -n "$p" ] && kill "$p" 2>/dev/null || true; done
  wait 2>/dev/null || true
  rm -rf "$tmp"
}
trap cleanup EXIT INT TERM

# --- a disposable copy of the app (its .nsh/test step files die with it) ----
copy="$tmp/app"
rsync -a --exclude .nsh --exclude target --exclude node_modules "$app/" "$copy/"

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

# --- loopback fixture vendors -----------------------------------------------
python3 "$here/typed_fixtures.py" "$sec/fixtures.json" >"$tmp/fixtures.log" 2>&1 &
pids+=($!)
for _ in $(seq 100); do [ -s "$sec/fixtures.json" ] && break; sleep 0.1; done
[ -s "$sec/fixtures.json" ] || { echo "fixtures did not start" >&2; cat "$tmp/fixtures.log" >&2; exit 1; }
fx() { python3 -c 'import json,sys; print(json.load(open(sys.argv[1]))[sys.argv[2]][sys.argv[3]])' "$sec/fixtures.json" "$1" "$2"; }
allowed="$(python3 -c 'import json,sys; d=json.load(open(sys.argv[1])); print(json.dumps([v["base_url"] for v in d.values()]))' "$sec/fixtures.json")"

# --- server --------------------------------------------------------------------
port="$(python3 -c 'import socket; s=socket.socket(); s.bind(("127.0.0.1",0)); print(s.getsockname()[1])')"
url="http://127.0.0.1:$port"
env -i PATH=/usr/bin:/bin HOME="$tmp" \
  METRO_BIND="127.0.0.1:$port" METRO_DB="$tmp/metro.sqlite" \
  METRO_DEV_USERS="$(cat "$sec/dev-users.json")" METRO_ORG_ADMINS='["alice"]' \
  METRO_PROVIDER_KEY_FILE="$sec/provider.key" METRO_PROVIDER_ALLOWED_BASE_URLS="$allowed" \
  "$bin/metro-server" >"$tmp/server.log" 2>&1 &
pids+=($!)
for _ in $(seq 150); do curl -fs -o /dev/null "$url/healthz" && break; sleep 0.1; done
curl -fs -o /dev/null "$url/healthz" || { echo "server did not start" >&2; tail -20 "$tmp/server.log" >&2; exit 1; }

api() { # api <user> <method> <path> [json | -] — prints the response body; "-" reads the body from stdin
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
# connect <user> <provider_id> <fixture> <name> [is_default] — the key goes from the fixture file to curl's stdin.
connect() {
  python3 - "$sec/fixtures.json" "$2" "$3" "$4" "${5:-}" <<'PY' | api "$1" POST /v1/providers - | field id
import json, sys
f, provider, fixture, name, default = sys.argv[1:]
v = json.load(open(f))[fixture]
body = {"name": name, "provider_id": provider, "base_url": v["base_url"], "api_key": v["key"], "model": v["model"]}
if default:
    body["is_default"] = default == "true"
print(json.dumps(body))
PY
}

# --- a Space, and the connections of Alice and Bob ------------------------------
sid="$(api alice POST /v1/spaces '{"name":"Acme"}' | field id)"
api alice POST "/v1/spaces/$sid/members" '{"user_id":"bob"}' >/dev/null
local_qwen="$(connect alice custom local "Local Qwen (self-hosted)")"
team="$(connect alice custom team "Team endpoint" true)"
s1="$(connect alice custom-systemone systemone "System One (self-hosted)")"
emb="$(connect alice custom-embeddings embeddings "Self-hosted embeddings")"
bob_gpu="$(connect bob custom local "Bob's GPU box")"
api alice PUT "/v1/providers/$local_qwen/sharing" "{\"visibility\":\"spaces\",\"space_ids\":[\"$sid\"]}" >/dev/null
api bob PUT "/v1/providers/$bob_gpu/sharing" "{\"visibility\":\"spaces\",\"space_ids\":[\"$sid\"]}" >/dev/null
api alice PUT /v1/model-routing "$(cat <<JSON
{"enabled":true,"infer_addressing":true,"classifier_id":"$s1","embedding_id":"$emb","default_chat_id":null,
 "min_directedness":0.85,"min_route_probability":0.65,
 "cases":[{"key":"incident_review","label":"Incident review","description":"Analyze the causal chain of an incident.","connection_id":"$bob_gpu"},
          {"key":"quick_question","label":"Quick question","description":"A short factual question with a one-line answer.","connection_id":"$team"}]}
JSON
)" >/dev/null
# Classification is opted into per Space: without it an unaddressed message stays conversation and no turn is routed.
api alice PUT "/v1/spaces/$sid/prefs" '{"classify_unaddressed":true}' >/dev/null

# --- the app, natively, in the container, from the disposable copy -------------
TOKEN_FILE="$sec/alice" URL="$url" SPACE="$sid" SHARED="$local_qwen" SHOTS=/shots node "$here/routing-steps.mjs" alice |
  docker run --rm -i --init --network host --user "$(id -u):$(id -g)" -e HOME=/tmp -e SCREEN=1280x1080x24 \
    -e DISPLAY_NUM=$((200 + RANDOM % 600)) \
    -v "$NSH_DIST":/opt/nsh:ro -v "$copy":/app -v "$out":/shots -w /app "$image" \
    sh /app/dev/screenshots/with-display.sh /opt/nsh/bin/nsh test App.tsx --target linux

# The routed turn really went through the fixtures: a System One call and Bob's shared chat endpoint.
grep -q "fixture handler errors" "$tmp/fixtures.log" && { echo "a fixture rejected a request" >&2; cat "$tmp/fixtures.log" >&2; exit 1; }

# Tokens and keys only ever lived in $sec; make sure none reached the output or the checkout.
python3 - "$sec" "$out" "$app" <<'PY' || { echo "a secret was found in the output or the checkout; refusing to keep it" >&2; exit 1; }
import json, pathlib, sys
sec, *roots = map(pathlib.Path, sys.argv[1:])
secrets = [(sec / n).read_text().strip() for n in ("alice", "bob", "provider.key")]
secrets += [v["key"] for v in json.loads((sec / "fixtures.json").read_text()).values()]
for root in roots:
    for p in root.rglob("*"):
        if p.is_file() and p.stat().st_size < 50_000_000:
            data = p.read_bytes()
            if any(s.encode() in data for s in secrets):
                sys.exit(1)
PY
echo "== wrote:"; ls -l "$out"/model-routing.png "$out"/model-routing-dataflow.png "$out"/model-sharing.png "$out"/route-attribution.png
echo "note: System One, embeddings and chat answers come from loopback fixtures (tests/e2e/intelligence.py); the server and app are real."
